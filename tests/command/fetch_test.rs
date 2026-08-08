//! Tests fetch command behavior for remote ref updates and pack retrieval flows.
//!
//! **Layer:** L1 (most tests). `test_fetch_invalid_remote` is L2 — requires `LIBRA_TEST_GITHUB_TOKEN`.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[cfg(unix)]
use libra::internal::vault;
#[cfg(unix)]
use libra::utils::test::ScopedEnvVar;
use libra::{
    command::fetch,
    internal::{
        branch::Branch,
        config::{ConfigKv, RemoteConfig},
    },
    utils::{
        output::OutputConfig,
        test::{ChangeDirGuard, setup_with_new_libra_in},
    },
};
use serial_test::serial;
use tempfile::{TempDir, tempdir};
use tokio::{process::Command as TokioCommand, time::timeout};

use super::{
    assert_cli_success, create_committed_repo_via_cli, parse_json_stdout, run_libra_command,
};

fn libra_command(cwd: &Path) -> Command {
    let home = cwd.join(".libra-test-home");
    let config_home = home.join(".config");
    fs::create_dir_all(&config_home).expect("failed to create isolated HOME");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_libra"));
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", &home)
        .env("LIBRA_TEST", "1");
    cmd
}

fn libra_tokio_command(cwd: &Path) -> TokioCommand {
    let home = cwd.join(".libra-test-home");
    let config_home = home.join(".config");
    fs::create_dir_all(&config_home).expect("failed to create isolated HOME");

    let mut cmd = TokioCommand::new(env!("CARGO_BIN_EXE_libra"));
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("USERPROFILE", &home)
        .env("LIBRA_TEST", "1");
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

async fn setup_local_fetch_cli_fixture() -> (TempDir, PathBuf, String, String) {
    let temp_root = tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let work_dir = temp_root.path().join("workdir");
    let repo_dir = temp_root.path().join("libra_repo");

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );
    assert!(
        Command::new("git")
            .args(["init", work_dir.to_str().unwrap()])
            .status()
            .expect("failed to init working repo")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["config", "user.name", "Libra Tester"])
            .status()
            .expect("failed to set user.name")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["config", "user.email", "tester@example.com"])
            .status()
            .expect("failed to set user.email")
            .success()
    );

    fs::write(work_dir.join("README.md"), "hello libra").expect("failed to write README");
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["add", "README.md"])
            .status()
            .expect("failed to add README")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["commit", "-m", "initial commit"])
            .status()
            .expect("failed to commit")
            .success()
    );

    let current_branch = String::from_utf8(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .expect("failed to read current branch")
            .stdout,
    )
    .expect("branch name not utf8")
    .trim()
    .to_string();

    let pushed_commit = String::from_utf8(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("failed to read HEAD commit")
            .stdout,
    )
    .expect("commit hash not utf8")
    .trim()
    .to_string();

    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["remote", "add", "origin", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to add origin remote")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args([
                "push",
                "origin",
                &format!("HEAD:refs/heads/{current_branch}"),
            ])
            .status()
            .expect("failed to push to remote")
            .success()
    );

    fs::create_dir_all(&repo_dir).expect("failed to create repo dir");
    setup_with_new_libra_in(&repo_dir).await;
    let _guard = ChangeDirGuard::new(&repo_dir);
    let remote_path = remote_dir.to_str().unwrap().to_string();
    ConfigKv::set("remote.origin.url", &remote_path, false)
        .await
        .unwrap();

    (temp_root, repo_dir, current_branch, pushed_commit)
}

#[test]
fn test_fetch_cli_without_remote_is_noop_like_git() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["fetch"], repo.path());

    // Without a configured remote, fetch should fail with a fatal error.
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no configured remote for the current branch"));
    assert!(stderr.contains("Error-Code: LBR-REPO-003"));
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
/// Test fetching from an invalid remote repository with timeout
async fn test_fetch_invalid_remote() {
    if std::env::var("LIBRA_TEST_GITHUB_TOKEN").map_or(true, |v| v.is_empty()) {
        eprintln!("skipped (LIBRA_TEST_GITHUB_TOKEN not set)");
        return;
    }
    let temp_repo = init_temp_repo();
    let temp_path = temp_repo.path();

    eprintln!("Starting test: fetch from invalid remote");

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

    // Attempt to fetch with 15-second timeout to avoid hanging CI
    eprintln!("Attempting 'libra fetch' with 15s timeout...");
    let fetch_result = timeout(Duration::from_secs(15), async {
        libra_tokio_command(temp_path).arg("fetch").output().await
    })
    .await;

    match fetch_result {
        // Timeout occurred — this is expected for unreachable remotes
        Err(_) => {
            eprintln!("Fetch timed out after 15 seconds — expected for invalid remote");
        }
        // Command completed within timeout
        Ok(Ok(output)) => {
            eprintln!("Fetch completed (status: {:?})", output.status);
            assert!(
                !output.status.success(),
                "Fetch should fail when remote is unreachable"
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !stderr.trim().is_empty(),
                "Expected error message in stderr, but was empty"
            );

            eprintln!("Fetch failed as expected: {stderr}");
        }
        // Failed to start the command
        Ok(Err(e)) => {
            panic!("Failed to run 'libra fetch' command: {e}");
        }
    }

    eprintln!("test_fetch_invalid_remote passed");
}

#[tokio::test]
#[serial]
async fn test_fetch_local_repository() {
    let temp_root = tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let work_dir = temp_root.path().join("workdir");

    // Prepare remote bare repository with an initial commit pushed from a working clone
    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );

    assert!(
        Command::new("git")
            .args(["init", work_dir.to_str().unwrap()])
            .status()
            .expect("failed to init working repo")
            .success()
    );

    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["config", "user.name", "Libra Tester"])
            .status()
            .expect("failed to set user.name")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["config", "user.email", "tester@example.com"])
            .status()
            .expect("failed to set user.email")
            .success()
    );

    fs::write(work_dir.join("README.md"), "hello libra").expect("failed to write README");
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["add", "README.md"])
            .status()
            .expect("failed to add README")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["commit", "-m", "initial commit"])
            .status()
            .expect("failed to commit")
            .success()
    );

    let current_branch = String::from_utf8(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .expect("failed to read current branch")
            .stdout,
    )
    .expect("branch name not utf8")
    .trim()
    .to_string();

    let pushed_commit = String::from_utf8(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("failed to read HEAD commit")
            .stdout,
    )
    .expect("commit hash not utf8")
    .trim()
    .to_string();

    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["remote", "add", "origin", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to add origin remote")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args([
                "push",
                "origin",
                &format!("HEAD:refs/heads/{current_branch}"),
            ])
            .status()
            .expect("failed to push to remote")
            .success()
    );

    // Initialize a fresh Libra repository to fetch into
    let repo_dir = temp_root.path().join("libra_repo");
    fs::create_dir_all(&repo_dir).expect("failed to create repo dir");
    setup_with_new_libra_in(&repo_dir).await;
    let _guard = ChangeDirGuard::new(&repo_dir);

    let remote_path = remote_dir.to_str().unwrap().to_string();
    ConfigKv::set("remote.origin.url", &remote_path, false)
        .await
        .unwrap();

    fetch::fetch_repository(
        RemoteConfig {
            name: "origin".to_string(),
            url: remote_path.clone(),
        },
        None,
        false,
        None,
    )
    .await;

    // Migrated from lossy `Branch::find_branch` per docs/development/commands/branch.md —
    // storage errors no longer collapse into "remote-tracking branch not found".
    let tracked_branch = Branch::find_branch_result(
        &format!("refs/remotes/origin/{current_branch}"),
        Some("origin"),
    )
    .await
    .expect("failed to query remote-tracking branch")
    .expect("remote-tracking branch not found");
    assert_eq!(tracked_branch.commit.to_string(), pushed_commit);
}

