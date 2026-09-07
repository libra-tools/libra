//! Tests push command negotiation and ref update flows against remotes.
//!
//! **Layer:** L1 (most tests). `test_push_invalid_remote` and `test_push_force_with_local_changes`
//! are L2 — require `LIBRA_TEST_GITHUB_TOKEN` or are `#[cfg(unix)]`.

#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::{env, fs, process::Command, time::Duration};

use clap::Parser;
use libra::{
    command::push,
    internal::{db::get_db_conn_instance, reflog::Reflog},
    utils::test::ChangeDirGuard,
};
#[cfg(unix)]
use serde_json::Value;
use serial_test::serial;
use tempfile::TempDir;
use tokio::{process::Command as TokioCommand, time::timeout};

use super::{create_committed_repo_via_cli, parse_cli_error_stderr, run_libra_command};

fn libra_command(cwd: &std::path::Path) -> Command {
    let home = cwd.join(".libra-test-home");
    let config_home = home.join(".config");
    fs::create_dir_all(&config_home).expect("failed to create isolated HOME");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
    cmd.current_dir(cwd)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", &home);
    cmd
}

fn libra_tokio_command(cwd: &std::path::Path) -> TokioCommand {
    let home = cwd.join(".libra-test-home");
    let config_home = home.join(".config");
    fs::create_dir_all(&config_home).expect("failed to create isolated HOME");

    let mut cmd = TokioCommand::new(env!("CARGO_BIN_EXE_libra"));
    cmd.current_dir(cwd)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", &home);
    cmd
}

/// Helper function: Initialize a temporary Libra repository
fn init_temp_repo() -> TempDir {
    let temp_dir = tempfile::tempdir().expect("Failed to create temporary directory");
    let temp_path = temp_dir.path();

    eprintln!("Temporary directory created at: {temp_path:?}");
    assert!(
        temp_path.is_dir(),
        "Temporary path is not a valid directory"
    );

    let output = libra_command(temp_path)
        .args(["init"])
        .output()
        .expect("Failed to execute libra binary");

    if !output.status.success() {
        panic!(
            "Failed to initialize libra repository: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    eprintln!("Initialized libra repo at: {temp_path:?}");
    temp_dir
}

#[cfg(unix)]
fn configure_local_identity(repo: &Path) {
    let name_out = libra_command(repo)
        .args(["config", "user.name", "Push Test User"])
        .output()
        .expect("failed to configure user.name");
    assert!(
        name_out.status.success(),
        "failed to configure user.name: {}",
        String::from_utf8_lossy(&name_out.stderr)
    );

    let email_out = libra_command(repo)
        .args(["config", "user.email", "push-test@example.com"])
        .output()
        .expect("failed to configure user.email");
    assert!(
        email_out.status.success(),
        "failed to configure user.email: {}",
        String::from_utf8_lossy(&email_out.stderr)
    );
}

#[cfg(unix)]
fn init_local_repo_with_commit(local_dir: &Path, file_name: &str, content: &str, message: &str) {
    fs::create_dir_all(local_dir).expect("failed to create local dir");
    let init_out = libra_command(local_dir)
        .args(["init"])
        .output()
        .expect("failed to init local libra repo");
    assert!(
        init_out.status.success(),
        "local init failed: {}",
        String::from_utf8_lossy(&init_out.stderr)
    );
    configure_local_identity(local_dir);

    fs::write(local_dir.join(file_name), content).expect("failed to write file");
    let add_out = libra_command(local_dir)
        .args(["add", file_name])
        .output()
        .expect("failed to add file");
    assert!(
        add_out.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&add_out.stderr)
    );
    let commit_out = libra_command(local_dir)
        .args(["commit", "-m", message])
        .output()
        .expect("failed to commit");
    assert!(
        commit_out.status.success(),
        "commit failed: {}",
        String::from_utf8_lossy(&commit_out.stderr)
    );
}

#[cfg(unix)]
fn current_branch_name(repo: &Path) -> String {
    String::from_utf8(
        libra_command(repo)
            .args(["branch", "--show-current"])
            .output()
            .expect("failed to read current branch")
            .stdout,
    )
    .expect("branch name not utf8")
    .trim()
    .to_string()
}

#[cfg(unix)]
fn add_fake_ssh_remote(repo: &Path, remote_dir: &Path) {
    let ssh_remote = format!("git@fakehost:{}", remote_dir.to_string_lossy());
    let remote_add_out = libra_command(repo)
        .args(["remote", "add", "origin", &ssh_remote])
        .output()
        .expect("failed to add ssh remote");
    assert!(
        remote_add_out.status.success(),
        "remote add failed: {}",
        String::from_utf8_lossy(&remote_add_out.stderr)
    );
}

#[cfg(unix)]
fn git_ref_exists(git_dir: &Path, refname: &str) -> bool {
    Command::new("git")
        .args([
            "--git-dir",
            git_dir.to_str().unwrap(),
            "show-ref",
            "--verify",
            refname,
        ])
        .output()
        .expect("failed to inspect git ref")
        .status
        .success()
}

#[test]
fn test_push_cli_without_remote_returns_fatal_128() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["push"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-003");
    assert!(stderr.contains("fatal: no configured push destination"));
    assert!(stderr.contains("Hint:"));
}

#[test]
fn test_push_porcelain_conflicts_with_json() {
    // `--porcelain` and a JSON mode are mutually exclusive; the check runs in
    // `execute_safe` before any remote lookup, so a committed repo (no remote)
    // is enough.
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(
        &["--json", "push", "--porcelain", "origin", "main"],
        repo.path(),
    );
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        report.message.contains("--porcelain"),
        "message: {}",
        report.message
    );
}

#[test]
fn test_push_porcelain_conflicts_with_machine() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(
        &["--machine", "push", "--porcelain", "origin", "main"],
        repo.path(),
    );
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(129), "stderr: {stderr}");
    assert_eq!(report.error_code, "LBR-CLI-002");
}

#[test]
fn test_push_help_lists_new_flags() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["push", "--help"], repo.path());
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for flag in [
        "--force-with-lease",
        "--force-if-includes",
        "--thin",
        "--no-thin",
        "--porcelain",
    ] {
        assert!(
            stdout.contains(flag),
            "push --help must expose `{flag}`; stdout: {stdout}"
        );
    }
}

#[cfg(unix)]
fn create_fake_ssh_script(root: &Path) -> PathBuf {
    let script_path = root.join("fake_ssh.sh");
    let script = r#"#!/bin/sh
set -eu

if [ -n "${LIBRA_TEST_SSH_LOG:-}" ]; then
  printf '%s\n' "$@" >> "$LIBRA_TEST_SSH_LOG"
  printf -- '---\n' >> "$LIBRA_TEST_SSH_LOG"
fi

if [ "${LIBRA_TEST_SSH_FAIL:-}" = "hostkey" ]; then
  echo "Host key verification failed." >&2
  exit 255
fi

remote_cmd=""
for arg in "$@"; do
  remote_cmd="$arg"
done

if [ -z "$remote_cmd" ]; then
  echo "missing remote command" >&2
  exit 2
fi

exec sh -c "$remote_cmd"
"#;
    fs::write(&script_path, script).expect("failed to write fake ssh script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script_path)
            .expect("failed to stat fake ssh script")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).expect("failed to chmod fake ssh script");
    }
    script_path
}

#[tokio::test]
#[serial(cwd)]
async fn test_push_force_flag_parsing() {
    let temp_repo = init_temp_repo();
    let temp_path = temp_repo.path();
    let _guard = ChangeDirGuard::new(temp_path);

    // Test that --force flag is correctly parsed
    let args = push::PushArgs::parse_from(["push", "--force", "origin", "main"]);
    assert!(args.force);

    // Test that -f flag is correctly parsed
    let args = push::PushArgs::parse_from(["push", "-f", "origin", "main"]);
    assert!(args.force);
}

