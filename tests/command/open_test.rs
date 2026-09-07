//! Tests open command integration to ensure it finds remote correctly.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use libra::{
    command::{
        open,
        remote::{self, RemoteCmds},
    },
    utils::{error::StableErrorCode, output::OutputConfig, test},
};
use serial_test::serial;
use tempfile::tempdir;

use super::*;

#[tokio::test]
#[serial(cwd)]
async fn test_open_remote_origin() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    let output = OutputConfig {
        quiet: true,
        ..OutputConfig::default()
    };

    // Add origin remote
    remote::execute_safe(
        RemoteCmds::Add {
            name: "origin".into(),
            url: "git@github.com:libra-tools/libra.git".into(),
            fetch: false,
            track: vec![],
            master: None,
            tags: false,
            no_tags: false,
            mirror: false,
        },
        &output,
    )
    .await
    .expect("adding origin remote should succeed");

    // Test explicit remote
    open::execute_safe(
        open::OpenArgs {
            remote: Some("origin".to_string()),
        },
        &output,
    )
    .await
    .expect("opening explicit origin remote should succeed");

    // Test default remote should find origin
    open::execute_safe(open::OpenArgs { remote: None }, &output)
        .await
        .expect("opening default remote should succeed");

    let error = open::execute_safe(
        open::OpenArgs {
            remote: Some("nonexistent".to_string()),
        },
        &output,
    )
    .await
    .expect_err("invalid direct remote target should return a CLI error");
    assert_eq!(error.stable_code(), StableErrorCode::CliInvalidTarget);
    assert_eq!(error.exit_code(), 129);
    assert!(
        error.message().contains("unsafe or invalid"),
        "unexpected error message: {}",
        error.message()
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_open_no_remote() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    let output = OutputConfig {
        quiet: true,
        ..OutputConfig::default()
    };

    let error = open::execute_safe(open::OpenArgs { remote: None }, &output)
        .await
        .expect_err("opening without a configured remote should fail");
    assert_eq!(error.stable_code(), StableErrorCode::RepoStateInvalid);
    assert_eq!(error.exit_code(), 128);
    assert!(
        error.message().contains("no remote configured"),
        "unexpected error message: {}",
        error.message()
    );
    assert!(
        error
            .hints()
            .iter()
            .any(|hint| hint.as_str().contains("libra remote add origin")),
        "expected add-remote hint, got {:?}",
        error.hints()
    );
}

#[test]
fn test_open_json_output_uses_origin_remote() {
    let repo = create_committed_repo_via_cli();

    let add_remote = run_libra_command(
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:libra-tools/libra.git",
        ],
        repo.path(),
    );
    assert_cli_success(&add_remote, "failed to add origin for open test");

    let output = run_libra_command(&["open", "--json"], repo.path());

    assert_cli_success(&output, "open --json should succeed");
    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "open");
    assert_eq!(json["data"]["remote"], "origin");
    assert_eq!(
        json["data"]["web_url"],
        "https://github.com/libra-tools/libra"
    );
    assert_eq!(json["data"]["launched"], false);
}

#[cfg(not(windows))]
#[test]
fn test_open_json_output_does_not_require_browser_launcher() {
    let repo = create_committed_repo_via_cli();

    let add_remote = run_libra_command(
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:libra-tools/libra.git",
        ],
        repo.path(),
    );
    assert_cli_success(
        &add_remote,
        "failed to add origin for browser-launch bypass test",
    );

    let output = base_libra_command(&["open", "--json"], repo.path())
        .env_remove(LIBRA_TEST_ENV)
        .env("PATH", repo.path())
        .output()
        .expect("failed to execute open --json without browser launcher");

    assert_cli_success(
        &output,
        "open --json should not require a browser launcher in automation",
    );
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["remote"], "origin");
    assert_eq!(json["data"]["launched"], false);
}

#[test]
fn test_open_json_output_falls_back_to_origin_when_head_is_detached() {
    let repo = create_committed_repo_via_cli();

    let add_remote = run_libra_command(
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:libra-tools/libra.git",
        ],
        repo.path(),
    );
    assert_cli_success(
        &add_remote,
        "failed to add origin for detached-head open test",
    );

    let log_out = run_libra_command(&["log"], repo.path());
    let stdout = String::from_utf8_lossy(&log_out.stdout);
    let hash = stdout
        .lines()
        .find(|line| line.starts_with("commit "))
        .and_then(|line| line.strip_prefix("commit "))
        .map(str::trim)
        .expect("expected commit hash in log output");

    let switch_out = run_libra_command(&["switch", "--detach", hash], repo.path());
    assert_cli_success(
        &switch_out,
        "failed to detach HEAD before running open --json",
    );

    let output = run_libra_command(&["open", "--json"], repo.path());
    assert_cli_success(
        &output,
        "open --json should fall back to origin on detached HEAD",
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["remote"], "origin");
    assert_eq!(
        json["data"]["web_url"],
        "https://github.com/libra-tools/libra"
    );
    assert_eq!(json["data"]["launched"], false);
}

#[test]
fn test_open_without_remote_reports_stable_error() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["open"], repo.path());

    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-003");
    assert!(
        report
            .hints
            .iter()
            .any(|hint| hint.contains("libra remote add origin")),
        "expected hint to mention adding a remote, got {:?}",
        report.hints
    );
}

#[test]
fn test_open_json_output_transforms_explicit_ssh_url() {
    let temp = tempdir().unwrap();

    let output = run_libra_command(
        &[
            "open",
            "--json",
            "ssh://git@github.com/libra-tools/libra.git",
        ],
        temp.path(),
    );

    assert_cli_success(&output, "open --json with explicit ssh URL should succeed");
    let json = parse_json_stdout(&output);
    assert!(json["data"]["remote"].is_null());
    assert_eq!(
        json["data"]["remote_url"],
        "ssh://git@github.com/libra-tools/libra.git"
    );
    assert_eq!(
        json["data"]["web_url"],
        "https://github.com/libra-tools/libra"
    );
    assert_eq!(json["data"]["launched"], false);
}

#[test]
fn test_open_json_output_keeps_explicit_https_url() {
    let temp = tempdir().unwrap();

    let output = run_libra_command(
        &["open", "--json", "https://github.com/libra-tools/libra.git"],
        temp.path(),
    );

    assert_cli_success(
        &output,
        "open --json with explicit https URL should succeed",
    );
    let json = parse_json_stdout(&output);
    assert!(json["data"]["remote"].is_null());
    assert_eq!(
        json["data"]["remote_url"],
        "https://github.com/libra-tools/libra.git"
    );
    assert_eq!(
        json["data"]["web_url"],
        "https://github.com/libra-tools/libra"
    );
    assert_eq!(json["data"]["launched"], false);
}