#[tokio::test]
#[serial]
async fn test_fetch_json_output_reports_updated_refs() {
    let (_temp_root, repo_dir, current_branch, pushed_commit) =
        setup_local_fetch_cli_fixture().await;

    let output = run_libra_command(&["--json", "fetch", "origin"], &repo_dir);
    assert_cli_success(&output, "fetch --json origin");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "fetch");
    assert_eq!(json["data"]["all"], false);
    assert_eq!(json["data"]["requested_remote"], "origin");
    assert_eq!(json["data"]["remotes"][0]["remote"], "origin");
    assert_eq!(
        json["data"]["remotes"][0]["refs_updated"][0]["remote_ref"],
        format!("refs/remotes/origin/{current_branch}")
    );
    assert_eq!(
        json["data"]["remotes"][0]["refs_updated"][0]["new_oid"],
        pushed_commit
    );
    assert!(
        json["data"]["remotes"][0]["objects_fetched"]
            .as_u64()
            .expect("objects_fetched should be a number")
            > 0
    );
    assert!(
        json["data"]["remotes"][0]["bytes_received"]
            .as_u64()
            .expect("bytes_received should be a number")
            > 0
    );
}

#[tokio::test]
#[serial]
async fn test_fetch_machine_output_is_single_line_json() {
    let (_temp_root, repo_dir, _current_branch, _pushed_commit) =
        setup_local_fetch_cli_fixture().await;

    let output = run_libra_command(&["--machine", "fetch", "origin"], &repo_dir);
    assert_cli_success(&output, "fetch --machine origin");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().count(),
        1,
        "machine output must be single-line JSON"
    );
    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "fetch");
    assert_eq!(json["data"]["requested_remote"], "origin");
    assert!(
        output.stderr.is_empty(),
        "machine mode should keep stderr clean, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
#[serial]
async fn test_fetch_json_emits_progress_events_to_stderr() {
    let (_temp_root, repo_dir, _current_branch, _pushed_commit) =
        setup_local_fetch_cli_fixture().await;

    let output = run_libra_command(&["--json", "fetch", "origin"], &repo_dir);
    assert_cli_success(&output, "fetch --json origin");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("\"event\":\"progress_done\""),
        "expected progress_done event in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("\"task\":\"fetch origin\""),
        "expected fetch task name in stderr, got: {stderr}"
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_fetch_ssh_remote_via_fake_ssh() {
    let temp_root = tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let work_dir = temp_root.path().join("workdir");
    let repo_dir = temp_root.path().join("libra_repo");
    let log_path = temp_root.path().join("fake_ssh.log");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );
    assert!(
        Command::new("git")
            .args(["init", work_dir.to_str().unwrap()])
            .status()
            .expect("failed to init working repo")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["config", "user.name", "Libra Tester"])
            .status()
            .expect("failed to set user.name")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["config", "user.email", "tester@example.com"])
            .status()
            .expect("failed to set user.email")
            .success()
    );

    fs::write(work_dir.join("README.md"), "hello ssh fetch").expect("failed to write README");
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["add", "README.md"])
            .status()
            .expect("failed to add README")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["commit", "-m", "initial commit"])
            .status()
            .expect("failed to commit")
            .success()
    );
    let current_branch = String::from_utf8(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .expect("failed to read current branch")
            .stdout,
    )
    .expect("branch name not utf8")
    .trim()
    .to_string();
    let pushed_commit = String::from_utf8(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("failed to read HEAD commit")
            .stdout,
    )
    .expect("commit hash not utf8")
    .trim()
    .to_string();
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["remote", "add", "origin", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to add origin remote")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args([
                "push",
                "origin",
                &format!("HEAD:refs/heads/{current_branch}"),
            ])
            .status()
            .expect("failed to push to remote")
            .success()
    );

    fs::create_dir_all(&repo_dir).expect("failed to create repo dir");
    setup_with_new_libra_in(&repo_dir).await;
    let _guard = ChangeDirGuard::new(&repo_dir);

    let ssh_remote = format!("git@fakehost:{}", remote_dir.to_string_lossy());
    ConfigKv::set("remote.origin.url", &ssh_remote, false)
        .await
        .unwrap();

    let fetch_out = libra_command(&repo_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .env("LIBRA_TEST_SSH_LOG", &log_path)
        .args(["fetch", "origin"])
        .output()
        .expect("failed to run libra fetch over fake ssh");
    assert!(
        fetch_out.status.success(),
        "fetch over SSH should succeed, stderr: {}",
        String::from_utf8_lossy(&fetch_out.stderr)
    );

    // Migrated from lossy `Branch::find_branch` per docs/development/commands/branch.md —
    // storage errors no longer collapse into "remote-tracking branch not found".
    let tracked_branch = Branch::find_branch_result(
        &format!("refs/remotes/origin/{current_branch}"),
        Some("origin"),
    )
    .await
    .expect("failed to query remote-tracking branch")
    .expect("remote-tracking branch not found");
    assert_eq!(tracked_branch.commit.to_string(), pushed_commit);

    let ssh_log = fs::read_to_string(&log_path).expect("failed to read fake ssh log");
    assert!(
        ssh_log.contains("StrictHostKeyChecking=yes"),
        "SSH command should enforce strict host key checking, log:\n{ssh_log}"
    );
    assert!(
        !ssh_log.contains("StrictHostKeyChecking=accept-new"),
        "SSH command must not use accept-new by default, log:\n{ssh_log}"
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_fetch_ssh_respects_strict_host_key_checking_config_casing() {
    let temp_root = tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let repo_dir = temp_root.path().join("libra_repo");
    let work_dir = temp_root.path().join("git_work");
    let log_path = temp_root.path().join("fake_ssh.log");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );
    fs::create_dir_all(&work_dir).expect("failed to create work dir");
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["init"])
            .status()
            .expect("failed to init git workdir")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["config", "user.name", "Fetch Test User"])
            .status()
            .expect("failed to configure git user.name")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["config", "user.email", "fetch-test@example.com"])
            .status()
            .expect("failed to configure git user.email")
            .success()
    );
    fs::write(work_dir.join("README.md"), "hello ssh fetch").expect("failed to write README");
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["add", "README.md"])
            .status()
            .expect("failed to add README")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["commit", "-m", "initial commit"])
            .status()
            .expect("failed to commit")
            .success()
    );
    let current_branch = String::from_utf8(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .expect("failed to read current branch")
            .stdout,
    )
    .expect("branch name not utf8")
    .trim()
    .to_string();
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["remote", "add", "origin", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to add origin remote")
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&work_dir)
            .args([
                "push",
                "origin",
                &format!("HEAD:refs/heads/{current_branch}"),
            ])
            .status()
            .expect("failed to push to remote")
            .success()
    );

    fs::create_dir_all(&repo_dir).expect("failed to create repo dir");
    setup_with_new_libra_in(&repo_dir).await;
    let _guard = ChangeDirGuard::new(&repo_dir);

    let ssh_remote = format!("git@fakehost:{}", remote_dir.to_string_lossy());
    ConfigKv::set("remote.origin.url", &ssh_remote, false)
        .await
        .unwrap();
    ConfigKv::set("ssh.strictHostKeyChecking", "accept-new", false)
        .await
        .unwrap();

    let fetch_out = libra_command(&repo_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .env("LIBRA_TEST_SSH_LOG", &log_path)
        .args(["fetch", "origin"])
        .output()
        .expect("failed to run libra fetch over fake ssh");
    assert!(
        fetch_out.status.success(),
        "fetch over SSH should succeed, stderr: {}",
        String::from_utf8_lossy(&fetch_out.stderr)
    );

    let ssh_log = fs::read_to_string(&log_path).expect("failed to read fake ssh log");
    assert!(
        ssh_log.contains("StrictHostKeyChecking=accept-new"),
        "SSH command should use configured strictHostKeyChecking mode, log:\n{ssh_log}"
    );
    assert!(
        !ssh_log.contains("StrictHostKeyChecking=yes"),
        "configured mode should override default strict host key checking, log:\n{ssh_log}"
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_fetch_ssh_host_key_failure_is_reported() {
    let temp_root = tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let repo_dir = temp_root.path().join("libra_repo");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );
    fs::create_dir_all(&repo_dir).expect("failed to create repo dir");
    setup_with_new_libra_in(&repo_dir).await;

    let ssh_remote = format!("git@fakehost:{}", remote_dir.to_string_lossy());
    let _guard = ChangeDirGuard::new(&repo_dir);
    ConfigKv::set("remote.origin.url", &ssh_remote, false)
        .await
        .unwrap();

    let fetch_out = libra_command(&repo_dir)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .env("LIBRA_TEST_SSH_FAIL", "hostkey")
        .args(["fetch", "origin"])
        .output()
        .expect("failed to run libra fetch over fake ssh");
    let stderr = String::from_utf8_lossy(&fetch_out.stderr);
    assert!(
        stderr.contains("Host key verification failed."),
        "fetch should surface SSH host-key failures, stderr: {stderr}"
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_fetch_ssh_invalid_vault_key_fails_without_fallback() {
    let temp_root = tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let repo_dir = temp_root.path().join("libra_repo");
    let home_dir = repo_dir.join(".libra-test-home");
    let config_home = home_dir.join(".config");
    let log_path = temp_root.path().join("fake_ssh.log");
    let ssh_script = create_fake_ssh_script(temp_root.path());

    assert!(
        Command::new("git")
            .args(["init", "--bare", remote_dir.to_str().unwrap()])
            .status()
            .expect("failed to init bare remote")
            .success()
    );
    fs::create_dir_all(&repo_dir).expect("failed to create repo dir");
    setup_with_new_libra_in(&repo_dir).await;
    fs::create_dir_all(&config_home).expect("failed to create config home");

    let ssh_remote = format!("git@fakehost:{}", remote_dir.to_string_lossy());
    let _home = ScopedEnvVar::set("HOME", &home_dir);
    let _userprofile = ScopedEnvVar::set("USERPROFILE", &home_dir);
    let _xdg = ScopedEnvVar::set("XDG_CONFIG_HOME", &config_home);
    let _guard = ChangeDirGuard::new(&repo_dir);
    vault::lazy_init_vault_for_scope("local")
        .await
        .expect("failed to initialize local vault");
    ConfigKv::set("remote.origin.url", &ssh_remote, false)
        .await
        .unwrap();
    ConfigKv::set("vault.ssh.origin.privkey", "not-valid-hex", true)
        .await
        .unwrap();

    let fetch_out = libra_command(&repo_dir)
        .env("HOME", &home_dir)
        .env("USERPROFILE", &home_dir)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("LIBRA_SSH_COMMAND", &ssh_script)
        .env("LIBRA_TEST_SSH_LOG", &log_path)
        .args(["fetch", "origin"])
        .output()
        .expect("failed to run libra fetch over fake ssh");
    let stderr = String::from_utf8_lossy(&fetch_out.stderr);
    assert!(
        !fetch_out.status.success(),
        "fetch should fail when configured vault SSH key is invalid"
    );
    assert!(
        stderr.contains("failed to decode vault SSH private key 'vault.ssh.origin.privkey'"),
        "fetch should report invalid configured vault SSH key, stderr: {stderr}"
    );
    assert!(
        !log_path.exists(),
        "fetch should fail before invoking SSH when vault key is invalid"
    );
}

// ---- C3: shallow-fetch contract (`libra fetch --depth N`) ---------------------------------
//
// The internal `fetch_repository(..., depth)` plumbing has supported shallow fetch for some
// time; C3 (compat plan) surfaces it as a public, stable CLI flag. These tests verify the
// public surface contract — not the wire-level shallow protocol semantics, which are owned
// by `git_internal` and exercised through its own test suites.

#[test]
fn test_fetch_help_lists_depth_flag_without_experimental() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["fetch", "--help"], repo.path());
    assert!(
        output.status.success(),
        "fetch --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--depth"),
        "fetch --help must surface --depth flag (C3 contract), stdout: {stdout}"
    );
    assert!(
        !stdout.to_lowercase().contains("experimental"),
        "fetch --depth is a stable public flag; --help must not mark it experimental, stdout: {stdout}"
    );
}