#[tokio::test]
#[serial(cwd)]
async fn test_push_file_remote_fails_without_reflog() {
    // local file remotes are not supported; ensure we fail loudly and avoid reflog writes
    let remote_dir = tempfile::tempdir().unwrap();
    let remote_path = remote_dir.path();

    // local repo
    let local_dir = tempfile::tempdir().unwrap();
    let local_path = local_dir.path();
    let _guard = ChangeDirGuard::new(local_path);
    let out = libra_command(local_path)
        .args(["init"])
        .output()
        .expect("init");
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = libra_command(local_path)
        .args(["config", "user.name", "Push Test User"])
        .output()
        .expect("set user.name");
    assert!(
        out.status.success(),
        "set user.name failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = libra_command(local_path)
        .args(["config", "user.email", "push-test@example.com"])
        .output()
        .expect("set user.email");
    assert!(
        out.status.success(),
        "set user.email failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // add file + commit
    std::fs::write(local_path.join("file.txt"), "hello").unwrap();
    let out = libra_command(local_path)
        .args(["add", "file.txt"])
        .output()
        .expect("add");
    assert!(
        out.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = libra_command(local_path)
        .args(["commit", "-m", "init"])
        .output()
        .expect("commit");
    assert!(
        out.status.success(),
        "commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // add remote (local path, will be treated as file://)
    let out = libra_command(local_path)
        .args(["remote", "add", "origin", remote_path.to_str().unwrap()])
        .output()
        .expect("remote add");
    assert!(
        out.status.success(),
        "remote add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // push should fail with clear fatal message
    let out = libra_command(local_path)
        .args(["push", "origin", "main"])
        .output()
        .expect("push");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("pushing to local file repositories is not supported"),
        "stderr should mention unsupported file:// push, got: {stderr}"
    );

    // ensure no reflog entry is written
    let db = get_db_conn_instance().await;
    let entry = Reflog::find_one(&db, "refs/remotes/origin/master")
        .await
        .expect("query reflog");
    assert!(
        entry.is_none(),
        "reflog should not be created when push fails"
    );
}

#[tokio::test]
#[serial(cwd)]
/// Test pushing to an invalid remote repository with timeout
async fn test_push_invalid_remote() {
    if std::env::var("LIBRA_TEST_GITHUB_TOKEN").map_or(true, |v| v.is_empty()) {
        eprintln!("skipped (LIBRA_TEST_GITHUB_TOKEN not set)");
        return;
    }
    let temp_repo = init_temp_repo();
    let temp_path = temp_repo.path();
    let _guard = ChangeDirGuard::new(temp_path);

    eprintln!("Starting test: push to invalid remote");

    // Configure an invalid remote repository
    eprintln!("Adding invalid remote: https://invalid-url.example/repo.git");
    let remote_output = libra_tokio_command(temp_path)
        .args([
            "remote",
            "add",
            "origin",
            "https://invalid-url.example/repo.git",
        ])
        .output()
        .await
        .expect("Failed to add remote");

    assert!(
        remote_output.status.success(),
        "Failed to add remote: {}",
        String::from_utf8_lossy(&remote_output.stderr)
    );

    // Set upstream branch
    eprintln!("Setting upstream to origin/main");
    let branch_output = libra_tokio_command(temp_path)
        .args(["branch", "--set-upstream-to", "origin/main"])
        .output()
        .await
        .expect("Failed to set upstream branch");

    assert!(
        branch_output.status.success(),
        "Failed to set upstream: {}",
        String::from_utf8_lossy(&branch_output.stderr)
    );

    // Attempt to push with 15-second timeout to avoid hanging CI
    eprintln!("Attempting 'libra push' with 15s timeout...");
    let push_result = timeout(Duration::from_secs(15), async {
        libra_tokio_command(temp_path).arg("push").output().await
    })
    .await;

    match push_result {
        // Timeout occurred — this is expected for unreachable remotes
        Err(_) => {
            eprintln!("Push timed out after 15 seconds — expected for invalid remote");
        }
        // Command completed within timeout
        Ok(Ok(output)) => {
            eprintln!("Push completed (status: {:?})", output.status);
            // Push to invalid remote should fail
            assert!(
                !output.status.success(),
                "Push should fail when remote is unreachable"
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !stderr.trim().is_empty(),
                "Expected error message in stderr, but was empty"
            );

            eprintln!("Push failed as expected: {stderr}");
        }
        // Failed to start the command
        Ok(Err(e)) => {
            panic!("Failed to run 'libra push' command: {e}");
        }
    }

    eprintln!("test_push_invalid_remote passed");
}

#[cfg(unix)]
#[tokio::test]
#[serial(env)]
async fn test_push_force_with_local_changes() {
    let temp_root = tempfile::tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let local_dir = temp_root.path().join("local");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    // Create a bare remote repository
    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );

    // Set up local repo with initial commit and push
    fs::create_dir_all(&local_dir).expect("failed to create local dir");
    let init_out = libra_command(&local_dir)
        .args(["init"])
        .output()
        .expect("failed to init local libra repo");
    assert!(
        init_out.status.success(),
        "local init failed: {}",
        String::from_utf8_lossy(&init_out.stderr)
    );
    configure_local_identity(&local_dir);

    fs::write(local_dir.join("file.txt"), "initial content").expect("failed to write file");
    let add_out = libra_command(&local_dir)
        .args(["add", "file.txt"])
        .output()
        .expect("failed to add file");
    assert!(add_out.status.success());
    let commit_out = libra_command(&local_dir)
        .args(["commit", "-m", "initial commit"])
        .output()
        .expect("failed to commit");
    assert!(commit_out.status.success());

    let current_branch = String::from_utf8(
        libra_command(&local_dir)
            .args(["branch", "--show-current"])
            .output()
            .expect("failed to read current branch")
            .stdout,
    )
    .expect("branch name not utf8")
    .trim()
    .to_string();

    let ssh_remote = format!("git@fakehost:{}", remote_dir.to_string_lossy());
    let remote_add_out = libra_command(&local_dir)
        .args(["remote", "add", "origin", &ssh_remote])
        .output()
        .expect("failed to add ssh remote");
    assert!(remote_add_out.status.success());

    let push_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["push", "origin", &current_branch])
        .output()
        .expect("failed to push");
    assert!(
        push_out.status.success(),
        "initial push should succeed, stderr: {}",
        String::from_utf8_lossy(&push_out.stderr)
    );

    // Record the initial remote HEAD
    let initial_head = String::from_utf8(
        Command::new("git")
            .args([
                "--git-dir",
                remote_dir.to_str().unwrap(),
                "rev-parse",
                &format!("refs/heads/{current_branch}"),
            ])
            .output()
            .expect("failed to read remote head")
            .stdout,
    )
    .expect("hash not utf8")
    .trim()
    .to_string();

    // Amend the commit locally to create divergent history
    fs::write(local_dir.join("file.txt"), "force pushed content").expect("failed to overwrite");
    let add_out = libra_command(&local_dir)
        .args(["add", "file.txt"])
        .output()
        .expect("failed to add");
    assert!(add_out.status.success());
    let commit_out = libra_command(&local_dir)
        .args(["commit", "-m", "divergent commit"])
        .output()
        .expect("failed to commit");
    assert!(commit_out.status.success());

    // Force push should succeed and update the remote
    let force_push_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["push", "--force", "origin", &current_branch])
        .output()
        .expect("failed to force push");
    assert!(
        force_push_out.status.success(),
        "force push should succeed, stderr: {}",
        String::from_utf8_lossy(&force_push_out.stderr)
    );

    // Verify that the remote HEAD changed
    let final_head = String::from_utf8(
        Command::new("git")
            .args([
                "--git-dir",
                remote_dir.to_str().unwrap(),
                "rev-parse",
                &format!("refs/heads/{current_branch}"),
            ])
            .output()
            .expect("failed to read remote head")
            .stdout,
    )
    .expect("hash not utf8")
    .trim()
    .to_string();

    assert_ne!(
        initial_head, final_head,
        "force push should have updated the remote HEAD"
    );
}

#[cfg(unix)]
#[test]
#[serial(env)]
fn test_push_explicit_refspec_uses_destination_branch_name() {
    let temp_root = tempfile::tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let local_dir = temp_root.path().join("local");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );

    init_local_repo_with_commit(
        &local_dir,
        "tracked.txt",
        "initial content",
        "initial commit",
    );
    add_fake_ssh_remote(&local_dir, &remote_dir);

    let feature_out = libra_command(&local_dir)
        .args(["branch", "feature"])
        .output()
        .expect("failed to create feature branch");
    assert!(
        feature_out.status.success(),
        "feature branch creation failed: {}",
        String::from_utf8_lossy(&feature_out.stderr)
    );
    let release_out = libra_command(&local_dir)
        .args(["branch", "release"])
        .output()
        .expect("failed to create release branch");
    assert!(
        release_out.status.success(),
        "release branch creation failed: {}",
        String::from_utf8_lossy(&release_out.stderr)
    );

    let set_remote_out = libra_command(&local_dir)
        .args(["config", "branch.release.remote", "origin"])
        .output()
        .expect("failed to configure branch.release.remote");
    assert!(
        set_remote_out.status.success(),
        "config branch.release.remote failed: {}",
        String::from_utf8_lossy(&set_remote_out.stderr)
    );
    let set_merge_out = libra_command(&local_dir)
        .args(["config", "branch.release.merge", "refs/heads/stable"])
        .output()
        .expect("failed to configure branch.release.merge");
    assert!(
        set_merge_out.status.success(),
        "config branch.release.merge failed: {}",
        String::from_utf8_lossy(&set_merge_out.stderr)
    );

    let push_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["push", "origin", "feature:release"])
        .output()
        .expect("failed to push explicit refspec");
    assert!(
        push_out.status.success(),
        "explicit refspec push failed: {}",
        String::from_utf8_lossy(&push_out.stderr)
    );

    let release_ref_out = Command::new("git")
        .args([
            "--git-dir",
            remote_dir.to_str().unwrap(),
            "rev-parse",
            "refs/heads/release",
        ])
        .output()
        .expect("failed to read remote release ref");
    assert!(
        release_ref_out.status.success(),
        "remote release ref should exist, stderr: {}",
        String::from_utf8_lossy(&release_ref_out.stderr)
    );

    let stable_ref_out = Command::new("git")
        .args([
            "--git-dir",
            remote_dir.to_str().unwrap(),
            "rev-parse",
            "refs/heads/stable",
        ])
        .output()
        .expect("failed to read remote stable ref");
    assert!(
        !stable_ref_out.status.success(),
        "explicit feature:release push should not create refs/heads/stable"
    );
}

