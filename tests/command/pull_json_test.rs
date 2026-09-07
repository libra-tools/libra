//! JSON and machine output tests for the pull command.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serial_test::serial;
use tempfile::{TempDir, tempdir};

use super::{assert_cli_success, configure_identity_via_cli, init_repo_via_cli, run_libra_command};

fn git(args: &[&str], cwd: &Path) {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("failed to execute git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(args: &[&str], cwd: &Path) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("failed to execute git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output should be utf8")
        .trim()
        .to_string()
}

fn create_remote_fixture() -> (TempDir, PathBuf, PathBuf, String) {
    let temp_root = tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let work_dir = temp_root.path().join("workdir");

    git(
        &["init", "--bare", remote_dir.to_str().unwrap()],
        temp_root.path(),
    );
    git(&["init", work_dir.to_str().unwrap()], temp_root.path());
    git(&["config", "user.name", "Libra Tester"], &work_dir);
    git(&["config", "user.email", "tester@example.com"], &work_dir);

    fs::write(work_dir.join("README.md"), "hello libra\n").expect("failed to write README");
    git(&["add", "README.md"], &work_dir);
    git(&["commit", "-m", "initial commit"], &work_dir);

    let branch = git_stdout(&["rev-parse", "--abbrev-ref", "HEAD"], &work_dir);
    git(
        &["remote", "add", "origin", remote_dir.to_str().unwrap()],
        &work_dir,
    );
    git(
        &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
        &work_dir,
    );

    (temp_root, remote_dir, work_dir, branch)
}

fn configure_pull_tracking(repo: &Path, remote_dir: &Path, branch: &str) {
    let remote_output = run_libra_command(
        &["remote", "add", "origin", remote_dir.to_str().unwrap()],
        repo,
    );
    assert_cli_success(&remote_output, "remote add");

    let branch_remote = run_libra_command(&["config", "branch.main.remote", "origin"], repo);
    assert_cli_success(&branch_remote, "set branch.main.remote");

    let merge_ref = format!("refs/heads/{branch}");
    let branch_merge = run_libra_command(&["config", "branch.main.merge", &merge_ref], repo);
    assert_cli_success(&branch_merge, "set branch.main.merge");
}

fn push_remote_commit(
    work_dir: &Path,
    branch: &str,
    file: &str,
    content: &str,
    message: &str,
) -> (String, String) {
    let previous = git_stdout(&["rev-parse", "HEAD"], work_dir);
    fs::write(work_dir.join(file), content).expect("failed to write remote file");
    git(&["add", file], work_dir);
    git(&["commit", "-m", message], work_dir);
    git(
        &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
        work_dir,
    );
    let current = git_stdout(&["rev-parse", "HEAD"], work_dir);
    (previous, current)
}

#[test]
#[serial(env)]
fn json_pull_fast_forward_returns_structured_data() {
    let (_temp_root, remote_dir, _work_dir, branch) = create_remote_fixture();

    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);

    let output = run_libra_command(&["--json", "pull"], local_repo.path());
    assert_cli_success(&output, "json pull");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON on stdout, got: {stdout}\nerror: {e}"));
    let data = &parsed["data"];

    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["command"], "pull");
    assert_eq!(data["branch"], "main");
    assert_eq!(data["upstream"], format!("origin/{branch}"));
    assert!(data["fetch"]["remote"].is_string());
    assert!(data["fetch"]["url"].is_string());
    assert!(data["fetch"]["refs_updated"].is_array());
    assert!(data["fetch"]["objects_fetched"].is_number());
    assert!(data["fetch"]["bytes_received"].is_number());
    assert!(data["merge"]["old_commit"].is_null());
    assert_eq!(data["merge"]["strategy"], "fast-forward");
    assert!(data["merge"]["commit"].is_string());
    assert!(data["merge"]["files_changed"].is_number());
    assert_eq!(data["merge"]["up_to_date"], false);
    assert!(
        output.stderr.is_empty(),
        "json pull success should keep stderr clean, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[serial(env)]
fn json_pull_already_up_to_date_returns_structured_data() {
    let (_temp_root, remote_dir, _work_dir, branch) = create_remote_fixture();

    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);

    let first_pull = run_libra_command(&["pull"], local_repo.path());
    assert_cli_success(&first_pull, "initial pull");

    let output = run_libra_command(&["--json", "pull"], local_repo.path());
    assert_cli_success(&output, "json up-to-date pull");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON on stdout, got: {stdout}\nerror: {e}"));
    let data = &parsed["data"];

    assert_eq!(data["merge"]["strategy"], "already-up-to-date");
    assert!(data["merge"]["old_commit"].is_string());
    assert_eq!(data["merge"]["commit"], serde_json::Value::Null);
    assert_eq!(data["merge"]["files_changed"], 0);
    assert_eq!(data["merge"]["up_to_date"], true);
    assert_eq!(data["fetch"]["refs_updated"], serde_json::json!([]));
    assert!(
        output.stderr.is_empty(),
        "json pull success should keep stderr clean, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[serial(env)]
fn machine_pull_emits_single_json_line() {
    let (_temp_root, remote_dir, _work_dir, branch) = create_remote_fixture();

    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);

    let output = run_libra_command(&["--machine", "pull"], local_repo.path());
    assert_cli_success(&output, "machine pull");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "machine pull should emit exactly one JSON line"
    );
    let parsed: serde_json::Value = serde_json::from_str(lines[0])
        .unwrap_or_else(|e| panic!("expected machine JSON line, got: {}\nerror: {e}", lines[0]));
    assert_eq!(parsed["command"], "pull");
    assert!(
        output.stderr.is_empty(),
        "machine pull success should keep stderr clean, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[serial(env)]
fn json_pull_follow_up_fast_forward_reports_old_and_new_commits() {
    let (_temp_root, remote_dir, work_dir, branch) = create_remote_fixture();

    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);

    let first_pull = run_libra_command(&["pull"], local_repo.path());
    assert_cli_success(&first_pull, "initial pull");

    let (previous, current) = push_remote_commit(
        &work_dir,
        &branch,
        "follow-up.txt",
        "follow-up change\n",
        "remote follow-up",
    );

    let output = run_libra_command(&["--json", "pull"], local_repo.path());
    assert_cli_success(&output, "follow-up json pull");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON on stdout, got: {stdout}\nerror: {e}"));
    let data = &parsed["data"];

    assert_eq!(data["merge"]["strategy"], "fast-forward");
    assert_eq!(data["merge"]["old_commit"], previous);
    assert_eq!(data["merge"]["commit"], current);
    assert_eq!(data["merge"]["up_to_date"], false);
}
