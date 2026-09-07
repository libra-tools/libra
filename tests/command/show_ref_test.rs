//! Integration tests for `show-ref` command.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::{fs, io::Write, process::Command};

use libra::internal::{
    branch::Branch, config::ConfigKv, db::get_db_conn_instance, model::reference,
};
use sea_orm::{ActiveModelTrait, Set};
use serial_test::serial;
use tempfile::tempdir;

use super::*;

/// Create a repo, add a file and commit with the given message.
async fn setup_repo_with_commit(temp: &tempfile::TempDir) -> ChangeDirGuard {
    test::setup_with_new_libra_in(temp.path()).await;
    let guard = ChangeDirGuard::new(temp.path());

    let mut f = fs::File::create("a.txt").unwrap();
    writeln!(f, "hello").unwrap();

    add::execute(AddArgs {
        pathspec: vec!["a.txt".into()],
        all: false,
        update: false,
        refresh: false,
        force: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    commit::execute(CommitArgs {
        message: Some("initial".into()),
        ..Default::default()
    })
    .await;

    guard
}

/// show-ref on an "empty" repo (initialized but no user commits) should list the AI branch.
#[tokio::test]
#[serial(env, cwd)]
async fn test_show_ref_empty_repo() {
    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp.path())
        .arg("show-ref")
        .output()
        .expect("failed to execute `libra show-ref`");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("refs/heads/libra/intent"),
        "expected NO refs/heads/libra/intent in output, got: {stdout}"
    );
    // If no refs exist, show-ref might return non-zero exit code, so we don't assert success here for empty repo
}

/// show-ref should list refs/heads/<branch> after a commit.
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_lists_branch() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;

    let head_commit = Head::current_commit().await.unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp.path())
        .arg("show-ref")
        .arg("--heads")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("refs/heads/main"),
        "expected refs/heads/main in output, got: {stdout}"
    );
    assert!(
        stdout.contains(&head_commit.to_string()),
        "expected commit hash in output, got: {stdout}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_json_lists_refs() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;

    let output = run_libra_command(&["show-ref", "--json", "--head", "--heads"], temp.path());
    assert_cli_success(&output, "show-ref --json should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "show-ref");
    assert_eq!(json["data"]["hash_only"], false);
    let entries = json["data"]["entries"]
        .as_array()
        .expect("entries should be an array");
    assert!(
        entries.iter().any(|entry| entry["refname"] == "HEAD"),
        "expected HEAD entry in JSON output"
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry["refname"] == "refs/heads/main"),
        "expected branch entry in JSON output"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_verify_exact_ref_outputs_matching_ref() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;
    let head_hash = Head::current_commit().await.unwrap().to_string();

    let output = run_libra_command(&["show-ref", "--verify", "refs/heads/main"], temp.path());
    assert_cli_success(&output, "show-ref --verify refs/heads/main");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("{head_hash} refs/heads/main")),
        "verify should print the exact ref entry: {stdout}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_verify_head_accepts_head_refname() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;
    let head_hash = Head::current_commit().await.unwrap().to_string();

    let output = run_libra_command(&["show-ref", "--verify", "HEAD"], temp.path());
    assert_cli_success(&output, "show-ref --verify HEAD");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("{head_hash} HEAD")),
        "verify should print the HEAD entry: {stdout}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_verify_short_name_is_not_exact_ref() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;

    let output = run_libra_command(&["show-ref", "--verify", "main"], temp.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert_eq!(report.exit_code, 128);
    assert!(
        stderr.contains("'main' - not a valid ref"),
        "verify should reject non-exact refnames: {stderr}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_exists_success_is_silent_in_human_mode() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;

    let output = run_libra_command(&["show-ref", "--exists", "refs/heads/main"], temp.path());
    assert_cli_success(&output, "show-ref --exists refs/heads/main");
    assert!(
        output.stdout.is_empty(),
        "exists should not print success output"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_exists_missing_ref_uses_git_exit_code_two() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;

    let output = run_libra_command(&["show-ref", "--exists", "refs/heads/missing"], temp.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert_eq!(report.exit_code, 2);
    assert!(
        stderr.contains("reference does not exist: refs/heads/missing"),
        "exists should report missing ref: {stderr}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_exists_json_reports_checked_ref() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;

    let output = run_libra_command(
        &["--json", "show-ref", "--exists", "refs/heads/main"],
        temp.path(),
    );
    assert_cli_success(&output, "show-ref --exists --json");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "show-ref");
    assert_eq!(json["data"]["exists"], true);
    assert_eq!(json["data"]["refname"], "refs/heads/main");
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_lists_remote_tracking_refs() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;
    let head_hash = Head::current_commit().await.unwrap().to_string();
    ConfigKv::set("remote.origin.url", "https://example.com/repo.git", false)
        .await
        .unwrap();
    Branch::update_branch("refs/remotes/origin/main", &head_hash, Some("origin"))
        .await
        .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp.path())
        .arg("show-ref")
        .arg("--heads")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("refs/remotes/origin/main"),
        "expected remote-tracking ref in output, got: {stdout}"
    );
    assert!(
        stdout.contains(&head_hash),
        "expected remote-tracking commit hash in output, got: {stdout}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_json_lists_remote_tracking_refs() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;
    let head_hash = Head::current_commit().await.unwrap().to_string();
    ConfigKv::set("remote.origin.url", "https://example.com/repo.git", false)
        .await
        .unwrap();
    Branch::update_branch("refs/remotes/origin/main", &head_hash, Some("origin"))
        .await
        .unwrap();

    let output = run_libra_command(&["show-ref", "--json", "--heads"], temp.path());
    assert_cli_success(&output, "show-ref --json --heads should succeed");

    let json = parse_json_stdout(&output);
    let entries = json["data"]["entries"]
        .as_array()
        .expect("entries should be an array");
    assert!(
        entries.iter().any(|entry| {
            entry["refname"] == "refs/remotes/origin/main" && entry["hash"] == head_hash
        }),
        "expected remote-tracking entry in JSON output: {json}",
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_surfaces_corrupt_branch_storage() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;
    let db = get_db_conn_instance().await;
    reference::ActiveModel {
        name: Set(Some("broken".to_string())),
        kind: Set(reference::ConfigKind::Branch),
        commit: Set(Some("not-a-valid-hash".to_string())),
        remote: Set(None),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp.path())
        .arg("show-ref")
        .arg("--heads")
        .output()
        .unwrap();

    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert!(
        stderr.contains("stored branch reference 'broken' is corrupt"),
        "unexpected stderr: {stderr}"
    );
}

/// show-ref --tags should list tags after creating one.
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_lists_tag() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;

    // Create a lightweight tag via the internal API (same pattern as tag_test.rs)
    libra::internal::tag::create("v1.0", None, false, false)
        .await
        .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp.path())
        .arg("show-ref")
        .arg("--tags")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("refs/tags/v1.0"),
        "expected refs/tags/v1.0 in output, got: {stdout}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_surfaces_corrupt_tag_storage() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;
    let db = get_db_conn_instance().await;
    reference::ActiveModel {
        name: Set(Some("refs/tags/broken".to_string())),
        kind: Set(reference::ConfigKind::Tag),
        commit: Set(Some("not-a-valid-hash".to_string())),
        remote: Set(None),
        ..Default::default()
    }
    .insert(&db)
    .await
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp.path())
        .arg("show-ref")
        .arg("--tags")
        .output()
        .unwrap();

    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert!(
        stderr.contains("failed to list tags"),
        "unexpected stderr: {stderr}"
    );
}