#[tokio::test]
#[serial]
async fn test_fetch_with_depth_one_against_local_remote() {
    // Smoke: `libra fetch origin --depth 1` succeeds against a local file remote
    // and reports the same JSON envelope shape as a non-shallow fetch.
    let (_temp_root, repo_dir, current_branch, pushed_commit) =
        setup_local_fetch_cli_fixture().await;

    let output = run_libra_command(&["--json", "fetch", "origin", "--depth", "1"], &repo_dir);
    assert_cli_success(&output, "fetch --json origin --depth 1");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "fetch");
    assert_eq!(json["data"]["all"], false);
    assert_eq!(json["data"]["requested_remote"], "origin");
    assert_eq!(json["data"]["remotes"][0]["remote"], "origin");
    assert_eq!(
        json["data"]["remotes"][0]["refs_updated"][0]["remote_ref"],
        format!("refs/remotes/origin/{current_branch}")
    );
    assert_eq!(
        json["data"]["remotes"][0]["refs_updated"][0]["new_oid"],
        pushed_commit
    );
}

#[tokio::test]
#[serial]
async fn test_fetch_all_with_depth_runs_across_remotes() {
    // `libra fetch --all --depth N` must accept both flags together and pass `depth`
    // through to every configured remote; conflicts_with("repository") on `--all`
    // already prevents the bad combination.
    let (_temp_root, repo_dir, current_branch, _pushed_commit) =
        setup_local_fetch_cli_fixture().await;

    let output = run_libra_command(&["--json", "fetch", "--all", "--depth", "3"], &repo_dir);
    assert_cli_success(&output, "fetch --json --all --depth 3");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "fetch");
    assert_eq!(json["data"]["all"], true);
    let remotes = json["data"]["remotes"]
        .as_array()
        .expect("remotes should be an array");
    assert!(
        !remotes.is_empty(),
        "fetch --all should report at least one remote"
    );
    let origin_seen = remotes.iter().any(|r| r["remote"] == "origin");
    assert!(origin_seen, "fetch --all should include 'origin' remote");
    let _ = current_branch;
}