#[cfg(unix)]
#[test]
#[serial(env)]
fn test_push_merge_commit_to_git_remote_succeeds() {
    let temp_root = tempfile::tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let local_dir = temp_root.path().join("local");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );

    init_local_repo_with_commit(&local_dir, "base.txt", "base\n", "initial commit");
    add_fake_ssh_remote(&local_dir, &remote_dir);
    let branch = current_branch_name(&local_dir);

    let initial_push = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["push", "origin", &branch])
        .output()
        .expect("failed to push initial branch");
    assert!(
        initial_push.status.success(),
        "initial push failed: {}",
        String::from_utf8_lossy(&initial_push.stderr)
    );

    let feature_out = libra_command(&local_dir)
        .args(["branch", "feature"])
        .output()
        .expect("failed to create feature branch");
    assert!(
        feature_out.status.success(),
        "feature branch creation failed: {}",
        String::from_utf8_lossy(&feature_out.stderr)
    );
    let checkout_feature = libra_command(&local_dir)
        .args(["checkout", "feature"])
        .output()
        .expect("failed to checkout feature");
    assert!(
        checkout_feature.status.success(),
        "checkout feature failed: {}",
        String::from_utf8_lossy(&checkout_feature.stderr)
    );
    fs::write(local_dir.join("feature.txt"), "feature\n").expect("failed to write feature file");
    let add_feature = libra_command(&local_dir)
        .args(["add", "feature.txt"])
        .output()
        .expect("failed to add feature file");
    assert!(
        add_feature.status.success(),
        "add feature failed: {}",
        String::from_utf8_lossy(&add_feature.stderr)
    );
    let commit_feature = libra_command(&local_dir)
        .args(["commit", "-m", "feature update", "--no-verify"])
        .output()
        .expect("failed to commit feature");
    assert!(
        commit_feature.status.success(),
        "commit feature failed: {}",
        String::from_utf8_lossy(&commit_feature.stderr)
    );

    let checkout_main = libra_command(&local_dir)
        .args(["checkout", &branch])
        .output()
        .expect("failed to checkout main branch");
    assert!(
        checkout_main.status.success(),
        "checkout main failed: {}",
        String::from_utf8_lossy(&checkout_main.stderr)
    );
    fs::write(local_dir.join("main.txt"), "main\n").expect("failed to write main file");
    let add_main = libra_command(&local_dir)
        .args(["add", "main.txt"])
        .output()
        .expect("failed to add main file");
    assert!(
        add_main.status.success(),
        "add main failed: {}",
        String::from_utf8_lossy(&add_main.stderr)
    );
    let commit_main = libra_command(&local_dir)
        .args(["commit", "-m", "main update", "--no-verify"])
        .output()
        .expect("failed to commit main");
    assert!(
        commit_main.status.success(),
        "commit main failed: {}",
        String::from_utf8_lossy(&commit_main.stderr)
    );

    let merge_out = libra_command(&local_dir)
        .args(["merge", "feature"])
        .output()
        .expect("failed to merge feature");
    assert!(
        merge_out.status.success(),
        "merge feature failed: {}",
        String::from_utf8_lossy(&merge_out.stderr)
    );

    let push_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["push", "origin", &branch])
        .output()
        .expect("failed to push merge commit");
    assert!(
        push_out.status.success(),
        "push merge commit failed: stdout={} stderr={}",
        String::from_utf8_lossy(&push_out.stdout),
        String::from_utf8_lossy(&push_out.stderr)
    );

    let remote_head_out = Command::new("git")
        .args([
            "--git-dir",
            remote_dir.to_str().unwrap(),
            "rev-parse",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .expect("failed to read remote head");
    assert!(
        remote_head_out.status.success(),
        "remote head should exist after merge push: {}",
        String::from_utf8_lossy(&remote_head_out.stderr)
    );
}

#[cfg(unix)]
#[test]
#[serial(env)]
fn test_push_multi_refspec_delete_tags_and_mirror_dry_run() {
    let temp_root = tempfile::tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let local_dir = temp_root.path().join("local");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );

    init_local_repo_with_commit(
        &local_dir,
        "tracked.txt",
        "initial content",
        "initial commit",
    );
    add_fake_ssh_remote(&local_dir, &remote_dir);
    let current_branch = current_branch_name(&local_dir);

    let feature_out = libra_command(&local_dir)
        .args(["branch", "feature"])
        .output()
        .expect("failed to create feature branch");
    assert!(
        feature_out.status.success(),
        "feature branch creation failed: {}",
        String::from_utf8_lossy(&feature_out.stderr)
    );

    let push_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args([
            "--json",
            "push",
            "origin",
            &current_branch,
            "feature:feature",
        ])
        .output()
        .expect("failed to push multi-refspec");
    assert!(
        push_out.status.success(),
        "multi-refspec push failed: {}",
        String::from_utf8_lossy(&push_out.stderr)
    );
    let stdout = String::from_utf8_lossy(&push_out.stdout);
    let json: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("multi-refspec push should emit valid JSON, got: {stdout}\nerror: {e}")
    });
    let updates = json["data"]["updates"]
        .as_array()
        .expect("updates should be an array");
    assert_eq!(updates.len(), 2, "expected two ref updates: {json}");
    assert!(updates.iter().all(|update| update["kind"] == "update"));
    assert!(
        updates
            .iter()
            .any(|update| { update["remote_ref"] == format!("refs/heads/{current_branch}") })
    );
    assert!(
        updates
            .iter()
            .any(|update| update["remote_ref"] == "refs/heads/feature")
    );

    let feature_ref_out = Command::new("git")
        .args([
            "--git-dir",
            remote_dir.to_str().unwrap(),
            "show-ref",
            "--verify",
            "refs/heads/feature",
        ])
        .output()
        .expect("failed to inspect remote feature ref");
    assert!(
        feature_ref_out.status.success(),
        "remote feature ref should exist after multi-refspec push"
    );

    let invalid_upstream_delete_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["push", "-u", "origin", ":feature"])
        .output()
        .expect("failed to attempt invalid upstream delete push");
    assert!(
        !invalid_upstream_delete_out.status.success(),
        "-u with a delete refspec should fail before deleting the remote ref"
    );
    let stderr = String::from_utf8_lossy(&invalid_upstream_delete_out.stderr);
    assert!(
        stderr.contains("--set-upstream only supports branch update refspecs"),
        "unexpected invalid upstream delete stderr: {stderr}"
    );
    let feature_ref_out = Command::new("git")
        .args([
            "--git-dir",
            remote_dir.to_str().unwrap(),
            "show-ref",
            "--verify",
            "refs/heads/feature",
        ])
        .output()
        .expect("failed to inspect remote feature ref after rejected -u delete");
    assert!(
        feature_ref_out.status.success(),
        "rejected -u delete must not remove the remote feature ref"
    );

    let delete_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["--json", "push", "origin", ":feature"])
        .output()
        .expect("failed to delete remote feature ref");
    assert!(
        delete_out.status.success(),
        "delete ref push failed: {}",
        String::from_utf8_lossy(&delete_out.stderr)
    );
    let stdout = String::from_utf8_lossy(&delete_out.stdout);
    let json: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("delete push should emit valid JSON, got: {stdout}\nerror: {e}")
    });
    assert_eq!(json["data"]["updates"][0]["kind"], "delete");
    assert_eq!(
        json["data"]["updates"][0]["remote_ref"],
        "refs/heads/feature"
    );
    let feature_ref_out = Command::new("git")
        .args([
            "--git-dir",
            remote_dir.to_str().unwrap(),
            "show-ref",
            "--verify",
            "refs/heads/feature",
        ])
        .output()
        .expect("failed to inspect remote feature ref after delete");
    assert!(
        !feature_ref_out.status.success(),
        "remote feature ref should be deleted"
    );

    let tag_out = libra_command(&local_dir)
        .args(["tag", "v1.0"])
        .output()
        .expect("failed to create local tag");
    assert!(
        tag_out.status.success(),
        "tag create failed: {}",
        String::from_utf8_lossy(&tag_out.stderr)
    );
    let tags_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["--json", "push", "--tags", "origin"])
        .output()
        .expect("failed to push tags");
    assert!(
        tags_out.status.success(),
        "--tags push failed: {}",
        String::from_utf8_lossy(&tags_out.stderr)
    );
    let stdout = String::from_utf8_lossy(&tags_out.stdout);
    let json: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("tags push should emit valid JSON, got: {stdout}\nerror: {e}"));
    assert_eq!(json["data"]["updates"][0]["remote_ref"], "refs/tags/v1.0");
    let tag_ref_out = Command::new("git")
        .args([
            "--git-dir",
            remote_dir.to_str().unwrap(),
            "show-ref",
            "--verify",
            "refs/tags/v1.0",
        ])
        .output()
        .expect("failed to inspect remote tag ref");
    assert!(tag_ref_out.status.success(), "remote tag should exist");

    let main_hash = String::from_utf8(
        Command::new("git")
            .args([
                "--git-dir",
                remote_dir.to_str().unwrap(),
                "rev-parse",
                &format!("refs/heads/{current_branch}"),
            ])
            .output()
            .expect("failed to read remote main hash")
            .stdout,
    )
    .expect("remote hash should be utf8")
    .trim()
    .to_string();
    assert!(
        Command::new("git")
            .args([
                "--git-dir",
                remote_dir.to_str().unwrap(),
                "update-ref",
                "refs/heads/remote-only",
                &main_hash,
            ])
            .status()
            .expect("failed to create remote-only ref")
            .success()
    );

    let mirror_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["--json", "push", "--mirror", "--dry-run", "origin"])
        .output()
        .expect("failed to preview mirror push");
    assert!(
        mirror_out.status.success(),
        "mirror dry-run failed: {}",
        String::from_utf8_lossy(&mirror_out.stderr)
    );
    let stdout = String::from_utf8_lossy(&mirror_out.stdout);
    let json: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("mirror dry-run should emit valid JSON, got: {stdout}\nerror: {e}")
    });
    let updates = json["data"]["updates"]
        .as_array()
        .expect("mirror updates should be an array");
    assert!(updates.iter().any(|update| {
        update["kind"] == "delete" && update["remote_ref"] == "refs/heads/remote-only"
    }));
    let remote_only_ref_out = Command::new("git")
        .args([
            "--git-dir",
            remote_dir.to_str().unwrap(),
            "show-ref",
            "--verify",
            "refs/heads/remote-only",
        ])
        .output()
        .expect("failed to inspect remote-only ref after dry-run");
    assert!(
        remote_only_ref_out.status.success(),
        "mirror dry-run must not delete remote-only ref"
    );
}