/// show-ref --head should include HEAD.
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_includes_head() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;

    let head_commit = Head::current_commit().await.unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp.path())
        .arg("show-ref")
        .arg("--head")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    // First line should be HEAD
    let first_line = stdout.lines().next().unwrap_or("");
    assert!(
        first_line.contains("HEAD"),
        "expected HEAD in first line, got: {first_line}"
    );
    assert!(
        first_line.contains(&head_commit.to_string()),
        "expected commit hash in HEAD line, got: {first_line}"
    );
}

/// show-ref --hash should output only hashes (no ref names).
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_hash_only() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;

    let head_commit = Head::current_commit().await.unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp.path())
        .arg("show-ref")
        .arg("--hash")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&head_commit.to_string()),
        "expected hash {}, got: {stdout}",
        head_commit
    );
    assert!(
        !stdout.contains("refs/"),
        "hash-only mode should not contain ref names"
    );
}

/// show-ref with a non-matching pattern should error.
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_pattern_no_match() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp.path())
        .arg("show-ref")
        .arg("nonexistent-xyz")
        .output()
        .unwrap();

    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        stderr.contains("no matching refs found"),
        "expected error for non-matching pattern, got stderr: {stderr}"
    );
}

/// show-ref with a matching pattern should filter results.
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_pattern_match() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;

    // Create a second branch to verify filtering
    let head_hash = Head::current_commit().await.unwrap().to_string();
    Branch::update_branch("feature", &head_hash, None)
        .await
        .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp.path())
        .arg("show-ref")
        .arg("--heads")
        .arg("main")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("refs/heads/main"),
        "pattern should match main"
    );
    assert!(
        !stdout.contains("refs/heads/feature"),
        "pattern should NOT match feature"
    );
}

/// show-ref default (no flags) should show both branches and tags.
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_default_shows_both() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;

    libra::internal::tag::create("v2.0", None, false, false)
        .await
        .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp.path())
        .arg("show-ref")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("refs/heads/"),
        "default should show branches"
    );
    assert!(stdout.contains("refs/tags/"), "default should show tags");
}

/// show-ref --head with a non-HEAD pattern should still include HEAD.
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_show_ref_head_exempt_from_pattern_filter() {
    let temp = tempdir().unwrap();
    let _guard = setup_repo_with_commit(&temp).await;

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp.path())
        .arg("show-ref")
        .arg("--head")
        .arg("main")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("HEAD"),
        "HEAD should appear even when pattern is 'master': {stdout}"
    );
    assert!(
        stdout.contains("refs/heads/main"),
        "main should also match: {stdout}"
    );
}

#[test]
fn test_show_ref_help_lists_examples_banner() {
    let repo = tempdir().expect("tempdir for show-ref --help");
    let output = run_libra_command(&["show-ref", "--help"], repo.path());
    assert!(
        output.status.success(),
        "show-ref --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "show-ref --help should include EXAMPLES banner, stdout: {stdout}"
    );
    for invocation in [
        "List all local refs with their object hashes",
        "libra show-ref --heads",
        "libra show-ref --tags",
        "libra show-ref --head",
        "libra show-ref -s",
        "libra show-ref -d",
        "libra show-ref main",
        "libra show-ref --json",
    ] {
        assert!(
            stdout.contains(invocation),
            "show-ref --help should include `{invocation}`, stdout: {stdout}"
        );
    }
}