#[tokio::test]
#[serial]
async fn test_fetch_full_then_shallow_is_idempotent() {
    // After a full (non-shallow) fetch has already populated origin's tracking
    // refs, re-running with `--depth 1` must not error. This exercises the
    // common workflow where a developer first does a regular fetch and then
    // wants to refresh just the tip.
    //
    // The converse case (shallow → shallow re-fetch) is covered by
    // `test_fetch_shallow_then_shallow_is_idempotent` below.
    let (_temp_root, repo_dir, _current_branch, _pushed_commit) =
        setup_local_fetch_cli_fixture().await;

    let first = run_libra_command(&["fetch", "origin"], &repo_dir);
    assert_cli_success(&first, "first fetch (full)");

    let second = run_libra_command(&["fetch", "origin", "--depth", "1"], &repo_dir);
    assert_cli_success(&second, "second fetch --depth 1 after full");
}

#[tokio::test]
#[serial]
async fn test_fetch_shallow_then_shallow_is_idempotent() {
    // C3 follow-up: once a shallow boundary has been created locally,
    // re-running the same shallow fetch should still negotiate cleanly.
    let (_temp_root, repo_dir, _current_branch, pushed_commit) =
        setup_local_fetch_cli_fixture().await;

    let first = run_libra_command(&["--json", "fetch", "origin", "--depth", "1"], &repo_dir);
    assert_cli_success(&first, "first fetch --depth 1");
    let first_json = parse_json_stdout(&first);
    assert!(
        first_json["data"]["remotes"][0]["objects_fetched"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "first shallow fetch must materialize at least one object: {first_json:?}"
    );

    let shallow_path = repo_dir.join(".libra").join("shallow");
    let shallow = fs::read_to_string(&shallow_path)
        .expect("first shallow fetch must persist .libra/shallow metadata");
    assert!(
        shallow.lines().any(|line| line.trim() == pushed_commit),
        "shallow metadata must contain the fetched boundary {pushed_commit}; got {shallow:?}"
    );

    let second = run_libra_command(&["fetch", "origin", "--depth", "1"], &repo_dir);
    assert_cli_success(&second, "second fetch --depth 1 after shallow");
}

/// `libra fetch --dry-run` previews the remote-tracking ref updates without
/// downloading any pack or writing refs / FETCH_HEAD.
#[tokio::test]
#[serial]
async fn test_fetch_dry_run_previews_without_writing() {
    let (_temp_root, repo_dir, current_branch, pushed_commit) =
        setup_local_fetch_cli_fixture().await;

    let output = run_libra_command(&["--json", "fetch", "origin", "--dry-run"], &repo_dir);
    assert_cli_success(&output, "fetch --dry-run origin");

    let json = parse_json_stdout(&output);
    assert_eq!(
        json["data"]["remotes"][0]["refs_updated"][0]["remote_ref"],
        format!("refs/remotes/origin/{current_branch}")
    );
    assert_eq!(
        json["data"]["remotes"][0]["refs_updated"][0]["new_oid"],
        pushed_commit
    );
    // Dry-run downloads nothing.
    assert_eq!(json["data"]["remotes"][0]["objects_fetched"], 0);
    assert_eq!(json["data"]["remotes"][0]["bytes_received"], 0);

    // No remote-tracking ref was written, and no FETCH_HEAD was created.
    assert!(
        !repo_dir.join(".libra/FETCH_HEAD").exists(),
        "--dry-run must not write FETCH_HEAD"
    );
    let _guard = ChangeDirGuard::new(&repo_dir);
    let tracking = Branch::find_branch_result(
        &format!("refs/remotes/origin/{current_branch}"),
        Some("origin"),
    )
    .await
    .expect("branch lookup should succeed");
    assert!(
        tracking.is_none(),
        "--dry-run must not persist a remote-tracking ref"
    );
}

/// `libra fetch --porcelain` prints one machine-readable line per ref update.
#[tokio::test]
#[serial]
async fn test_fetch_porcelain_prints_per_ref_lines() {
    let (_temp_root, repo_dir, current_branch, pushed_commit) =
        setup_local_fetch_cli_fixture().await;

    let output = run_libra_command(&["fetch", "origin", "--porcelain"], &repo_dir);
    assert_cli_success(&output, "fetch --porcelain origin");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find(|line| line.contains(&format!("refs/remotes/origin/{current_branch}")))
        .unwrap_or_else(|| panic!("expected a porcelain line for the fetched ref, got: {stdout}"));
    let cols: Vec<&str> = line.split(' ').collect();
    // `<flag> <old-oid> <new-oid> <local-ref>` — new ref uses the `*` flag.
    assert_eq!(cols[0], "*", "a new ref must use the `*` flag");
    assert_eq!(cols[2], pushed_commit, "third column must be the new oid");
    assert_eq!(
        cols[3],
        format!("refs/remotes/origin/{current_branch}"),
        "fourth column must be the local tracking ref"
    );
}

/// `--porcelain` and the global `--json` are both machine formats and must not
/// be combined (usage error, exit 129).
#[tokio::test]
#[serial]
async fn test_fetch_porcelain_rejects_combination_with_json() {
    let (_temp_root, repo_dir, _current_branch, _pushed_commit) =
        setup_local_fetch_cli_fixture().await;

    let output = run_libra_command(&["--json", "fetch", "origin", "--porcelain"], &repo_dir);
    assert_eq!(
        output.status.code(),
        Some(129),
        "combining --porcelain with --json must be a usage error"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--porcelain") && stderr.contains("--json"),
        "error must mention the conflicting flags, got: {stderr}"
    );
}

/// `-v/--verbose` announces the remote being contacted on stderr without
/// changing the stdout result contract.
#[tokio::test]
#[serial]
async fn test_fetch_verbose_announces_remote_on_stderr() {
    let (_temp_root, repo_dir, _current_branch, _pushed_commit) =
        setup_local_fetch_cli_fixture().await;

    let output = run_libra_command(&["fetch", "origin", "-v"], &repo_dir);
    assert_cli_success(&output, "fetch -v origin");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Fetching origin from"),
        "verbose mode must announce the remote on stderr, got: {stderr}"
    );
}