#[cfg(unix)]
#[test]
#[serial(env)]
fn test_push_delete_short_name_resolves_tags_and_errors_on_missing() {
    // Issue #465 regression: `push --delete <short-name>` must resolve the
    // name against the remote's advertised refs (heads, then tags) instead of
    // hardcoding refs/heads/, must delete an advertised tag, and must FAIL —
    // not report Everything up-to-date — when the ref does not exist remotely.
    let temp_root = tempfile::tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let local_dir = temp_root.path().join("local");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );

    init_local_repo_with_commit(
        &local_dir,
        "tracked.txt",
        "initial content",
        "initial commit",
    );
    add_fake_ssh_remote(&local_dir, &remote_dir);

    // Publish the branch so the remote advertises refs/heads/<current>, then
    // create and push a tag.
    let current_branch = current_branch_name(&local_dir);
    let initial_push = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["push", "origin", &current_branch])
        .output()
        .expect("failed to push initial branch");
    assert!(
        initial_push.status.success(),
        "initial push failed: {}",
        String::from_utf8_lossy(&initial_push.stderr)
    );

    let tag_out = libra_command(&local_dir)
        .args(["tag", "v1.0"])
        .output()
        .expect("failed to create local tag");
    assert!(
        tag_out.status.success(),
        "tag create failed: {}",
        String::from_utf8_lossy(&tag_out.stderr)
    );
    let push_tag_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["--json", "push", "origin", "v1.0"])
        .output()
        .expect("failed to push tag");
    assert!(
        push_tag_out.status.success(),
        "tag push failed: {}",
        String::from_utf8_lossy(&push_tag_out.stderr)
    );
    assert!(
        git_ref_exists(&remote_dir, "refs/tags/v1.0"),
        "remote tag should exist before the delete"
    );

    // Dry-run delete reports the deletion plan without touching the remote.
    let dry_run_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["--json", "push", "--dry-run", "--delete", "origin", "v1.0"])
        .output()
        .expect("failed to dry-run delete tag");
    assert!(
        dry_run_out.status.success(),
        "dry-run delete of an advertised tag should succeed: {}",
        String::from_utf8_lossy(&dry_run_out.stderr)
    );
    let stdout = String::from_utf8_lossy(&dry_run_out.stdout);
    let json: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("dry-run delete should emit valid JSON, got: {stdout}\nerror: {e}")
    });
    assert_eq!(json["data"]["dry_run"], true);
    assert_eq!(json["data"]["updates"][0]["kind"], "delete");
    assert_eq!(json["data"]["updates"][0]["remote_ref"], "refs/tags/v1.0");
    assert!(
        git_ref_exists(&remote_dir, "refs/tags/v1.0"),
        "dry-run delete must not remove the remote tag"
    );

    // THE regression: short-name delete removes the remote TAG.
    let delete_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["--json", "push", "--delete", "origin", "v1.0"])
        .output()
        .expect("failed to delete remote tag by short name");
    assert!(
        delete_out.status.success(),
        "short-name delete of an advertised tag failed: {}",
        String::from_utf8_lossy(&delete_out.stderr)
    );
    let stdout = String::from_utf8_lossy(&delete_out.stdout);
    let json: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("delete push should emit valid JSON, got: {stdout}\nerror: {e}")
    });
    assert_eq!(json["data"]["updates"][0]["kind"], "delete");
    assert_eq!(
        json["data"]["updates"][0]["remote_ref"], "refs/tags/v1.0",
        "short name must resolve to the advertised TAG namespace"
    );
    assert!(
        !git_ref_exists(&remote_dir, "refs/tags/v1.0"),
        "remote tag should be gone after the short-name delete"
    );

    // A short name matching nothing the remote advertises now FAILS (exit
    // 129, LBR-CLI-003) instead of printing Everything up-to-date with exit 0.
    let missing_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args([
            "--json",
            "push",
            "--delete",
            "origin",
            "zz-nonexistent-probe",
        ])
        .output()
        .expect("failed to attempt missing-ref delete");
    assert!(
        !missing_out.status.success(),
        "deleting an unadvertised ref must fail"
    );
    assert_eq!(
        missing_out.status.code(),
        Some(129),
        "unadvertised delete should exit 129"
    );
    let (_stderr, report) = parse_cli_error_stderr(&missing_out.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert_eq!(report.exit_code, 129);
    assert!(
        report
            .message
            .contains("unable to delete 'zz-nonexistent-probe'"),
        "error must name the ref as typed: {report:?}"
    );

    // Dry-run surfaces the same error before sending anything.
    let dry_missing_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args([
            "--json",
            "push",
            "--dry-run",
            "--delete",
            "origin",
            "zz-nonexistent-probe",
        ])
        .output()
        .expect("failed to attempt missing-ref dry-run delete");
    assert!(
        !dry_missing_out.status.success(),
        "dry-run delete of an unadvertised ref must fail too"
    );
}

