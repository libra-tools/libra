//! Error code validation tests for `libra status`.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::fs;

use libra::internal::{db::get_db_conn_instance, model::reference};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serial_test::serial;
use tempfile::tempdir;

use super::{
    ChangeDirGuard, assert_cli_success, configure_identity_via_cli, create_committed_repo_via_cli,
    init_repo_via_cli, parse_cli_error_stderr, run_libra_command,
};

// ---------------------------------------------------------------------------
// Outside repository → LBR-REPO-001
// ---------------------------------------------------------------------------

#[test]
fn status_outside_repo_returns_repo_not_found() {
    let temp = tempdir().unwrap();

    let output = run_libra_command(&["status"], temp.path());
    assert_eq!(output.status.code(), Some(128));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not a libra repository"));
}

// ---------------------------------------------------------------------------
// Corrupt index → LBR-REPO-002
// ---------------------------------------------------------------------------

#[test]
fn status_corrupt_index_returns_repo_corrupt() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    // Corrupt the index file
    fs::write(repo.path().join(".libra").join("index"), b"corrupted").unwrap();

    let output = run_libra_command(&["status"], repo.path());
    assert_eq!(output.status.code(), Some(128));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal:"),
        "stderr should contain fatal prefix: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// --json mode: errors on stderr, stdout clean
// ---------------------------------------------------------------------------

#[test]
fn status_json_error_keeps_stdout_clean() {
    let temp = tempdir().unwrap();

    let output = run_libra_command(&["--json", "status"], temp.path());
    assert!(!output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    // stdout should be clean (no JSON output on success path)
    assert!(
        stdout.trim().is_empty(),
        "stdout should be clean on error: {stdout}"
    );

    // stderr should contain the error
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.trim().is_empty(), "stderr should have error info");
}

#[tokio::test]
#[serial(cwd)]
async fn status_corrupt_head_reference_returns_repo_corrupt() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    let db = get_db_conn_instance().await;
    let head = reference::Entity::find()
        .filter(reference::Column::Kind.eq(reference::ConfigKind::Head))
        .filter(reference::Column::Remote.is_null())
        .one(&db)
        .await
        .unwrap()
        .expect("expected HEAD row");
    let mut head: reference::ActiveModel = head.into();
    head.name = Set(None);
    head.commit = Set(Some("not-a-valid-hash".to_string()));
    head.update(&db).await.unwrap();

    let output = run_libra_command(&["status"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert!(
        stderr.contains("failed to resolve HEAD"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        stderr.contains("invalid detached HEAD commit hash"),
        "unexpected stderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// --exit-code: dirty → exit 1
// ---------------------------------------------------------------------------

#[test]
fn status_exit_code_dirty_returns_1() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());

    fs::write(repo.path().join("a.txt"), "a").unwrap();
    let out = run_libra_command(&["add", ".libraignore", "a.txt"], repo.path());
    assert_cli_success(&out, "add a.txt");
    let out = run_libra_command(&["commit", "-m", "init", "--no-verify"], repo.path());
    assert_cli_success(&out, "initial commit");

    // Modify tracked file → dirty
    fs::write(repo.path().join("a.txt"), "modified").unwrap();

    let output = run_libra_command(&["status", "--exit-code"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(1),
        "dirty repo with --exit-code should exit 1"
    );
}

// ---------------------------------------------------------------------------
// --exit-code: clean → exit 0
// ---------------------------------------------------------------------------

#[test]
fn status_exit_code_clean_returns_0() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());

    fs::write(repo.path().join("a.txt"), "a").unwrap();
    let out = run_libra_command(&["add", ".libraignore", "a.txt"], repo.path());
    assert_cli_success(&out, "add a.txt");
    let out = run_libra_command(&["commit", "-m", "init", "--no-verify"], repo.path());
    assert_cli_success(&out, "initial commit");

    let output = run_libra_command(&["status", "--exit-code"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(0),
        "clean repo with --exit-code should exit 0"
    );
}

// ---------------------------------------------------------------------------
// --exit-code --quiet: dirty → exit 1, no output
// ---------------------------------------------------------------------------

#[test]
fn status_exit_code_quiet_dirty_silent_exit_1() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());

    fs::write(repo.path().join("a.txt"), "a").unwrap();
    let out = run_libra_command(&["add", ".libraignore", "a.txt"], repo.path());
    assert_cli_success(&out, "add a.txt");
    let out = run_libra_command(&["commit", "-m", "init", "--no-verify"], repo.path());
    assert_cli_success(&out, "initial commit");

    fs::write(repo.path().join("a.txt"), "modified").unwrap();

    let output = run_libra_command(&["--quiet", "status", "--exit-code"], repo.path());
    assert_eq!(output.status.code(), Some(1));

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.trim().is_empty(), "quiet should suppress stdout");
}

// ---------------------------------------------------------------------------
// --quiet without --exit-code: always exit 0
// ---------------------------------------------------------------------------

#[test]
fn status_quiet_without_exit_code_always_0() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());

    fs::write(repo.path().join("a.txt"), "a").unwrap();
    let out = run_libra_command(&["add", ".libraignore", "a.txt"], repo.path());
    assert_cli_success(&out, "add a.txt");
    let out = run_libra_command(&["commit", "-m", "init", "--no-verify"], repo.path());
    assert_cli_success(&out, "initial commit");

    fs::write(repo.path().join("a.txt"), "modified").unwrap();

    let output = run_libra_command(&["--quiet", "status"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(0),
        "quiet without --exit-code should always exit 0"
    );
}

// ---------------------------------------------------------------------------
// --exit-code respects --untracked-files=no filter
// ---------------------------------------------------------------------------

#[test]
fn status_exit_code_untracked_only_with_filter_is_clean() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());

    fs::write(repo.path().join("a.txt"), "a").unwrap();
    let out = run_libra_command(&["add", ".libraignore", "a.txt"], repo.path());
    assert_cli_success(&out, "add a.txt");
    let out = run_libra_command(&["commit", "-m", "init", "--no-verify"], repo.path());
    assert_cli_success(&out, "initial commit");

    // Only untracked file, no tracked modifications
    fs::write(repo.path().join("untracked.txt"), "new").unwrap();

    let output = run_libra_command(
        &["status", "--exit-code", "--untracked-files=no"],
        repo.path(),
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "only untracked files with --untracked-files=no should be clean"
    );
}

// ---------------------------------------------------------------------------
// --exit-code --json: JSON output + exit 1
// ---------------------------------------------------------------------------

#[test]
fn status_exit_code_json_dirty_returns_json_and_exit_1() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());

    fs::write(repo.path().join("a.txt"), "a").unwrap();
    let out = run_libra_command(&["add", ".libraignore", "a.txt"], repo.path());
    assert_cli_success(&out, "add a.txt");
    let out = run_libra_command(&["commit", "-m", "init", "--no-verify"], repo.path());
    assert_cli_success(&out, "initial commit");

    fs::write(repo.path().join("a.txt"), "modified").unwrap();

    let output = run_libra_command(&["--json", "status", "--exit-code"], repo.path());
    assert_eq!(output.status.code(), Some(1));

    // JSON should still be valid on stdout
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON, got: {stdout}\nerror: {e}"));
    assert_eq!(
        parsed["ok"], true,
        "JSON ok should be true even with exit 1"
    );
    assert_eq!(parsed["data"]["is_clean"], false);
}