/// A plain `libra fetch` writes `.libra/FETCH_HEAD`; `--append` accumulates
/// rather than overwriting.
#[tokio::test]
#[serial]
async fn test_fetch_writes_and_appends_fetch_head() {
    let (_temp_root, repo_dir, current_branch, pushed_commit) =
        setup_local_fetch_cli_fixture().await;

    let first = run_libra_command(&["fetch", "origin"], &repo_dir);
    assert_cli_success(&first, "fetch origin (writes FETCH_HEAD)");

    let fetch_head_path = repo_dir.join(".libra/FETCH_HEAD");
    assert!(
        fetch_head_path.exists(),
        "fetch must write .libra/FETCH_HEAD"
    );
    let body = fs::read_to_string(&fetch_head_path).expect("read FETCH_HEAD");
    assert!(
        body.contains(&pushed_commit) && body.contains("not-for-merge"),
        "FETCH_HEAD must record the fetched oid as not-for-merge, got: {body}"
    );
    assert!(
        body.contains(&format!("branch '{current_branch}'")),
        "FETCH_HEAD must describe the fetched branch, got: {body}"
    );

    // `--append` accumulates: re-fetching with --append keeps prior lines.
    let append = run_libra_command(&["fetch", "origin", "--append"], &repo_dir);
    assert_cli_success(&append, "fetch --append origin");
    let appended = fs::read_to_string(&fetch_head_path).expect("read FETCH_HEAD after append");
    assert!(
        appended.matches("not-for-merge").count() >= body.matches("not-for-merge").count(),
        "--append must not shrink FETCH_HEAD, before: {body}\nafter: {appended}"
    );
}

/// Like `setup_local_fetch_cli_fixture`, but the remote also carries a
/// lightweight tag (`light-1`) and an annotated tag (`annot-1`) so tag-fetch
/// behaviour can be exercised end-to-end. Returns the Libra repo dir.
async fn setup_local_fetch_with_tags_fixture() -> (TempDir, PathBuf, String) {
    let temp_root = tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let work_dir = temp_root.path().join("workdir");
    let repo_dir = temp_root.path().join("libra_repo");
    let git_config_global = temp_root.path().join("gitconfig");

    let git = |args: &[&str], cwd: Option<&Path>| {
        let mut cmd = Command::new("git");
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        cmd.env("GIT_CONFIG_GLOBAL", &git_config_global)
            .env("GIT_CONFIG_NOSYSTEM", "1");
        assert!(
            cmd.args(args)
                .status()
                .expect("git invocation failed")
                .success(),
            "git {args:?} failed"
        );
    };

    git(&["init", "--bare", remote_dir.to_str().unwrap()], None);
    git(&["init", work_dir.to_str().unwrap()], None);
    git(&["config", "user.name", "Libra Tester"], Some(&work_dir));
    git(
        &["config", "user.email", "tester@example.com"],
        Some(&work_dir),
    );
    fs::write(work_dir.join("README.md"), "hello libra").expect("write README");
    git(&["add", "README.md"], Some(&work_dir));
    git(&["commit", "-m", "initial commit"], Some(&work_dir));

    let current_branch = String::from_utf8(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .expect("failed to read current branch")
            .stdout,
    )
    .expect("branch name not utf8")
    .trim()
    .to_string();

    // A lightweight tag and an annotated tag. The annotated form exercises tag
    // object download plus local peel for the `have` set.
    git(&["tag", "light-1"], Some(&work_dir));
    git(
        &["tag", "-a", "annot-1", "-m", "annotated release"],
        Some(&work_dir),
    );

    git(
        &["remote", "add", "origin", remote_dir.to_str().unwrap()],
        Some(&work_dir),
    );
    git(
        &[
            "push",
            "origin",
            &format!("HEAD:refs/heads/{current_branch}"),
        ],
        Some(&work_dir),
    );
    git(&["push", "origin", "--tags"], Some(&work_dir));

    fs::create_dir_all(&repo_dir).expect("failed to create repo dir");
    setup_with_new_libra_in(&repo_dir).await;
    {
        let _guard = ChangeDirGuard::new(&repo_dir);
        ConfigKv::set("remote.origin.url", remote_dir.to_str().unwrap(), false)
            .await
            .unwrap();
    }

    (temp_root, repo_dir, current_branch)
}