#[cfg(unix)]
#[test]
#[serial(env)]
fn test_push_delete_short_name_ambiguous_refused() {
    // A short delete name matching BOTH refs/heads/<name> and refs/tags/<name>
    // on the remote is refused (git: "dst refspec matches more than one").
    let temp_root = tempfile::tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let local_dir = temp_root.path().join("local");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );

    init_local_repo_with_commit(
        &local_dir,
        "tracked.txt",
        "initial content",
        "initial commit",
    );
    add_fake_ssh_remote(&local_dir, &remote_dir);
    let current_branch = current_branch_name(&local_dir);

    let push_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["--json", "push", "origin", &current_branch])
        .output()
        .expect("failed to push branch");
    assert!(
        push_out.status.success(),
        "branch push failed: {}",
        String::from_utf8_lossy(&push_out.stderr)
    );

    let tag_out = libra_command(&local_dir)
        .args(["tag", &current_branch])
        .output()
        .expect("failed to create same-named tag");
    assert!(
        tag_out.status.success(),
        "tag create failed: {}",
        String::from_utf8_lossy(&tag_out.stderr)
    );
    // The short `push origin <name>` form is ambiguous once a same-named tag
    // exists, so publish the tag through its fully-qualified refspec.
    let push_tag_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args([
            "--json",
            "push",
            "origin",
            &format!("refs/tags/{current_branch}"),
        ])
        .output()
        .expect("failed to push same-named tag");
    assert!(
        push_tag_out.status.success(),
        "tag push failed: {}",
        String::from_utf8_lossy(&push_tag_out.stderr)
    );
    assert!(git_ref_exists(
        &remote_dir,
        &format!("refs/heads/{current_branch}")
    ));
    assert!(git_ref_exists(
        &remote_dir,
        &format!("refs/tags/{current_branch}")
    ));

    // The ambiguous short name is refused and deletes NOTHING.
    let ambiguous_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["--json", "push", "--delete", "origin", &current_branch])
        .output()
        .expect("failed to attempt ambiguous delete");
    assert!(
        !ambiguous_out.status.success(),
        "ambiguous short-name delete must be refused"
    );
    // Public error contract (docs/commands/push.md error table): exit 129,
    // LBR-CLI-003, message naming the ref, and the qualification hint.
    assert_eq!(
        ambiguous_out.status.code(),
        Some(129),
        "ambiguous delete should exit 129"
    );
    let (_amb_stderr, amb_report) = parse_cli_error_stderr(&ambiguous_out.stderr);
    assert_eq!(amb_report.error_code, "LBR-CLI-003");
    assert_eq!(amb_report.exit_code, 129);
    assert!(
        amb_report.message.contains(&format!(
            "dst refspec '{current_branch}' matches more than one"
        )),
        "unexpected ambiguity message: {amb_report:?}"
    );
    assert!(
        amb_report
            .hints
            .iter()
            .any(|h| h.contains("refs/heads/") && h.contains("refs/tags/")),
        "ambiguity error must hint at the qualified spellings: {amb_report:?}"
    );
    assert!(git_ref_exists(
        &remote_dir,
        &format!("refs/heads/{current_branch}")
    ));
    assert!(git_ref_exists(
        &remote_dir,
        &format!("refs/tags/{current_branch}")
    ));

    // Fully-qualified spellings remain unambiguous and work.
    let delete_tag_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args([
            "--json",
            "push",
            "origin",
            &format!(":refs/tags/{current_branch}"),
        ])
        .output()
        .expect("failed to delete qualified tag");
    assert!(
        delete_tag_out.status.success(),
        "qualified tag delete failed: {}",
        String::from_utf8_lossy(&delete_tag_out.stderr)
    );
    assert!(!git_ref_exists(
        &remote_dir,
        &format!("refs/tags/{current_branch}")
    ));
    assert!(git_ref_exists(
        &remote_dir,
        &format!("refs/heads/{current_branch}")
    ));
}

#[cfg(unix)]
#[test]
#[serial(env)]
fn test_push_explicit_tag_refspec_uses_tag_namespace() {
    let temp_root = tempfile::tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let local_dir = temp_root.path().join("local");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );

    init_local_repo_with_commit(
        &local_dir,
        "tracked.txt",
        "initial content",
        "initial commit",
    );
    add_fake_ssh_remote(&local_dir, &remote_dir);

    let tag_out = libra_command(&local_dir)
        .args(["tag", "v1.0"])
        .output()
        .expect("failed to create local tag");
    assert!(
        tag_out.status.success(),
        "tag create failed: {}",
        String::from_utf8_lossy(&tag_out.stderr)
    );

    let push_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["--json", "push", "origin", "v1.0:release"])
        .output()
        .expect("failed to push explicit tag refspec");
    assert!(
        push_out.status.success(),
        "explicit tag refspec push failed: {}",
        String::from_utf8_lossy(&push_out.stderr)
    );
    let stdout = String::from_utf8_lossy(&push_out.stdout);
    let json: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("explicit tag refspec push should emit valid JSON, got: {stdout}\nerror: {e}")
    });
    assert_eq!(json["data"]["updates"][0]["local_ref"], "refs/tags/v1.0");
    assert_eq!(
        json["data"]["updates"][0]["remote_ref"],
        "refs/tags/release"
    );

    assert!(
        git_ref_exists(&remote_dir, "refs/tags/release"),
        "explicit tag refspec should create a remote tag ref"
    );
    assert!(
        !git_ref_exists(&remote_dir, "refs/heads/release"),
        "explicit tag refspec must not create a branch with the destination name"
    );
}

#[cfg(unix)]
#[test]
#[serial(env)]
fn test_push_mirror_non_dry_run_deletes_remote_only_ref() {
    let temp_root = tempfile::tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let local_dir = temp_root.path().join("local");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );

    init_local_repo_with_commit(
        &local_dir,
        "tracked.txt",
        "initial content",
        "initial commit",
    );
    add_fake_ssh_remote(&local_dir, &remote_dir);
    let current_branch = current_branch_name(&local_dir);

    let initial_push = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["push", "origin", &current_branch])
        .output()
        .expect("failed to push initial branch");
    assert!(
        initial_push.status.success(),
        "initial push failed: {}",
        String::from_utf8_lossy(&initial_push.stderr)
    );

    let main_hash = String::from_utf8(
        Command::new("git")
            .args([
                "--git-dir",
                remote_dir.to_str().unwrap(),
                "rev-parse",
                &format!("refs/heads/{current_branch}"),
            ])
            .output()
            .expect("failed to read remote branch hash")
            .stdout,
    )
    .expect("remote hash should be utf8")
    .trim()
    .to_string();
    assert!(
        Command::new("git")
            .args([
                "--git-dir",
                remote_dir.to_str().unwrap(),
                "update-ref",
                "refs/heads/remote-only",
                &main_hash,
            ])
            .status()
            .expect("failed to create remote-only ref")
            .success()
    );
    assert!(
        git_ref_exists(&remote_dir, "refs/heads/remote-only"),
        "remote-only ref should exist before mirror push"
    );

    let mirror_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["--json", "push", "--mirror", "origin"])
        .output()
        .expect("failed to run mirror push");
    assert!(
        mirror_out.status.success(),
        "mirror push failed: {}",
        String::from_utf8_lossy(&mirror_out.stderr)
    );
    let stdout = String::from_utf8_lossy(&mirror_out.stdout);
    let json: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("mirror push should emit valid JSON, got: {stdout}\nerror: {e}")
    });
    let updates = json["data"]["updates"]
        .as_array()
        .expect("mirror updates should be an array");
    assert!(updates.iter().any(|update| {
        update["kind"] == "delete" && update["remote_ref"] == "refs/heads/remote-only"
    }));
    assert!(
        !git_ref_exists(&remote_dir, "refs/heads/remote-only"),
        "non-dry-run mirror push should delete remote-only ref"
    );
}

#[cfg(unix)]
#[test]
#[serial(env)]
fn test_push_json_with_set_upstream_keeps_structured_output_clean() {
    let temp_root = tempfile::tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let local_dir = temp_root.path().join("local");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );

    init_local_repo_with_commit(
        &local_dir,
        "tracked.txt",
        "initial content",
        "initial commit",
    );
    add_fake_ssh_remote(&local_dir, &remote_dir);
    let current_branch = current_branch_name(&local_dir);

    let push_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["--json", "push", "-u", "origin", &current_branch])
        .output()
        .expect("failed to run json push");
    assert!(
        push_out.status.success(),
        "json push failed: {}",
        String::from_utf8_lossy(&push_out.stderr)
    );

    let stdout = String::from_utf8_lossy(&push_out.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("json push should emit valid JSON, got: {stdout}\nerror: {e}"));
    assert_eq!(parsed["ok"], Value::Bool(true));
    assert_eq!(parsed["command"], Value::String("push".to_string()));
    assert_eq!(
        parsed["data"]["upstream_set"],
        Value::String(format!("origin/{current_branch}"))
    );
    assert_eq!(
        parsed["data"]["updates"][0]["remote_ref"],
        Value::String(format!("refs/heads/{current_branch}"))
    );

    let stderr = String::from_utf8_lossy(&push_out.stderr);
    assert!(
        stderr.trim().is_empty(),
        "json push success should keep stderr clean, got: {stderr}"
    );
}

#[cfg(unix)]
#[test]
#[serial(env)]
fn test_push_set_upstream_when_remote_is_already_up_to_date() {
    let temp_root = tempfile::tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let local_dir = temp_root.path().join("local");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );

    init_local_repo_with_commit(
        &local_dir,
        "tracked.txt",
        "initial content",
        "initial commit",
    );
    add_fake_ssh_remote(&local_dir, &remote_dir);
    let current_branch = current_branch_name(&local_dir);

    let initial_push = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["push", "origin", &current_branch])
        .output()
        .expect("failed to run initial push");
    assert!(
        initial_push.status.success(),
        "initial push failed: {}",
        String::from_utf8_lossy(&initial_push.stderr)
    );

    let push_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["--json", "push", "-u", "origin", &current_branch])
        .output()
        .expect("failed to run up-to-date -u push");
    assert!(
        push_out.status.success(),
        "up-to-date -u push failed: {}",
        String::from_utf8_lossy(&push_out.stderr)
    );

    let stdout = String::from_utf8_lossy(&push_out.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("up-to-date -u push should emit valid JSON, got: {stdout}\nerror: {e}")
    });
    assert_eq!(
        parsed["data"]["upstream_set"],
        Value::String(format!("origin/{current_branch}"))
    );
    assert_eq!(parsed["data"]["updates"], Value::Array(vec![]));
    assert_eq!(parsed["data"]["up_to_date"], Value::Bool(true));

    let remote_config_out = libra_command(&local_dir)
        .args([
            "config",
            "--get",
            &format!("branch.{current_branch}.remote"),
        ])
        .output()
        .expect("failed to read branch remote config");
    assert!(
        remote_config_out.status.success(),
        "config get branch remote failed: {}",
        String::from_utf8_lossy(&remote_config_out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&remote_config_out.stdout).trim(),
        "origin"
    );

    let merge_config_out = libra_command(&local_dir)
        .args(["config", "--get", &format!("branch.{current_branch}.merge")])
        .output()
        .expect("failed to read branch merge config");
    assert!(
        merge_config_out.status.success(),
        "config get branch merge failed: {}",
        String::from_utf8_lossy(&merge_config_out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&merge_config_out.stdout).trim(),
        format!("refs/heads/{current_branch}")
    );
}

#[cfg(unix)]
#[test]
#[serial(env)]
fn test_push_machine_success_is_single_json_line() {
    let temp_root = tempfile::tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let local_dir = temp_root.path().join("local");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );

    init_local_repo_with_commit(
        &local_dir,
        "tracked.txt",
        "initial content",
        "initial commit",
    );
    add_fake_ssh_remote(&local_dir, &remote_dir);
    let current_branch = current_branch_name(&local_dir);

    let push_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["--machine", "push", "origin", &current_branch])
        .output()
        .expect("failed to run machine push");
    assert!(
        push_out.status.success(),
        "machine push failed: {}",
        String::from_utf8_lossy(&push_out.stderr)
    );

    let stdout = String::from_utf8_lossy(&push_out.stdout);
    let non_empty_lines: Vec<_> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(
        non_empty_lines.len(),
        1,
        "machine push should emit exactly one JSON line, got: {non_empty_lines:?}"
    );
    let parsed: Value = serde_json::from_str(non_empty_lines[0]).unwrap_or_else(|e| {
        panic!(
            "machine push should emit valid JSON, got: {}\nerror: {e}",
            non_empty_lines[0]
        )
    });
    assert_eq!(parsed["ok"], Value::Bool(true));

    let stderr = String::from_utf8_lossy(&push_out.stderr);
    assert!(
        stderr.trim().is_empty(),
        "machine push success should keep stderr clean, got: {stderr}"
    );
}

#[cfg(unix)]
#[test]
#[serial(env)]
fn test_push_quiet_force_still_emits_warning_and_warning_exit_code() {
    let temp_root = tempfile::tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let local_dir = temp_root.path().join("local");
    let other_dir = temp_root.path().join("other");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );

    init_local_repo_with_commit(
        &local_dir,
        "tracked.txt",
        "initial content",
        "initial commit",
    );
    add_fake_ssh_remote(&local_dir, &remote_dir);
    let current_branch = current_branch_name(&local_dir);

    let initial_push_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args(["push", "origin", &current_branch])
        .output()
        .expect("failed initial push");
    assert!(
        initial_push_out.status.success(),
        "initial push failed: {}",
        String::from_utf8_lossy(&initial_push_out.stderr)
    );

    assert!(
        Command::new("git")
            .args([
                "clone",
                "--branch",
                &current_branch,
                remote_dir.to_str().unwrap(),
                other_dir.to_str().unwrap(),
            ])
            .status()
            .expect("failed to clone remote")
            .success()
    );
    assert!(
        Command::new("git")
            .args([
                "-C",
                other_dir.to_str().unwrap(),
                "config",
                "user.name",
                "Git User"
            ])
            .status()
            .expect("failed to configure git user.name")
            .success()
    );
    assert!(
        Command::new("git")
            .args([
                "-C",
                other_dir.to_str().unwrap(),
                "config",
                "user.email",
                "git-user@example.com",
            ])
            .status()
            .expect("failed to configure git user.email")
            .success()
    );
    fs::write(other_dir.join("remote.txt"), "remote change").expect("failed to write remote file");
    assert!(
        Command::new("git")
            .args(["-C", other_dir.to_str().unwrap(), "add", "remote.txt"])
            .status()
            .expect("failed to add remote file")
            .success()
    );
    assert!(
        Command::new("git")
            .args([
                "-C",
                other_dir.to_str().unwrap(),
                "commit",
                "-m",
                "remote change"
            ])
            .status()
            .expect("failed to commit remote change")
            .success()
    );
    assert!(
        Command::new("git")
            .args([
                "-C",
                other_dir.to_str().unwrap(),
                "push",
                "origin",
                &current_branch
            ])
            .status()
            .expect("failed to push remote change")
            .success()
    );
    let remote_diverged_head = String::from_utf8(
        Command::new("git")
            .args([
                "--git-dir",
                remote_dir.to_str().unwrap(),
                "rev-parse",
                &format!("refs/heads/{current_branch}"),
            ])
            .output()
            .expect("failed to read diverged remote head")
            .stdout,
    )
    .expect("remote head not utf8")
    .trim()
    .to_string();

    fs::write(local_dir.join("tracked.txt"), "local divergent change")
        .expect("failed to write local divergent file");
    let add_out = libra_command(&local_dir)
        .args(["add", "tracked.txt"])
        .output()
        .expect("failed to add local divergent change");
    assert!(
        add_out.status.success(),
        "failed to add local divergent change: {}",
        String::from_utf8_lossy(&add_out.stderr)
    );
    let commit_out = libra_command(&local_dir)
        .args(["commit", "-m", "local divergent change"])
        .output()
        .expect("failed to commit local divergent change");
    assert!(
        commit_out.status.success(),
        "failed to commit local divergent change: {}",
        String::from_utf8_lossy(&commit_out.stderr)
    );

    let force_push_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .args([
            "--quiet",
            "--exit-code-on-warning",
            "push",
            "--force",
            "origin",
            &current_branch,
        ])
        .output()
        .expect("failed to force push quietly");
    assert_eq!(
        force_push_out.status.code(),
        Some(9),
        "force push with warning exit code should return 9, stderr: {}",
        String::from_utf8_lossy(&force_push_out.stderr)
    );
    assert!(
        force_push_out.stdout.is_empty(),
        "quiet force push should suppress stdout, got: {}",
        String::from_utf8_lossy(&force_push_out.stdout)
    );
    let stderr = String::from_utf8_lossy(&force_push_out.stderr);
    assert!(
        stderr.contains("warning: force push overwrites remote history"),
        "quiet force push should preserve warning output, got: {stderr}"
    );

    let final_remote_head = String::from_utf8(
        Command::new("git")
            .args([
                "--git-dir",
                remote_dir.to_str().unwrap(),
                "rev-parse",
                &format!("refs/heads/{current_branch}"),
            ])
            .output()
            .expect("failed to read final remote head")
            .stdout,
    )
    .expect("final remote head not utf8")
    .trim()
    .to_string();
    assert_ne!(
        remote_diverged_head, final_remote_head,
        "force push should still update the remote ref"
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial(env)]
async fn test_push_ssh_remote_via_fake_ssh() {
    let temp_root = tempfile::tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let local_dir = temp_root.path().join("local");
    let log_path = temp_root.path().join("fake_ssh.log");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );

    fs::create_dir_all(&local_dir).expect("failed to create local dir");
    let init_out = libra_command(&local_dir)
        .args(["init"])
        .output()
        .expect("failed to init local libra repo");
    assert!(
        init_out.status.success(),
        "local init failed: {}",
        String::from_utf8_lossy(&init_out.stderr)
    );
    configure_local_identity(&local_dir);

    fs::write(local_dir.join("hello.txt"), "hello push ssh").expect("failed to write file");
    let add_out = libra_command(&local_dir)
        .args(["add", "hello.txt"])
        .output()
        .expect("failed to add file");
    assert!(
        add_out.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&add_out.stderr)
    );
    let commit_out = libra_command(&local_dir)
        .args(["commit", "-m", "initial commit"])
        .output()
        .expect("failed to commit");
    assert!(
        commit_out.status.success(),
        "commit failed: {}",
        String::from_utf8_lossy(&commit_out.stderr)
    );
    let current_branch = String::from_utf8(
        libra_command(&local_dir)
            .args(["branch", "--show-current"])
            .output()
            .expect("failed to read current branch")
            .stdout,
    )
    .expect("branch name not utf8")
    .trim()
    .to_string();
    assert!(
        !current_branch.is_empty(),
        "current branch should not be empty"
    );

    let ssh_remote = format!("git@fakehost:{}", remote_dir.to_string_lossy());
    let remote_add_out = libra_command(&local_dir)
        .args(["remote", "add", "origin", &ssh_remote])
        .output()
        .expect("failed to add ssh remote");
    assert!(
        remote_add_out.status.success(),
        "remote add failed: {}",
        String::from_utf8_lossy(&remote_add_out.stderr)
    );

    let push_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .env("LIBRA_TEST_SSH_LOG", &log_path)
        .args(["push", "origin", &current_branch])
        .output()
        .expect("failed to run push over fake ssh");
    assert!(
        push_out.status.success(),
        "push over SSH should succeed, stderr: {}",
        String::from_utf8_lossy(&push_out.stderr)
    );
    let stdout = String::from_utf8_lossy(&push_out.stdout);
    assert!(
        stdout.contains("To ") && stdout.contains("->"),
        "push should report success with ref update summary, stdout: {stdout}"
    );

    let remote_head_out = Command::new("git")
        .args([
            "--git-dir",
            remote_dir.to_str().unwrap(),
            "rev-parse",
            &format!("refs/heads/{current_branch}"),
        ])
        .output()
        .expect("failed to read remote head");
    assert!(
        remote_head_out.status.success(),
        "remote master branch should exist after push, stderr: {}",
        String::from_utf8_lossy(&remote_head_out.stderr)
    );

    let ssh_log = fs::read_to_string(&log_path).expect("failed to read fake ssh log");
    assert!(
        !ssh_log.contains("StrictHostKeyChecking="),
        "default must defer host key checking to the user's ssh_config (git parity), \
         log:\n{ssh_log}"
    );
    assert!(
        ssh_log.contains("BatchMode=yes"),
        "headless runs must enable BatchMode so ssh fails fast instead of prompting, \
         log:\n{ssh_log}"
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial(env)]
async fn test_push_ssh_host_key_failure_is_reported() {
    let temp_root = tempfile::tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let local_dir = temp_root.path().join("local");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );

    fs::create_dir_all(&local_dir).expect("failed to create local dir");
    let init_out = libra_command(&local_dir)
        .args(["init"])
        .output()
        .expect("failed to init local libra repo");
    assert!(
        init_out.status.success(),
        "local init failed: {}",
        String::from_utf8_lossy(&init_out.stderr)
    );
    configure_local_identity(&local_dir);

    fs::write(local_dir.join("hello.txt"), "hello push ssh fail").expect("failed to write file");
    let add_out = libra_command(&local_dir)
        .args(["add", "hello.txt"])
        .output()
        .expect("failed to add file");
    assert!(
        add_out.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&add_out.stderr)
    );
    let commit_out = libra_command(&local_dir)
        .args(["commit", "-m", "initial commit"])
        .output()
        .expect("failed to commit");
    assert!(
        commit_out.status.success(),
        "commit failed: {}",
        String::from_utf8_lossy(&commit_out.stderr)
    );
    let current_branch = String::from_utf8(
        libra_command(&local_dir)
            .args(["branch", "--show-current"])
            .output()
            .expect("failed to read current branch")
            .stdout,
    )
    .expect("branch name not utf8")
    .trim()
    .to_string();
    assert!(
        !current_branch.is_empty(),
        "current branch should not be empty"
    );

    let ssh_remote = format!("git@fakehost:{}", remote_dir.to_string_lossy());
    let remote_add_out = libra_command(&local_dir)
        .args(["remote", "add", "origin", &ssh_remote])
        .output()
        .expect("failed to add ssh remote");
    assert!(
        remote_add_out.status.success(),
        "remote add failed: {}",
        String::from_utf8_lossy(&remote_add_out.stderr)
    );

    let push_out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .env("LIBRA_TEST_SSH_FAIL", "hostkey")
        .args(["push", "origin", &current_branch])
        .output()
        .expect("failed to run push over fake ssh");
    let stderr = String::from_utf8_lossy(&push_out.stderr);
    assert!(
        stderr.contains("Host key verification failed."),
        "push should surface SSH host-key failures, stderr: {stderr}"
    );

    let remote_head_out = Command::new("git")
        .args([
            "--git-dir",
            remote_dir.to_str().unwrap(),
            "rev-parse",
            &format!("refs/heads/{current_branch}"),
        ])
        .output()
        .expect("failed to read remote head");
    assert!(
        !remote_head_out.status.success(),
        "remote branch should not be created when SSH host-key verification fails"
    );
}