/// `--tags` downloads all remote tags into `refs/tags/*`, and a second
/// `--tags` fetch re-downloads nothing (the `have` set now covers tag objects
/// and their targets — the regression that previously forced tag fetch to be
/// backed out).
#[tokio::test]
#[serial]
async fn test_fetch_tags_creates_local_tags_and_is_idempotent() {
    let (_root, repo_dir, _branch) = setup_local_fetch_with_tags_fixture().await;

    let first = run_libra_command(&["--json", "fetch", "origin", "--tags"], &repo_dir);
    assert_cli_success(&first, "fetch origin --tags");
    let first_json = parse_json_stdout(&first);
    assert!(
        first_json["data"]["remotes"][0]["objects_fetched"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "first --tags fetch must download tag objects: {first_json}"
    );

    // Both tags are now local.
    let list = run_libra_command(&["tag", "--list"], &repo_dir);
    assert_cli_success(&list, "tag --list");
    let listed = String::from_utf8_lossy(&list.stdout);
    assert!(
        listed.contains("light-1"),
        "lightweight tag should be local: {listed}"
    );
    assert!(
        listed.contains("annot-1"),
        "annotated tag should be local: {listed}"
    );

    // Second --tags fetch must NOT re-download anything.
    let second = run_libra_command(&["--json", "fetch", "origin", "--tags"], &repo_dir);
    assert_cli_success(&second, "second fetch origin --tags");
    let second_json = parse_json_stdout(&second);
    assert_eq!(
        second_json["data"]["remotes"][0]["objects_fetched"], 0,
        "second --tags fetch must download nothing (no re-download): {second_json}"
    );
    // The real idempotency guarantee is `objects_fetched == 0` (above). The
    // second `--tags` fetch re-advertises the already-present tags, so depending
    // on the transport's up-to-date short-circuit, `git-upload-pack` may still
    // return a minimal *empty* pack (a 12-byte header + a 20-byte SHA-1 / 32-byte
    // SHA-256 trailer). Allow that empty-pack overhead rather than asserting an
    // exact zero, which is environment-dependent on the system Git version.
    let second_bytes = second_json["data"]["remotes"][0]["bytes_received"]
        .as_u64()
        .expect("bytes_received should be a number");
    assert!(
        second_bytes <= 44,
        "second --tags fetch must transfer no real data, only at most an empty pack: {second_json}"
    );
}

/// The default fetch auto-follows tags that point into the fetched history
/// (Git's default): the lightweight tag's commit is fetched, and the annotated
/// tag object arrives via the `include-tag` capability.
#[tokio::test]
#[serial]
async fn test_fetch_default_auto_follows_reachable_tags() {
    let (_root, repo_dir, _branch) = setup_local_fetch_with_tags_fixture().await;

    let default = run_libra_command(&["fetch", "origin"], &repo_dir);
    assert_cli_success(&default, "fetch origin (default auto-follow)");
    let listed = String::from_utf8_lossy(&run_libra_command(&["tag", "--list"], &repo_dir).stdout)
        .into_owned();
    assert!(
        listed.contains("light-1"),
        "default fetch should auto-follow the lightweight tag: {listed}"
    );
    assert!(
        listed.contains("annot-1"),
        "default fetch should auto-follow the annotated tag (via include-tag): {listed}"
    );
}

/// `--no-tags` (and `remote.<name>.tagOpt=--no-tags`) suppresses tag fetching
/// entirely, even tags reachable from fetched commits.
#[tokio::test]
#[serial]
async fn test_fetch_no_tags_skips_even_reachable_tags() {
    let (_root, repo_dir, _branch) = setup_local_fetch_with_tags_fixture().await;

    let no_tags = run_libra_command(&["fetch", "origin", "--no-tags"], &repo_dir);
    assert_cli_success(&no_tags, "fetch origin --no-tags");
    let listed = String::from_utf8_lossy(&run_libra_command(&["tag", "--list"], &repo_dir).stdout)
        .into_owned();
    assert!(
        !listed.contains("light-1") && !listed.contains("annot-1"),
        "--no-tags must not create tags, got: {listed}"
    );

    // `remote.origin.tagOpt=--no-tags` makes the default (flagless) fetch skip
    // tags too. Set it in-process (the value starts with `-`, which the CLI
    // would parse as a flag).
    {
        let _guard = ChangeDirGuard::new(&repo_dir);
        ConfigKv::set("remote.origin.tagOpt", "--no-tags", false)
            .await
            .unwrap();
    }
    let flagless = run_libra_command(&["fetch", "origin"], &repo_dir);
    assert_cli_success(&flagless, "fetch origin (tagOpt=--no-tags)");
    let listed = String::from_utf8_lossy(&run_libra_command(&["tag", "--list"], &repo_dir).stdout)
        .into_owned();
    assert!(
        !listed.contains("light-1") && !listed.contains("annot-1"),
        "remote.origin.tagOpt=--no-tags must skip tags, got: {listed}"
    );
}

/// `--force` overwrites a conflicting local tag; without it the local tag is
/// kept (already covered by `test_fetch_tags_does_not_clobber_existing_local_tag`).
#[tokio::test]
#[serial]
async fn test_fetch_tags_force_clobbers_conflicting_local_tag() {
    let (_root, repo_dir, _branch) = setup_local_fetch_with_tags_fixture().await;

    // A local commit + a local `light-1` colliding with the remote's tag.
    fs::write(repo_dir.join("local.txt"), "local\n").expect("write local file");
    assert_cli_success(
        &run_libra_command(&["add", "local.txt"], &repo_dir),
        "add local file",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "local", "--no-verify"], &repo_dir),
        "commit local file",
    );
    let local_head =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], &repo_dir).stdout)
            .trim()
            .to_string();
    assert_cli_success(
        &run_libra_command(&["tag", "light-1"], &repo_dir),
        "local tag light-1",
    );

    // `--tags --force` clobbers the local tag to the remote's target.
    assert_cli_success(
        &run_libra_command(&["fetch", "origin", "--tags", "--force"], &repo_dir),
        "fetch --tags --force",
    );
    let resolved =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "light-1"], &repo_dir).stdout)
            .trim()
            .to_string();
    assert_ne!(
        resolved, local_head,
        "fetch --tags --force must overwrite the conflicting local tag"
    );
}

/// `remote.<name>.tagOpt=--tags` makes a flagless fetch behave like `--tags`.
#[tokio::test]
#[serial]
async fn test_fetch_tagopt_all_fetches_every_tag() {
    let (_root, repo_dir, _branch) = setup_local_fetch_with_tags_fixture().await;

    {
        let _guard = ChangeDirGuard::new(&repo_dir);
        ConfigKv::set("remote.origin.tagOpt", "--tags", false)
            .await
            .unwrap();
    }
    let flagless = run_libra_command(&["fetch", "origin"], &repo_dir);
    assert_cli_success(&flagless, "fetch origin (tagOpt=--tags)");
    let listed = String::from_utf8_lossy(&run_libra_command(&["tag", "--list"], &repo_dir).stdout)
        .into_owned();
    assert!(
        listed.contains("light-1") && listed.contains("annot-1"),
        "tagOpt=--tags must fetch all tags, got: {listed}"
    );
}

/// `--tags` never clobbers an existing local tag that points elsewhere.
#[tokio::test]
#[serial]
async fn test_fetch_tags_does_not_clobber_existing_local_tag() {
    let (_root, repo_dir, _branch) = setup_local_fetch_with_tags_fixture().await;

    // Create a local commit and a local `light-1` tag that collides with the
    // remote's (which points at a different commit).
    fs::write(repo_dir.join("local.txt"), "local\n").expect("write local file");
    assert_cli_success(
        &run_libra_command(&["add", "local.txt"], &repo_dir),
        "add local file",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "local", "--no-verify"], &repo_dir),
        "commit local file",
    );
    let local_head =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], &repo_dir).stdout)
            .trim()
            .to_string();
    assert_cli_success(
        &run_libra_command(&["tag", "light-1"], &repo_dir),
        "local tag light-1",
    );

    assert_cli_success(
        &run_libra_command(&["fetch", "origin", "--tags"], &repo_dir),
        "fetch origin --tags",
    );

    // The local tag must still resolve to the local commit (not clobbered).
    let resolved =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "light-1"], &repo_dir).stdout)
            .trim()
            .to_string();
    assert_eq!(
        resolved, local_head,
        "fetch --tags must not clobber an existing local tag"
    );
}

/// `--tags --dry-run` previews the new tags without downloading or writing them.
#[tokio::test]
#[serial]
async fn test_fetch_tags_dry_run_previews_without_writing() {
    let (_root, repo_dir, _branch) = setup_local_fetch_with_tags_fixture().await;

    let out = run_libra_command(
        &["--json", "fetch", "origin", "--tags", "--dry-run"],
        &repo_dir,
    );
    assert_cli_success(&out, "fetch origin --tags --dry-run");
    let json = parse_json_stdout(&out);
    assert_eq!(
        json["data"]["remotes"][0]["objects_fetched"], 0,
        "dry-run downloads nothing: {json}"
    );
    let refs = json["data"]["remotes"][0]["refs_updated"]
        .as_array()
        .expect("refs_updated array");
    assert!(
        refs.iter().any(|u| u["remote_ref"] == "refs/tags/light-1"),
        "dry-run must preview the lightweight tag: {json}"
    );
    assert!(
        refs.iter().any(|u| u["remote_ref"] == "refs/tags/annot-1"),
        "dry-run must preview the annotated tag: {json}"
    );

    // Nothing was actually written.
    let listed = String::from_utf8_lossy(&run_libra_command(&["tag", "--list"], &repo_dir).stdout)
        .into_owned();
    assert!(
        !listed.contains("light-1") && !listed.contains("annot-1"),
        "--dry-run must not write tags, got: {listed}"
    );
}