/// `libra push --help` surfaces the EXAMPLES banner so users see the
/// six canonical invocations (default upstream push, named origin/branch
/// push, `-u`, `--force`, `--dry-run`, `--json`) without having to read
/// the design doc. Companion to the cross-cutting `--help` EXAMPLES
/// rollout from `docs/development/commands/_general.md` item B.
#[test]
fn test_push_help_lists_examples_banner() {
    let repo = tempfile::tempdir().expect("tempdir for push --help");
    let output = run_libra_command(&["push", "--help"], repo.path());
    assert!(
        output.status.success(),
        "push --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "push --help should include EXAMPLES banner, stdout: {stdout}"
    );
    for invocation in [
        "libra push origin main",
        "libra push -u origin feature-x",
        "libra push --force origin main",
        "libra push --dry-run",
        "libra push --json",
    ] {
        assert!(
            stdout.contains(invocation),
            "push --help should include `{invocation}`, stdout: {stdout}"
        );
    }
}

#[test]
fn push_no_verify_flag_is_accepted() {
    let repo = create_committed_repo_via_cli();
    // `--no-verify` (bypass the pre-push hook) parses and reaches the runtime.
    // Libra runs no client-side pre-push hook, so it is an accepted no-op; with
    // no configured remote it fails at the push-destination check, NOT at clap.
    let output = run_libra_command(&["push", "--no-verify"], repo.path());
    assert!(!output.status.success(), "push without a remote fails");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "--no-verify is accepted by the parser: {stderr}"
    );
}

#[test]
fn push_no_progress_flag_is_accepted() {
    let repo = create_committed_repo_via_cli();
    // `--no-progress` parses and reaches the runtime (the "Compressing/Writing
    // objects" suppression is wired through `progress_output_config`). With no
    // configured remote it fails at the push-destination check, NOT at clap.
    let output = run_libra_command(&["push", "--no-progress"], repo.path());
    assert!(!output.status.success(), "push without a remote fails");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "--no-progress is accepted by the parser: {stderr}"
    );
    assert!(
        stderr.contains("no configured push destination"),
        "--no-progress reaches the push-destination runtime check: {stderr}"
    );
}

// ── lore.md 2.10: real-git-server interop matrix ────────────────────────────

/// Shared scaffold: bare git remote + libra local repo with one pushed
/// commit, wired over the fake-ssh shim (a REAL `git receive-pack` runs).
#[cfg(unix)]
fn interop_setup(
    temp_root: &Path,
) -> (
    PathBuf, /*remote*/
    PathBuf, /*local*/
    PathBuf, /*ssh*/
) {
    let remote_dir = temp_root.join("remote.git");
    let local_dir = temp_root.join("local");
    let ssh_script = create_fake_ssh_script(temp_root);
    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("init bare remote")
            .success()
    );
    fs::create_dir_all(&local_dir).expect("local dir");
    let init_out = libra_command(&local_dir)
        .args(["init"])
        .output()
        .expect("libra init");
    assert!(init_out.status.success());
    configure_local_identity(&local_dir);
    fs::write(local_dir.join("base.txt"), "base\n").expect("write");
    assert!(
        libra_command(&local_dir)
            .args(["add", "base.txt"])
            .output()
            .expect("add")
            .status
            .success()
    );
    assert!(
        libra_command(&local_dir)
            .args(["commit", "-m", "base", "--no-verify"])
            .output()
            .expect("commit")
            .status
            .success()
    );
    add_fake_ssh_remote(&local_dir, &remote_dir);
    let push = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", ssh_script.to_str().unwrap())
        .args(["push", "origin", "main"])
        .output()
        .expect("initial push");
    assert!(
        push.status.success(),
        "initial push: {}",
        String::from_utf8_lossy(&push.stderr)
    );
    (remote_dir, local_dir, ssh_script)
}

/// `--thin` round-trip against a real `git receive-pack`, exercising BOTH
/// server completion paths (index-pack --fix-thin above the unpack limit,
/// unpack-objects below it), with a fixture guaranteeing a real delta win.
#[cfg(unix)]
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_push_thin_roundtrip_real_git_both_unpack_paths() {
    for unpack_limit in ["1", "10000"] {
        let temp_root = tempfile::tempdir().expect("temp root");
        let (remote_dir, local_dir, ssh_script) = interop_setup(temp_root.path());
        assert!(
            Command::new("git")
                .args([
                    "-C",
                    remote_dir.to_str().unwrap(),
                    "config",
                    "receive.unpackLimit",
                    unpack_limit,
                ])
                .status()
                .expect("config unpackLimit")
                .success()
        );
        // Large blob + small edit: guaranteed net delta win.
        let big = "The quick brown fox jumps over the lazy dog. ".repeat(4000);
        fs::write(local_dir.join("large.txt"), &big).expect("write large");
        run_ok(&local_dir, &["add", "large.txt"]);
        run_ok(&local_dir, &["commit", "-m", "large", "--no-verify"]);
        let push = libra_command(&local_dir)
            .env("LIBRA_SSH_COMMAND", ssh_script.to_str().unwrap())
            .args(["push", "origin", "main"])
            .output()
            .expect("push large");
        assert!(
            push.status.success(),
            "{}",
            String::from_utf8_lossy(&push.stderr)
        );
        // Edit a small region; push THIN.
        let edited = big.replacen("quick", "QUICK-EDITED", 3);
        fs::write(local_dir.join("large.txt"), &edited).expect("edit large");
        run_ok(&local_dir, &["add", "large.txt"]);
        run_ok(&local_dir, &["commit", "-m", "edit", "--no-verify"]);
        let thin = libra_command(&local_dir)
            .env("LIBRA_SSH_COMMAND", ssh_script.to_str().unwrap())
            .args(["push", "--thin", "origin", "main"])
            .output()
            .expect("thin push");
        assert!(
            thin.status.success(),
            "thin push (unpackLimit={unpack_limit}): {}",
            String::from_utf8_lossy(&thin.stderr)
        );
        assert!(
            String::from_utf8_lossy(&thin.stderr).contains("delta object(s)")
                || String::from_utf8_lossy(&thin.stdout).contains("delta object(s)"),
            "a delta was actually used: {}",
            String::from_utf8_lossy(&thin.stderr)
        );
        // The REAL arbiter: git fsck + content check on the receiving side.
        assert!(
            Command::new("git")
                .args(["-C", remote_dir.to_str().unwrap(), "fsck", "--strict"])
                .status()
                .expect("fsck")
                .success(),
            "remote repo consistent after thin push"
        );
        let shown = Command::new("git")
            .args(["-C", remote_dir.to_str().unwrap(), "show", "main:large.txt"])
            .output()
            .expect("git show");
        assert_eq!(
            String::from_utf8_lossy(&shown.stdout),
            edited,
            "delta replay produced the exact content (unpackLimit={unpack_limit})"
        );
    }
}

/// Capability degrade paths must fail CLEAN (actionable error, nothing sent):
/// stock git does not advertise push-options; atomic can be disabled.
#[cfg(unix)]
#[tokio::test]
#[serial(env)]
async fn test_push_capability_degrades_clean_against_real_git() {
    let temp_root = tempfile::tempdir().expect("temp root");
    let (remote_dir, local_dir, ssh_script) = interop_setup(temp_root.path());
    fs::write(local_dir.join("next.txt"), "x\n").expect("write");
    run_ok(&local_dir, &["add", "next.txt"]);
    run_ok(&local_dir, &["commit", "-m", "next", "--no-verify"]);
    // push-options: NOT advertised by default → clean refusal.
    let out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", ssh_script.to_str().unwrap())
        .args(["push", "-o", "ci.skip", "origin", "main"])
        .output()
        .expect("push -o");
    assert!(
        !out.status.success(),
        "unadvertised push-options must refuse"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("push option") || err.contains("push-option"),
        "actionable capability error, not a protocol failure: {err}"
    );
    // atomic: disable advertisement → clean refusal.
    assert!(
        Command::new("git")
            .args([
                "-C",
                remote_dir.to_str().unwrap(),
                "config",
                "receive.advertiseAtomic",
                "false",
            ])
            .status()
            .expect("config")
            .success()
    );
    let out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", ssh_script.to_str().unwrap())
        .args(["push", "--atomic", "origin", "main"])
        .output()
        .expect("push --atomic");
    assert!(!out.status.success(), "unadvertised atomic must refuse");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("atomic"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Remote tip unchanged by either refusal.
    let tip = Command::new("git")
        .args([
            "-C",
            remote_dir.to_str().unwrap(),
            "log",
            "--format=%s",
            "-n",
            "1",
            "main",
        ])
        .output()
        .expect("log");
    assert_eq!(String::from_utf8_lossy(&tip.stdout).trim(), "base");
}

/// Push-options round-trip when the server ADVERTISES them (a pre-receive
/// hook dumps $GIT_PUSH_OPTION_*).
#[cfg(unix)]
#[tokio::test]
#[serial(env)]
async fn test_push_options_roundtrip_real_git() {
    use std::os::unix::fs::PermissionsExt;
    let temp_root = tempfile::tempdir().expect("temp root");
    let (remote_dir, local_dir, ssh_script) = interop_setup(temp_root.path());
    assert!(
        Command::new("git")
            .args([
                "-C",
                remote_dir.to_str().unwrap(),
                "config",
                "receive.advertisePushOptions",
                "true",
            ])
            .status()
            .expect("config")
            .success()
    );
    let hook_log = temp_root.path().join("options.log");
    let hook = remote_dir.join("hooks/pre-receive");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\nset -eu\ncat >/dev/null\necho \"count=${{GIT_PUSH_OPTION_COUNT:-none}} first=${{GIT_PUSH_OPTION_0:-none}}\" > {}\n",
            hook_log.display()
        ),
    )
    .expect("hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("chmod");
    fs::write(local_dir.join("opt.txt"), "x\n").expect("write");
    run_ok(&local_dir, &["add", "opt.txt"]);
    run_ok(&local_dir, &["commit", "-m", "opt", "--no-verify"]);
    let out = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", ssh_script.to_str().unwrap())
        .args(["push", "-o", "ci.skip=true", "origin", "main"])
        .output()
        .expect("push -o");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let logged = fs::read_to_string(&hook_log).expect("hook log");
    assert_eq!(logged.trim(), "count=1 first=ci.skip=true");
}

/// --force-if-includes accept/reject against the real remote: rejected when
/// the tracking tip was never integrated, accepted after integration.
#[cfg(unix)]
#[tokio::test]
#[serial(env)]
async fn test_push_force_if_includes_matrix_real_git() {
    let temp_root = tempfile::tempdir().expect("temp root");
    let (remote_dir, local_dir, ssh_script) = interop_setup(temp_root.path());
    // Someone else advances the remote.
    let other = temp_root.path().join("other");
    assert!(
        Command::new("git")
            .args([
                "clone",
                remote_dir.to_str().unwrap(),
                other.to_str().unwrap()
            ])
            .status()
            .expect("clone")
            .success()
    );
    for (key, value) in [("user.name", "o"), ("user.email", "o@o")] {
        assert!(
            Command::new("git")
                .args(["-C", other.to_str().unwrap(), "config", key, value])
                .status()
                .expect("config")
                .success()
        );
    }
    // The bare remote's HEAD points at an unborn `master` — base the other
    // clone's work on the real branch.
    assert!(
        Command::new("git")
            .args([
                "-C",
                other.to_str().unwrap(),
                "checkout",
                "-b",
                "work",
                "origin/main",
            ])
            .status()
            .expect("checkout work")
            .success()
    );
    fs::write(other.join("remote-change.txt"), "r\n").expect("write");
    for argv in [
        vec!["add", "remote-change.txt"],
        vec!["commit", "-m", "remote change"],
        vec!["push", "origin", "HEAD:main"],
    ] {
        let out = Command::new("git")
            .arg("-C")
            .arg(other.to_str().unwrap())
            .args(&argv)
            .output()
            .expect("other git");
        assert!(
            out.status.success(),
            "{argv:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // Local: FETCH (tracking learns the new tip) but do NOT integrate; make
    // a divergent local commit.
    let fetch = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", ssh_script.to_str().unwrap())
        .args(["fetch", "origin"])
        .output()
        .expect("fetch");
    assert!(
        fetch.status.success(),
        "{}",
        String::from_utf8_lossy(&fetch.stderr)
    );
    fs::write(local_dir.join("local-change.txt"), "l\n").expect("write");
    run_ok(&local_dir, &["add", "local-change.txt"]);
    run_ok(&local_dir, &["commit", "-m", "local change", "--no-verify"]);
    // Lease alone would pass (tracking == remote), but the tracking tip was
    // never INTEGRATED → --force-if-includes rejects.
    let rejected = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", ssh_script.to_str().unwrap())
        .args([
            "push",
            "--force-with-lease",
            "--force-if-includes",
            "origin",
            "main",
        ])
        .output()
        .expect("push rejected");
    assert!(
        !rejected.status.success(),
        "unintegrated tracking tip must reject: {}",
        String::from_utf8_lossy(&rejected.stdout)
    );
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("integrated"),
        "{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    // Integrate (merge the tracking ref), then the same push is accepted.
    let merge = libra_command(&local_dir)
        .args(["merge", "origin/main", "-m", "integrate"])
        .output()
        .expect("merge");
    assert!(
        merge.status.success(),
        "{}",
        String::from_utf8_lossy(&merge.stderr)
    );
    let accepted = libra_command(&local_dir)
        .env("LIBRA_SSH_COMMAND", ssh_script.to_str().unwrap())
        .args([
            "push",
            "--force-with-lease",
            "--force-if-includes",
            "origin",
            "main",
        ])
        .output()
        .expect("push accepted");
    assert!(
        accepted.status.success(),
        "integrated tip must be accepted: {}",
        String::from_utf8_lossy(&accepted.stderr)
    );
}

#[cfg(unix)]
fn run_ok(dir: &Path, argv: &[&str]) {
    let out = libra_command(dir).args(argv).output().expect("run libra");
    assert!(
        out.status.success(),
        "{argv:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