/// `--tags` and `--no-tags` form a last-on-CLI-wins toggle (clap overrides).
#[test]
fn test_fetch_tags_and_no_tags_are_mutually_overriding() {
    use clap::Parser;
    let tags_last = fetch::FetchArgs::try_parse_from(["fetch", "--no-tags", "--tags"]).unwrap();
    assert!(tags_last.tags, "--tags given last must win");
    let no_tags_last = fetch::FetchArgs::try_parse_from(["fetch", "--tags", "--no-tags"]).unwrap();
    assert!(!no_tags_last.tags, "--no-tags given last must win");
}

/// `libra clone` fetches all tags by default (Git parity); `--no-tags` skips
/// them and records `remote.origin.tagOpt=--no-tags`.
#[tokio::test]
#[serial]
async fn test_clone_fetches_all_tags_by_default() {
    let temp_root = tempdir().expect("temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let work_dir = temp_root.path().join("workdir");
    let git_config_global = temp_root.path().join("gitconfig");

    let git = |args: &[&str], cwd: Option<&Path>| {
        let mut cmd = Command::new("git");
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        cmd.env("GIT_CONFIG_GLOBAL", &git_config_global)
            .env("GIT_CONFIG_NOSYSTEM", "1");
        assert!(
            cmd.args(args).status().expect("git failed").success(),
            "git {args:?}"
        );
    };
    git(&["init", "--bare", remote_dir.to_str().unwrap()], None);
    git(&["init", work_dir.to_str().unwrap()], None);
    git(&["config", "user.name", "T"], Some(&work_dir));
    git(&["config", "user.email", "t@e"], Some(&work_dir));
    fs::write(work_dir.join("README.md"), "hi").expect("write");
    git(&["add", "README.md"], Some(&work_dir));
    git(&["commit", "-m", "c"], Some(&work_dir));
    git(&["tag", "-a", "v-annot", "-m", "rel"], Some(&work_dir));
    git(&["tag", "light"], Some(&work_dir));
    let branch = String::from_utf8(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    git(
        &["remote", "add", "origin", remote_dir.to_str().unwrap()],
        Some(&work_dir),
    );
    git(
        &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
        Some(&work_dir),
    );
    git(&["push", "origin", "--tags"], Some(&work_dir));

    // Default clone: tags present.
    let dest = temp_root.path().join("cloned");
    let out = run_libra_command(
        &[
            "clone",
            remote_dir.to_str().unwrap(),
            dest.to_str().unwrap(),
        ],
        temp_root.path(),
    );
    assert_cli_success(&out, "clone (default all-tags)");
    let listed =
        String::from_utf8_lossy(&run_libra_command(&["tag", "--list"], &dest).stdout).into_owned();
    assert!(
        listed.contains("v-annot") && listed.contains("light"),
        "clone must fetch all tags by default, got: {listed}"
    );

    // `--no-tags` clone: no tags.
    let dest2 = temp_root.path().join("cloned_no_tags");
    let out = run_libra_command(
        &[
            "clone",
            "--no-tags",
            remote_dir.to_str().unwrap(),
            dest2.to_str().unwrap(),
        ],
        temp_root.path(),
    );
    assert_cli_success(&out, "clone --no-tags");
    let listed =
        String::from_utf8_lossy(&run_libra_command(&["tag", "--list"], &dest2).stdout).into_owned();
    assert!(
        !listed.contains("v-annot") && !listed.contains("light"),
        "clone --no-tags must not fetch tags, got: {listed}"
    );
}

/// A libra-native remote (LocalClient) serves annotated tag objects, so a
/// libra->libra `fetch --tags` of an annotated tag works end-to-end. This relies
/// on git-internal >= 0.7.6 making the tag id the canonical hash of `to_data()`.
#[tokio::test]
#[serial]
async fn test_fetch_tags_from_libra_native_remote_serves_annotated() {
    let temp_root = tempdir().expect("temp root");
    let remote = temp_root.path().join("libra_remote");
    fs::create_dir_all(&remote).expect("mkdir remote");

    assert_cli_success(&run_libra_command(&["init"], &remote), "init libra remote");
    run_libra_command(&["config", "set", "user.name", "T"], &remote);
    run_libra_command(&["config", "set", "user.email", "t@e"], &remote);
    fs::write(remote.join("f.txt"), "x").expect("write");
    assert_cli_success(&run_libra_command(&["add", "f.txt"], &remote), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c", "--no-verify"], &remote),
        "commit",
    );
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "rel", "v-annot"], &remote),
        "annotated tag on libra remote",
    );

    let local = temp_root.path().join("local");
    fs::create_dir_all(&local).expect("mkdir local");
    assert_cli_success(&run_libra_command(&["init"], &local), "init local");
    {
        let _guard = ChangeDirGuard::new(&local);
        ConfigKv::set("remote.origin.url", remote.to_str().unwrap(), false)
            .await
            .unwrap();
    }
    let fetched = run_libra_command(&["fetch", "origin", "--tags"], &local);
    assert_cli_success(&fetched, "fetch --tags from libra-native remote");
    let listed =
        String::from_utf8_lossy(&run_libra_command(&["tag", "--list"], &local).stdout).into_owned();
    assert!(
        listed.contains("v-annot"),
        "annotated tag must be served + fetched from a libra-native remote: {listed}"
    );
}

#[test]
fn fetch_no_auto_gc_flag_is_accepted() {
    let repo = create_committed_repo_via_cli();
    // `--no-auto-gc` parses and reaches the runtime: with no configured remote
    // it fails at remote resolution, NOT at clap. Libra's fetch never triggers
    // an automatic gc, so the flag is an accepted no-op.
    let output = run_libra_command(&["fetch", "--no-auto-gc"], repo.path());
    assert!(!output.status.success(), "fetch without a remote fails");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "--no-auto-gc is accepted by the parser: {stderr}"
    );
}

#[test]
fn fetch_no_progress_flag_is_accepted() {
    let repo = create_committed_repo_via_cli();
    // `--no-progress` parses and reaches the runtime (suppressing the progress
    // meter is exercised by the unit test `apply_no_progress_*`); with no remote
    // it fails at remote resolution, NOT at clap.
    let output = run_libra_command(&["fetch", "--no-progress"], repo.path());
    assert!(!output.status.success(), "fetch without a remote fails");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "--no-progress is accepted by the parser: {stderr}"
    );
}

#[test]
fn fetch_no_prune_flag_is_accepted() {
    let repo = create_committed_repo_via_cli();
    // `--no-prune` parses and reaches the runtime: with no configured remote it
    // fails at remote resolution, NOT at clap. Libra's fetch never prunes
    // remote-tracking refs, so the flag is an accepted no-op.
    let output = run_libra_command(&["fetch", "--no-prune"], repo.path());
    assert!(!output.status.success(), "fetch without a remote fails");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument"),
        "--no-prune is accepted by the parser: {stderr}"
    );
}

/// Build a bare git remote carrying `main` plus `feature1/2/3`, and a Libra repo
/// that has fetched all of them (so `refs/remotes/origin/*` tracking refs
/// exist). Returns `(temp_root, repo_dir, default_branch, cwd_guard)`; the guard
/// keeps the process CWD pointed at the Libra repo for in-process fetch calls
/// and must be held for the duration of the test.
async fn setup_multi_branch_remote_and_fetch() -> (TempDir, PathBuf, String, ChangeDirGuard) {
    let temp_root = tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let work_dir = temp_root.path().join("workdir");
    let repo_dir = temp_root.path().join("libra_repo");

    let git = |args: &[&str], cwd: Option<&Path>| {
        let mut cmd = Command::new("git");
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        assert!(
            cmd.args(args)
                .status()
                .expect("git command failed")
                .success(),
            "git {args:?} failed"
        );
    };

    git(&["init", "--bare", remote_dir.to_str().unwrap()], None);
    git(&["init", work_dir.to_str().unwrap()], None);
    git(&["config", "user.name", "Libra Tester"], Some(&work_dir));
    git(
        &["config", "user.email", "tester@example.com"],
        Some(&work_dir),
    );
    fs::write(work_dir.join("README.md"), "hello libra").expect("failed to write README");
    git(&["add", "README.md"], Some(&work_dir));
    git(&["commit", "-m", "initial commit"], Some(&work_dir));

    let default_branch = String::from_utf8(
        Command::new("git")
            .current_dir(&work_dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .expect("failed to read current branch")
            .stdout,
    )
    .expect("branch name not utf8")
    .trim()
    .to_string();

    git(
        &["remote", "add", "origin", remote_dir.to_str().unwrap()],
        Some(&work_dir),
    );
    git(
        &[
            "push",
            "origin",
            &format!("HEAD:refs/heads/{default_branch}"),
        ],
        Some(&work_dir),
    );
    for branch in ["feature1", "feature2", "feature3"] {
        git(&["checkout", "-b", branch], Some(&work_dir));
        git(&["push", "origin", branch], Some(&work_dir));
    }

    fs::create_dir_all(&repo_dir).expect("failed to create repo dir");
    setup_with_new_libra_in(&repo_dir).await;
    let guard = ChangeDirGuard::new(&repo_dir);
    let remote_path = remote_dir.to_str().unwrap().to_string();
    ConfigKv::set("remote.origin.url", &remote_path, false)
        .await
        .unwrap();

    // Initial fetch establishes the `refs/remotes/origin/*` tracking refs.
    fetch::fetch_repository(
        RemoteConfig {
            name: "origin".to_string(),
            url: remote_path,
        },
        None,
        false,
        None,
    )
    .await;

    (temp_root, repo_dir, default_branch, guard)
}

/// Construct a minimal local `FetchArgs` for the given repository, toggling only
/// `--prune` / `--dry-run`. Progress is suppressed to keep test output quiet.
fn local_fetch_args(repository: &str, prune: bool, dry_run: bool) -> fetch::FetchArgs {
    fetch::FetchArgs {
        repository: Some(repository.to_string()),
        refspec: None,
        all: false,
        depth: None,
        dry_run,
        append: false,
        verbose: false,
        porcelain: false,
        force: false,
        tags: false,
        no_tags: false,
        no_auto_gc: false,
        no_progress: true,
        prune,
        no_prune: false,
        notes: false,
    }
}

async fn origin_tracking_ref_exists(branch: &str) -> bool {
    Branch::find_branch_result(&format!("refs/remotes/origin/{branch}"), Some("origin"))
        .await
        .expect("failed to query remote-tracking branch")
        .is_some()
}

/// `fetch --prune` removes `refs/remotes/origin/*` refs the remote no longer
/// advertises, while leaving live tracking refs intact.
#[tokio::test]
#[serial]
async fn test_fetch_prune_removes_stale_tracking_refs() {
    let (temp_root, _repo_dir, default_branch, _guard) =
        setup_multi_branch_remote_and_fetch().await;
    let remote_dir = temp_root.path().join("remote.git");

    for branch in ["feature1", "feature2", "feature3"] {
        assert!(
            origin_tracking_ref_exists(branch).await,
            "{branch} should be tracked after the initial fetch"
        );
    }

    // Remove two branches on the remote, then prune.
    for branch in ["feature1", "feature3"] {
        assert!(
            Command::new("git")
                .current_dir(&remote_dir)
                .args(["update-ref", "-d", &format!("refs/heads/{branch}")])
                .status()
                .expect("git update-ref -d failed")
                .success()
        );
    }

    fetch::execute_safe(
        local_fetch_args("origin", true, false),
        &OutputConfig::default(),
    )
    .await
    .expect("fetch --prune should succeed");

    assert!(
        !origin_tracking_ref_exists("feature1").await,
        "stale feature1 tracking ref should be pruned"
    );
    assert!(
        !origin_tracking_ref_exists("feature3").await,
        "stale feature3 tracking ref should be pruned"
    );
    assert!(
        origin_tracking_ref_exists("feature2").await,
        "live feature2 tracking ref must be kept"
    );
    assert!(
        origin_tracking_ref_exists(&default_branch).await,
        "live default-branch tracking ref must be kept"
    );
}

/// `fetch --dry-run --prune` reports stale refs but must not delete them; a real
/// `fetch --prune` afterwards removes them.
#[tokio::test]
#[serial]
async fn test_fetch_prune_dry_run_previews_without_deleting() {
    let (temp_root, _repo_dir, _default_branch, _guard) =
        setup_multi_branch_remote_and_fetch().await;
    let remote_dir = temp_root.path().join("remote.git");

    assert!(
        Command::new("git")
            .current_dir(&remote_dir)
            .args(["update-ref", "-d", "refs/heads/feature1"])
            .status()
            .expect("git update-ref -d failed")
            .success()
    );

    // Dry-run prune must not write anything.
    fetch::execute_safe(
        local_fetch_args("origin", true, true),
        &OutputConfig::default(),
    )
    .await
    .expect("fetch --dry-run --prune should succeed");
    assert!(
        origin_tracking_ref_exists("feature1").await,
        "dry-run prune must not delete the stale tracking ref"
    );

    // A real prune then removes it.
    fetch::execute_safe(
        local_fetch_args("origin", true, false),
        &OutputConfig::default(),
    )
    .await
    .expect("fetch --prune should succeed");
    assert!(
        !origin_tracking_ref_exists("feature1").await,
        "a real prune removes the stale tracking ref"
    );
}

/// `-p` / `--prune` are accepted by the parser, and `--prune --no-prune` form a
/// last-one-wins toggle (clap `overrides_with`) rather than a hard conflict.
#[test]
fn test_fetch_prune_flag_and_toggle_parse() {
    let repo = init_temp_repo();
    for flag in ["-p", "--prune"] {
        let output = run_libra_command(&["fetch", flag], repo.path());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("unexpected argument") && !stderr.contains("unrecognized"),
            "fetch {flag} should be accepted by the parser: {stderr}"
        );
    }
    let output = run_libra_command(&["fetch", "--prune", "--no-prune"], repo.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument") && !stderr.contains("cannot be used with"),
        "--prune and --no-prune should form a last-wins toggle, not a hard conflict: {stderr}"
    );
}
