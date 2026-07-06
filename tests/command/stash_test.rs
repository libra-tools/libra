//! Tests stash push/pop/apply/drop/list operations.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::{fs, path::Path, str::FromStr};

use libra::{
    command::{
        add::{self, AddArgs},
        commit::{self, CommitArgs},
    },
    internal::branch::Branch,
    utils::{error::StableErrorCode, test::ChangeDirGuard},
};
use serial_test::serial;
use tempfile::tempdir;

use super::*;

fn latest_stash_commit(repo: &Path) -> Commit {
    let _guard = ChangeDirGuard::new(repo);
    let stash_ref =
        fs::read_to_string(repo.join(".libra/refs/stash")).expect("failed to read refs/stash");
    let stash_hash =
        ObjectHash::from_str(stash_ref.trim()).expect("refs/stash must contain a valid object id");
    load_object::<Commit>(&stash_hash).expect("failed to load latest stash commit")
}

fn status_short(repo: &Path) -> String {
    let output = run_libra_command(&["status", "--short"], repo);
    assert_cli_success(&output, "status --short");
    String::from_utf8(output.stdout).expect("status --short output should be UTF-8")
}

#[test]
#[serial]
fn test_stash_cli_outside_repository_returns_fatal_128() {
    let temp = tempdir().unwrap();
    let output = run_libra_command(&["stash", "push"], temp.path());
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal: not a libra repository"),
        "unexpected stderr: {stderr}"
    );
}

#[tokio::test]
#[serial]
async fn test_stash_push_no_changes() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create initial commit so HEAD exists
    fs::write("base.txt", "base").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["base.txt".to_string()],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("Initial commit".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    // stash push with no changes should remain a successful no-op
    let output = run_libra_command(&["stash", "push"], temp_path.path());
    assert_cli_success(&output, "stash push should be a no-op success");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No local changes to save"),
        "expected no-op message in stdout, got: {stdout}"
    );
}

#[tokio::test]
#[serial]
async fn test_stash_push_no_changes_json_output() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    fs::write("base.txt", "base").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["base.txt".to_string()],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("Initial commit".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    let output = run_libra_command(&["stash", "push", "--json"], temp_path.path());
    assert_cli_success(&output, "stash push --json should be a no-op success");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "stash");
    assert_eq!(json["data"]["action"], "noop");
    assert_eq!(json["data"]["message"], "No local changes to save");
    assert!(json["data"].get("stash_id").is_none());
}

#[tokio::test]
#[serial]
async fn test_stash_push_and_pop() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create initial commit
    fs::write("base.txt", "base content").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["base.txt".to_string()],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("Initial commit".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    // Modify file
    fs::write("base.txt", "modified content").unwrap();

    // Stash push
    let output = run_libra_command(&["stash", "push"], temp_path.path());
    assert!(
        output.status.success(),
        "stash push failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Saved working directory"),
        "expected confirmation message, got: {stdout}"
    );

    // File should be restored to original
    let content = fs::read_to_string(temp_path.path().join("base.txt")).unwrap();
    assert_eq!(
        content, "base content",
        "file should be restored after stash push"
    );

    // Stash pop
    let output = run_libra_command(&["stash", "pop"], temp_path.path());
    assert!(
        output.status.success(),
        "stash pop failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // File should have modified content again
    let content = fs::read_to_string(temp_path.path().join("base.txt")).unwrap();
    assert_eq!(
        content, "modified content",
        "file should be modified after stash pop"
    );
}

#[tokio::test]
#[serial]
async fn test_stash_push_and_pop_preserves_dotfiles() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    fs::create_dir_all(".config").unwrap();
    fs::write(".gitignore", "target/\n").unwrap();
    fs::write(".config/tool.toml", "mode = \"base\"\n").unwrap();

    add::execute(AddArgs {
        pathspec: vec![".gitignore".to_string(), ".config/tool.toml".to_string()],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("Track dotfiles".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    fs::write(".gitignore", "target/\n.env\n").unwrap();
    fs::write(".config/tool.toml", "mode = \"stashed\"\n").unwrap();

    let output = run_libra_command(&["stash", "push"], temp_path.path());
    assert!(
        output.status.success(),
        "stash push failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(".gitignore").unwrap(),
        "target/\n",
        "dotfile should be restored after stash push"
    );
    assert_eq!(
        fs::read_to_string(".config/tool.toml").unwrap(),
        "mode = \"base\"\n",
        "dot-directory content should be restored after stash push"
    );

    let output = run_libra_command(&["stash", "pop"], temp_path.path());
    assert!(
        output.status.success(),
        "stash pop failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(".gitignore").unwrap(),
        "target/\n.env\n",
        "dotfile change should round-trip through stash"
    );
    assert_eq!(
        fs::read_to_string(".config/tool.toml").unwrap(),
        "mode = \"stashed\"\n",
        "dot-directory change should round-trip through stash"
    );
}

#[test]
fn test_stash_pop_restores_unstaged_change_without_staging() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // Given: a tracked file has only a working-tree edit.
    fs::write(p.join("tracked.txt"), "worktree version\n").unwrap();
    assert_cli_success(&run_libra_command(&["stash", "push"], p), "stash push");

    // When: the stash is popped without --index support.
    assert_cli_success(&run_libra_command(&["stash", "pop"], p), "stash pop");

    // Then: the edit is back in the working tree but remains unstaged, matching
    // Git's default `stash pop` behavior.
    assert_eq!(status_short(p), " M tracked.txt\n");
}

#[test]
fn test_stash_pop_restores_staged_only_change_as_unstaged_by_default() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // Given: a tracked file has only a staged edit.
    fs::write(p.join("tracked.txt"), "staged version\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], p),
        "stage tracked file",
    );
    assert_cli_success(&run_libra_command(&["stash", "push"], p), "stash push");

    // When: the stash is popped without --index support.
    assert_cli_success(&run_libra_command(&["stash", "pop"], p), "stash pop");

    // Then: default pop restores the content as an unstaged working-tree edit.
    assert_eq!(
        fs::read_to_string(p.join("tracked.txt")).unwrap(),
        "staged version\n"
    );
    assert_eq!(status_short(p), " M tracked.txt\n");
}

#[test]
fn test_stash_pop_restores_mixed_file_as_unstaged_worktree_content() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // Given: a file has both staged content and a newer working-tree edit.
    fs::write(p.join("tracked.txt"), "staged version\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], p),
        "stage tracked file",
    );
    fs::write(p.join("tracked.txt"), "worktree version\n").unwrap();
    assert_cli_success(&run_libra_command(&["stash", "push"], p), "stash push");

    // When: the stash is popped without --index support.
    assert_cli_success(&run_libra_command(&["stash", "pop"], p), "stash pop");

    // Then: the working-tree content wins, but the index remains at HEAD.
    assert_eq!(
        fs::read_to_string(p.join("tracked.txt")).unwrap(),
        "worktree version\n"
    );
    assert_eq!(status_short(p), " M tracked.txt\n");
}

#[test]
fn test_stash_pop_reports_index_load_failure_without_dropping_stash() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // Given: a valid stash exists, then the on-disk index becomes unreadable.
    fs::write(p.join("tracked.txt"), "worktree version\n").unwrap();
    assert_cli_success(&run_libra_command(&["stash", "push"], p), "stash push");
    fs::write(p.join(".libra").join("index"), b"garb").unwrap();

    // When: default pop tries to build the current-worktree side of the merge.
    let output = run_libra_command(&["stash", "pop"], p);

    // Then: the index load failure is reported instead of treating the index as
    // empty, and pop leaves the stash entry in place.
    assert_eq!(output.status.code(), Some(128));
    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        human.contains("failed to load index"),
        "unexpected human stderr: {human}"
    );
    assert_eq!(report.error_code, StableErrorCode::IoReadFailed.as_str());
    assert!(
        report.message.contains("failed to load index"),
        "unexpected JSON message: {}",
        report.message
    );

    let list = run_libra_command(&["stash", "list"], p);
    assert_cli_success(&list, "stash list after failed pop");
    assert!(
        String::from_utf8_lossy(&list.stdout).contains("stash@{0}:"),
        "failed pop must keep the stash entry"
    );
}

#[tokio::test]
#[serial]
async fn test_stash_list() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create initial commit
    fs::write("base.txt", "base").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["base.txt".to_string()],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("Initial commit".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    // Empty stash list
    let output = run_libra_command(&["stash", "list"], temp_path.path());
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().is_empty(),
        "stash list should be empty initially"
    );

    // Create a stash
    fs::write("base.txt", "modified").unwrap();
    let output = run_libra_command(&["stash", "push"], temp_path.path());
    assert!(
        output.status.success(),
        "stash push failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // List should now show one entry
    let output = run_libra_command(&["stash", "list"], temp_path.path());
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("stash@{0}"),
        "expected stash@{{0}} in list, got: {stdout}"
    );
}

#[test]
fn test_stash_list_json_skips_blank_reflog_lines() {
    let repo = create_committed_repo_via_cli();

    fs::write(repo.path().join("tracked.txt"), "modified\n")
        .expect("failed to modify tracked file");
    assert_cli_success(
        &run_libra_command(&["stash", "push"], repo.path()),
        "stash push before reflog blank-line mutation",
    );

    let stash_log_path = repo.path().join(".libra/logs/refs/stash");
    let original = fs::read_to_string(&stash_log_path).expect("failed to read stash reflog");
    fs::write(&stash_log_path, format!("\n{original}\n\n"))
        .expect("failed to inject blank lines into stash reflog");

    let output = run_libra_command(&["stash", "list", "--json"], repo.path());
    assert_cli_success(
        &output,
        "stash list --json should ignore blank reflog lines",
    );

    let json = parse_json_stdout(&output);
    let entries = json["data"]["entries"]
        .as_array()
        .expect("expected stash list entries array");
    assert_eq!(entries.len(), 1, "blank reflog lines should be ignored");
    assert_eq!(entries[0]["index"], 0);
}

#[test]
fn test_stash_list_malformed_reflog_entry_returns_io_error() {
    let repo = create_committed_repo_via_cli();

    fs::write(repo.path().join("tracked.txt"), "modified\n")
        .expect("failed to modify tracked file");
    assert_cli_success(
        &run_libra_command(&["stash", "push"], repo.path()),
        "stash push before reflog corruption",
    );

    let stash_log_path = repo.path().join(".libra/logs/refs/stash");
    fs::write(&stash_log_path, "corrupted entry without hash\n")
        .expect("failed to corrupt stash reflog");

    let output = run_libra_command(&["stash", "list"], repo.path());
    assert_eq!(output.status.code(), Some(128));

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        human.contains("corrupted stash log entry"),
        "unexpected stderr: {human}"
    );
    assert_eq!(report.error_code, "LBR-IO-001");
    assert_eq!(report.exit_code, 128);
}

#[tokio::test]
#[serial]
async fn test_stash_drop() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create initial commit
    fs::write("base.txt", "base").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["base.txt".to_string()],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("Initial commit".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    // Create a stash
    fs::write("base.txt", "modified").unwrap();
    run_libra_command(&["stash", "push"], temp_path.path());

    // Drop it
    let output = run_libra_command(&["stash", "drop"], temp_path.path());
    assert!(
        output.status.success(),
        "stash drop failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Dropped stash@{0}"),
        "expected drop confirmation, got: {stdout}"
    );

    // List should be empty now
    let output = run_libra_command(&["stash", "list"], temp_path.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().is_empty(),
        "stash list should be empty after drop"
    );
}

#[tokio::test]
#[serial]
async fn test_stash_drop_missing_reflog_returns_no_stash_found() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    fs::write("base.txt", "base").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["base.txt".to_string()],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("Initial commit".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    fs::write("base.txt", "modified").unwrap();
    assert_cli_success(
        &run_libra_command(&["stash", "push"], temp_path.path()),
        "stash push before reflog removal",
    );

    fs::remove_file(temp_path.path().join(".libra/logs/refs/stash"))
        .expect("failed to remove stash reflog");

    let output = run_libra_command(&["stash", "drop"], temp_path.path());
    assert_eq!(output.status.code(), Some(129));

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        human.contains("fatal: no stash found"),
        "unexpected stderr: {human}"
    );
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert_eq!(report.exit_code, 129);
}

#[tokio::test]
#[serial]
async fn test_stash_json_output() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create initial commit
    fs::write("base.txt", "base").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["base.txt".to_string()],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("Initial commit".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    // JSON list on empty stash
    let output = run_libra_command(&["stash", "list", "--json"], temp_path.path());
    assert!(output.status.success());
    let json: Value =
        serde_json::from_slice(&output.stdout).expect("expected valid JSON from stash list --json");
    assert_eq!(json["command"], "stash");
    assert_eq!(json["data"]["action"], "list");
    assert!(json["data"]["entries"].as_array().unwrap().is_empty());

    // Stash something and test push JSON
    fs::write("base.txt", "modified").unwrap();
    let output = run_libra_command(&["stash", "push", "--json"], temp_path.path());
    assert!(
        output.status.success(),
        "stash push --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: Value =
        serde_json::from_slice(&output.stdout).expect("expected valid JSON from stash push --json");
    assert_eq!(json["command"], "stash");
    assert_eq!(json["data"]["action"], "push");
    assert!(json["data"]["message"].as_str().is_some());
    assert!(json["data"]["stash_id"].as_str().is_some());
}

#[test]
fn stash_round_trip_preserves_nested_dotfile_paths() {
    let repo = create_committed_repo_via_cli();

    let config_dir = repo.path().join(".config");
    let nested_file = config_dir.join("tool.toml");
    fs::create_dir_all(&config_dir).expect("failed to create nested config dir");
    fs::write(&nested_file, "name = \"base\"\n").expect("failed to write base nested file");

    let output = run_libra_command(&["add", ".config/tool.toml"], repo.path());
    assert_cli_success(&output, "add nested dotfile");

    let output = run_libra_command(
        &["commit", "-m", "track nested dotfile", "--no-verify"],
        repo.path(),
    );
    assert_cli_success(&output, "commit nested dotfile");

    fs::write(&nested_file, "name = \"modified\"\n").expect("failed to write modified nested file");

    let output = run_libra_command(&["stash", "push"], repo.path());
    assert_cli_success(&output, "stash push nested dotfile");
    assert_eq!(
        fs::read_to_string(&nested_file).expect("failed to read nested file after stash push"),
        "name = \"base\"\n"
    );

    let output = run_libra_command(&["stash", "pop"], repo.path());
    assert_cli_success(&output, "stash pop nested dotfile");

    assert_eq!(
        fs::read_to_string(&nested_file).expect("failed to read nested file after stash pop"),
        "name = \"modified\"\n"
    );
    assert!(
        !repo.path().join("tool.toml").exists(),
        "stash pop should not flatten nested dotfiles into the repo root"
    );
}

#[test]
fn test_stash_push_default_excludes_untracked() {
    let repo = create_committed_repo_via_cli();

    fs::write(repo.path().join("tracked.txt"), "modified tracked\n")
        .expect("failed to modify tracked file");
    fs::write(repo.path().join("untracked.txt"), "untracked\n")
        .expect("failed to write untracked file");

    let output = run_libra_command(&["stash", "push"], repo.path());
    assert_cli_success(&output, "default stash push");
    assert!(
        repo.path().join("untracked.txt").exists(),
        "default stash push should leave untracked files in the worktree"
    );

    let output = run_libra_command(&["stash", "show", "--json"], repo.path());
    assert_cli_success(&output, "stash show after default push");
    let json = parse_json_stdout(&output);
    let files = json["data"]["files"]
        .as_array()
        .expect("stash show files array");
    assert!(
        files.iter().all(|file| file["path"] != "untracked.txt"),
        "default stash push must not record untracked files: {json}"
    );
}

#[test]
fn test_stash_push_untracked_only_not_noop() {
    let repo = create_committed_repo_via_cli();

    fs::write(repo.path().join("untracked.txt"), "untracked\n")
        .expect("failed to write untracked file");

    let output = run_libra_command(&["stash", "push", "-u", "--json"], repo.path());
    assert_cli_success(&output, "stash push -u with only untracked files");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["action"], "push");
    assert_eq!(json["data"]["included_untracked"], 1);
    assert!(
        !repo.path().join("untracked.txt").exists(),
        "stash push -u should remove included untracked files"
    );
}

#[test]
fn test_stash_push_include_untracked() {
    let repo = create_committed_repo_via_cli();

    fs::write(repo.path().join("tracked.txt"), "modified tracked\n")
        .expect("failed to modify tracked file");
    fs::write(repo.path().join("untracked.txt"), "untracked\n")
        .expect("failed to write untracked file");
    fs::write(repo.path().join(".libraignore"), "ignored.log\n")
        .expect("failed to update libraignore");
    fs::write(repo.path().join("ignored.log"), "ignored\n").expect("failed to write ignored file");

    let output = run_libra_command(&["stash", "push", "-u"], repo.path());
    assert_cli_success(&output, "stash push -u");

    assert_eq!(
        fs::read_to_string(repo.path().join("tracked.txt")).expect("tracked file after stash -u"),
        "tracked\n"
    );
    assert!(
        !repo.path().join("untracked.txt").exists(),
        "stash push -u should remove included untracked files from the worktree"
    );
    assert!(
        repo.path().join("ignored.log").exists(),
        "stash push -u must leave ignored files alone"
    );

    let stash_commit = latest_stash_commit(repo.path());
    assert_eq!(
        stash_commit.parent_commit_ids.len(),
        3,
        "stash push -u should write HEAD, index, and untracked parents"
    );
}

#[test]
fn test_stash_push_all_includes_ignored() {
    let repo = create_committed_repo_via_cli();

    fs::write(repo.path().join(".libraignore"), "ignored.log\n")
        .expect("failed to update libraignore");
    fs::write(repo.path().join("untracked.txt"), "untracked\n")
        .expect("failed to write untracked file");
    fs::write(repo.path().join("ignored.log"), "ignored\n").expect("failed to write ignored file");

    let output = run_libra_command(&["stash", "push", "--all", "--json"], repo.path());
    assert_cli_success(&output, "stash push --all");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["included_untracked"], 2);

    assert!(
        !repo.path().join("untracked.txt").exists(),
        "stash push --all should remove visible untracked files"
    );
    assert!(
        !repo.path().join("ignored.log").exists(),
        "stash push --all should remove included ignored files from the worktree"
    );
}

#[test]
fn test_stash_push_keep_index() {
    let repo = create_committed_repo_via_cli();

    fs::write(repo.path().join("tracked.txt"), "staged version\n")
        .expect("failed to write staged version");
    let output = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert_cli_success(&output, "stage tracked file before stash --keep-index");
    fs::write(repo.path().join("tracked.txt"), "worktree version\n")
        .expect("failed to write unstaged version");

    let output = run_libra_command(&["stash", "push", "--keep-index"], repo.path());
    assert_cli_success(&output, "stash push --keep-index");

    assert_eq!(
        fs::read_to_string(repo.path().join("tracked.txt"))
            .expect("tracked file after stash --keep-index"),
        "staged version\n",
        "stash --keep-index should keep staged content in the worktree"
    );

    let status = run_libra_command(&["status", "--json"], repo.path());
    assert_cli_success(&status, "status --json after stash --keep-index");
    let json = parse_json_stdout(&status);
    let staged = json["data"]["staged"]["modified"]
        .as_array()
        .expect("staged modified array");
    assert!(
        staged.iter().any(|path| path == "tracked.txt"),
        "pre-stash staged state should remain in the index: {json}"
    );
    let unstaged = json["data"]["unstaged"]["modified"]
        .as_array()
        .expect("unstaged modified array");
    assert!(
        unstaged.iter().all(|path| path != "tracked.txt"),
        "unstaged delta should be removed by --keep-index: {json}"
    );
}

#[test]
fn test_stash_push_keep_index_mixed_file() {
    let repo = create_committed_repo_via_cli();

    fs::write(repo.path().join("tracked.txt"), "staged version\n")
        .expect("failed to write staged version");
    let output = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert_cli_success(&output, "stage tracked file before stash --keep-index");
    fs::write(repo.path().join("tracked.txt"), "worktree version\n")
        .expect("failed to write unstaged version");

    let output = run_libra_command(&["stash", "push", "--keep-index", "--json"], repo.path());
    assert_cli_success(&output, "stash push --keep-index --json");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["kept_index"], true);

    assert_eq!(
        fs::read_to_string(repo.path().join("tracked.txt"))
            .expect("tracked file after stash --keep-index"),
        "staged version\n"
    );
}

#[test]
fn test_stash_apply_restores_included_untracked() {
    let repo = create_committed_repo_via_cli();

    fs::write(repo.path().join("untracked.txt"), "untracked\n")
        .expect("failed to write untracked file");
    let output = run_libra_command(&["stash", "push", "-u"], repo.path());
    assert_cli_success(&output, "stash push -u before apply");
    assert!(
        !repo.path().join("untracked.txt").exists(),
        "stash push -u should remove included untracked file"
    );

    let output = run_libra_command(&["stash", "apply"], repo.path());
    assert_cli_success(
        &output,
        "stash apply should restore included untracked file",
    );
    assert_eq!(
        fs::read_to_string(repo.path().join("untracked.txt"))
            .expect("restored untracked file should exist"),
        "untracked\n"
    );

    let status = run_libra_command(&["status", "--json"], repo.path());
    assert_cli_success(&status, "status --json after restoring untracked file");
    let json = parse_json_stdout(&status);
    let untracked = json["data"]["untracked"]
        .as_array()
        .expect("untracked array");
    assert!(
        untracked.iter().any(|path| path == "untracked.txt"),
        "restored parent3 file should remain untracked: {json}"
    );
}

#[test]
fn test_stash_apply_untracked_collision_errors() {
    let repo = create_committed_repo_via_cli();

    fs::write(repo.path().join("untracked.txt"), "stashed\n")
        .expect("failed to write stashed untracked file");
    let output = run_libra_command(&["stash", "push", "-u"], repo.path());
    assert_cli_success(&output, "stash push -u before collision");
    fs::write(repo.path().join("untracked.txt"), "local\n")
        .expect("failed to write colliding untracked file");

    let output = run_libra_command(&["stash", "apply"], repo.path());
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("untracked files would be overwritten by stash apply"),
        "collision should name untracked overwrite risk, stderr: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(repo.path().join("untracked.txt"))
            .expect("colliding local file should remain"),
        "local\n",
        "stash apply must not overwrite a colliding untracked file"
    );
}

// ── C4 surface tests: `stash show` / `stash branch` / `stash clear` ───────────────────────

/// `libra stash --help` lists the new subcommands plus the EXAMPLES banner.
#[test]
fn test_stash_help_lists_show_branch_clear() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["stash", "--help"], repo.path());
    assert!(
        output.status.success(),
        "stash --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for sub in ["show", "branch", "clear"] {
        assert!(
            stdout.contains(sub),
            "stash --help should list '{sub}', stdout: {stdout}"
        );
    }
    assert!(
        stdout.contains("EXAMPLES:"),
        "stash --help should include EXAMPLES, stdout: {stdout}"
    );
}

/// `stash show` against a stash with a modified file emits a per-file
/// status entry and the matching JSON envelope.
#[test]
fn test_stash_show_reports_modified_file() {
    let repo = create_committed_repo_via_cli();

    fs::write(repo.path().join("tracked.txt"), "modified content\n")
        .expect("failed to modify tracked file");
    assert_cli_success(
        &run_libra_command(&["stash", "push"], repo.path()),
        "stash push before show",
    );

    let output = run_libra_command(&["stash", "show", "--json"], repo.path());
    assert_cli_success(&output, "stash show --json");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "stash");
    assert_eq!(json["data"]["action"], "show");
    let files = json["data"]["files"]
        .as_array()
        .expect("files should be an array");
    let tracked_modified = files
        .iter()
        .find(|f| f["path"] == "tracked.txt")
        .expect("tracked.txt must appear in stash show output");
    assert_eq!(
        tracked_modified["status"], "modified",
        "tracked.txt should be reported as modified"
    );
    assert!(
        json["data"]["files_changed"]["modified"]
            .as_u64()
            .expect("files_changed.modified should be a number")
            >= 1
    );
}

/// `stash show -p` emits a git-style unified diff of the stashed changes (and
/// the `--json` envelope carries the diff in an additive `patch` field), while a
/// plain `stash show` omits the `patch` field entirely.
#[test]
fn test_stash_show_patch_emits_unified_diff() {
    let repo = create_committed_repo_via_cli();

    fs::write(repo.path().join("tracked.txt"), "first line\nsecond line\n")
        .expect("failed to modify tracked file");
    assert_cli_success(
        &run_libra_command(&["stash", "push"], repo.path()),
        "stash push before show -p",
    );

    // Human `-p`: a unified diff with the git header + hunk for the change.
    let human = run_libra_command(&["stash", "show", "-p"], repo.path());
    assert_cli_success(&human, "stash show -p");
    let patch = String::from_utf8_lossy(&human.stdout);
    assert!(
        patch.contains("diff --git a/tracked.txt b/tracked.txt"),
        "expected a git diff header: {patch}"
    );
    assert!(patch.contains("@@"), "expected a hunk header: {patch}");
    assert!(
        patch.contains("+first line"),
        "expected the added line in the diff: {patch}"
    );
    // `-p` replaces the file-level summary footer.
    assert!(
        !patch.contains("files changed,"),
        "`-p` should not print the summary footer: {patch}"
    );

    // JSON `-p`: the `patch` field is present and holds the same diff.
    let json_out = run_libra_command(&["--json", "stash", "show", "-p"], repo.path());
    assert_cli_success(&json_out, "stash show -p --json");
    let json = parse_json_stdout(&json_out);
    assert_eq!(json["data"]["action"], "show");
    assert!(
        json["data"]["patch"]
            .as_str()
            .is_some_and(|p| p.contains("diff --git")),
        "JSON patch field should hold the unified diff"
    );

    // Without `-p`, the additive `patch` field is absent (back-compatible).
    let plain = run_libra_command(&["--json", "stash", "show"], repo.path());
    assert_cli_success(&plain, "stash show --json");
    assert!(
        parse_json_stdout(&plain)["data"].get("patch").is_none(),
        "plain stash show must not include the patch field"
    );
}

/// `stash show --name-only` in human mode prints only the file path,
/// without the "files changed" footer.
#[test]
fn test_stash_show_name_only_strips_summary() {
    let repo = create_committed_repo_via_cli();

    fs::write(repo.path().join("tracked.txt"), "modified content\n")
        .expect("failed to modify tracked file");
    assert_cli_success(
        &run_libra_command(&["stash", "push"], repo.path()),
        "stash push before show --name-only",
    );

    let output = run_libra_command(&["stash", "show", "--name-only"], repo.path());
    assert_cli_success(&output, "stash show --name-only");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.lines().any(|l| l == "tracked.txt"),
        "stash show --name-only should print 'tracked.txt', stdout: {stdout}"
    );
    assert!(
        !stdout.contains("files changed"),
        "stash show --name-only should suppress the footer, stdout: {stdout}"
    );
}

/// `stash show stash@{NN}` with an out-of-range index returns a fatal
/// error mapped to `LBR-CLI-003`.
#[test]
fn test_stash_show_invalid_index_errors() {
    let repo = create_committed_repo_via_cli();

    fs::write(repo.path().join("tracked.txt"), "modified\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["stash", "push"], repo.path()),
        "stash push before invalid show",
    );

    let output = run_libra_command(&["stash", "show", "stash@{42}"], repo.path());
    assert!(
        !output.status.success(),
        "stash show with bad index must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LBR-CLI-003"),
        "stash show invalid index should map to CLI-003, stderr: {stderr}"
    );
}

/// `stash branch <name>` creates a new branch, applies the stash, and
/// drops it. `applied` and `dropped` are both `true` in the JSON output
/// when the operation succeeds end-to-end.
#[test]
fn test_stash_branch_creates_branch_and_applies() {
    let repo = create_committed_repo_via_cli();

    fs::write(repo.path().join("tracked.txt"), "modified\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["stash", "push"], repo.path()),
        "stash push before branch",
    );

    let output = run_libra_command(&["stash", "branch", "stash-feature", "--json"], repo.path());
    assert_cli_success(&output, "stash branch --json");

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["action"], "branch");
    assert_eq!(json["data"]["branch"], "stash-feature");
    assert_eq!(json["data"]["applied"], true);
    assert_eq!(json["data"]["dropped"], true);
}

/// `stash branch <existing-name>` refuses with the dedicated
/// `LBR-CONFLICT-002` so callers can distinguish from generic failures.
#[test]
fn test_stash_branch_refuses_existing_branch() {
    let repo = create_committed_repo_via_cli();

    // Create a competing branch first via the CLI.
    assert_cli_success(
        &run_libra_command(&["branch", "occupied"], repo.path()),
        "create occupied branch",
    );

    fs::write(repo.path().join("tracked.txt"), "modified\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["stash", "push"], repo.path()),
        "stash push before branch conflict",
    );

    let output = run_libra_command(&["stash", "branch", "occupied"], repo.path());
    assert!(
        !output.status.success(),
        "stash branch onto existing name must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LBR-CONFLICT-002"),
        "branch conflict should surface ConflictOperationBlocked, stderr: {stderr}"
    );
}

/// `stash branch <name>` must treat a corrupt existing branch row as
/// name-occupied instead of letting the lossy branch lookup downgrade it to
/// "missing" and overwrite the row.
#[tokio::test]
#[serial]
async fn test_stash_branch_refuses_corrupt_existing_branch() {
    let repo = create_committed_repo_via_cli();
    {
        let _guard = ChangeDirGuard::new(repo.path());
        Branch::update_branch("occupied", "not-a-valid-hash", None)
            .await
            .unwrap();
    }

    fs::write(repo.path().join("tracked.txt"), "modified\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["stash", "push"], repo.path()),
        "stash push before corrupt branch conflict",
    );

    let output = run_libra_command(&["stash", "branch", "occupied"], repo.path());
    assert!(
        !output.status.success(),
        "stash branch must not overwrite a corrupt existing branch row"
    );
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(
        stderr.contains("a branch named 'occupied' already exists"),
        "unexpected stderr: {stderr}"
    );
}

/// `stash clear` without `--force` and not in JSON mode is rejected with
/// `LBR-CLI-002` to avoid accidental destructive runs in interactive use.
#[test]
fn test_stash_clear_requires_force_in_human_mode() {
    let repo = create_committed_repo_via_cli();

    fs::write(repo.path().join("tracked.txt"), "modified\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["stash", "push"], repo.path()),
        "stash push before clear without force",
    );

    let output = run_libra_command(&["stash", "clear"], repo.path());
    assert!(
        !output.status.success(),
        "stash clear without --force should fail in human mode"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("LBR-CLI-002"),
        "stash clear refusal should use CLI-002, stderr: {stderr}"
    );
}

/// `stash clear --force` removes every entry and reports the count.
#[test]
fn test_stash_clear_force_removes_all_entries() {
    let repo = create_committed_repo_via_cli();

    // Create two stash entries so the cleared_count is non-trivial.
    fs::write(repo.path().join("tracked.txt"), "first\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["stash", "push"], repo.path()),
        "stash push first",
    );
    fs::write(repo.path().join("tracked.txt"), "second\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["stash", "push"], repo.path()),
        "stash push second",
    );

    let output = run_libra_command(&["stash", "clear", "--force", "--json"], repo.path());
    assert_cli_success(&output, "stash clear --force --json");

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["action"], "clear");
    assert_eq!(json["data"]["cleared_count"], 2);

    // After clear the list should be empty again.
    let list = run_libra_command(&["stash", "list", "--json"], repo.path());
    assert_cli_success(&list, "stash list after clear");
    let list_json = parse_json_stdout(&list);
    assert_eq!(
        list_json["data"]["entries"]
            .as_array()
            .expect("entries array")
            .len(),
        0
    );
}

#[test]
fn stash_push_dash_k_is_keep_index_alias() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("k.txt"), "v1\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "k.txt"], p), "stage k.txt");

    // `-k` is the short alias for `--keep-index`; the push succeeds and the
    // staged content is kept in the index.
    let push = run_libra_command(&["stash", "push", "-k"], p);
    assert_cli_success(&push, "stash push -k");
    // The staged change is still present after `-k` (index kept).
    let status = run_libra_command(&["status", "--short"], p);
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("k.txt"),
        "the staged file remains tracked after stash push -k"
    );
}

#[test]
fn stash_no_include_untracked_countermands_u() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("tracked.txt"), "modified\n").unwrap();
    std::fs::write(p.join("untracked.txt"), "new\n").unwrap();

    // `-u --no-include-untracked` (last wins) countermands `-u`, so the untracked
    // file is NOT stashed and remains in the working tree.
    let out = run_libra_command(&["stash", "push", "-u", "--no-include-untracked"], p);
    assert_cli_success(&out, "stash push -u --no-include-untracked");
    assert!(
        p.join("untracked.txt").exists(),
        "untracked.txt not stashed (--no-include-untracked countermands -u)"
    );
}

/// `stash push <pathspec>` stashes ONLY the matched path: the path is reset to
/// HEAD while every other change stays in the working tree, and `pop` restores
/// the stashed change while preserving a further edit made to the untouched
/// path (exercising the working-tree-as-ours apply).
#[tokio::test]
#[serial]
async fn test_stash_push_pathspec_stashes_only_matched() {
    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let p = temp.path();
    let _guard = ChangeDirGuard::new(p);

    fs::write(p.join("a.txt"), "A0\n").unwrap();
    fs::write(p.join("b.txt"), "B0\n").unwrap();
    assert!(
        run_libra_command(&["add", "a.txt", "b.txt"], p)
            .status
            .success()
    );
    assert!(
        run_libra_command(&["commit", "-m", "base", "--no-verify"], p)
            .status
            .success()
    );

    fs::write(p.join("a.txt"), "A1\n").unwrap();
    fs::write(p.join("b.txt"), "B1\n").unwrap();

    let out = run_libra_command(&["stash", "push", "a.txt"], p);
    assert!(
        out.status.success(),
        "stash push a.txt: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(p.join("a.txt")).unwrap(),
        "A0\n",
        "matched path reset to HEAD"
    );
    assert_eq!(
        fs::read_to_string(p.join("b.txt")).unwrap(),
        "B1\n",
        "unmatched path keeps its change"
    );

    // Edit the unmatched path further before popping.
    fs::write(p.join("b.txt"), "B2\n").unwrap();

    let out = run_libra_command(&["stash", "pop"], p);
    assert!(
        out.status.success(),
        "stash pop: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(p.join("a.txt")).unwrap(),
        "A1\n",
        "matched path restored on pop"
    );
    assert_eq!(
        fs::read_to_string(p.join("b.txt")).unwrap(),
        "B2\n",
        "later edit to the unmatched path is preserved"
    );
}

/// A directory pathspec selects every changed file beneath it; files outside the
/// directory are left dirty.
#[tokio::test]
#[serial]
async fn test_stash_push_pathspec_directory() {
    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let p = temp.path();
    let _guard = ChangeDirGuard::new(p);

    fs::create_dir_all(p.join("sub")).unwrap();
    fs::write(p.join("sub/x.txt"), "X0\n").unwrap();
    fs::write(p.join("top.txt"), "T0\n").unwrap();
    assert!(
        run_libra_command(&["add", "sub/x.txt", "top.txt"], p)
            .status
            .success()
    );
    assert!(
        run_libra_command(&["commit", "-m", "base", "--no-verify"], p)
            .status
            .success()
    );

    fs::write(p.join("sub/x.txt"), "X1\n").unwrap();
    fs::write(p.join("top.txt"), "T1\n").unwrap();

    let out = run_libra_command(&["stash", "push", "sub"], p);
    assert!(
        out.status.success(),
        "stash push sub: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(p.join("sub/x.txt")).unwrap(),
        "X0\n",
        "file under the directory pathspec is reset"
    );
    assert_eq!(
        fs::read_to_string(p.join("top.txt")).unwrap(),
        "T1\n",
        "file outside the directory keeps its change"
    );

    assert!(run_libra_command(&["stash", "pop"], p).status.success());
    assert_eq!(fs::read_to_string(p.join("sub/x.txt")).unwrap(), "X1\n");
}

/// A pathspec that matches no tracked path is a usage error (exit 128,
/// `LBR-...` invalid-target), not an internal-invariant panic.
#[tokio::test]
#[serial]
async fn test_stash_push_pathspec_no_match_errors() {
    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let p = temp.path();
    let _guard = ChangeDirGuard::new(p);

    fs::write(p.join("a.txt"), "A0\n").unwrap();
    assert!(run_libra_command(&["add", "a.txt"], p).status.success());
    assert!(
        run_libra_command(&["commit", "-m", "base", "--no-verify"], p)
            .status
            .success()
    );
    fs::write(p.join("a.txt"), "A1\n").unwrap();

    let out = run_libra_command(&["stash", "push", "nonexistent.txt"], p);
    assert_eq!(out.status.code(), Some(129), "no-match pathspec exits 129");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("did not match"),
        "expected a pathspec-no-match message: {stderr}"
    );
    // The working tree is untouched after the rejected push.
    assert_eq!(fs::read_to_string(p.join("a.txt")).unwrap(), "A1\n");
}

/// Regression for the working-tree-as-ours apply: a FULL `stash push` followed
/// by an unrelated edit then `pop` must preserve that unrelated edit rather than
/// silently reverting it to HEAD.
#[tokio::test]
#[serial]
async fn test_stash_pop_preserves_unrelated_uncommitted_change() {
    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let p = temp.path();
    let _guard = ChangeDirGuard::new(p);

    fs::write(p.join("a.txt"), "A0\n").unwrap();
    fs::write(p.join("b.txt"), "B0\n").unwrap();
    assert!(
        run_libra_command(&["add", "a.txt", "b.txt"], p)
            .status
            .success()
    );
    assert!(
        run_libra_command(&["commit", "-m", "base", "--no-verify"], p)
            .status
            .success()
    );

    // Stash only a.txt's change (full stash, since b is unchanged here).
    fs::write(p.join("a.txt"), "A1\n").unwrap();
    assert!(run_libra_command(&["stash", "push"], p).status.success());

    // Now make an unrelated change to b.txt, then pop.
    fs::write(p.join("b.txt"), "B-new\n").unwrap();
    assert!(run_libra_command(&["stash", "pop"], p).status.success());

    assert_eq!(
        fs::read_to_string(p.join("a.txt")).unwrap(),
        "A1\n",
        "stashed change restored"
    );
    assert_eq!(
        fs::read_to_string(p.join("b.txt")).unwrap(),
        "B-new\n",
        "unrelated uncommitted change preserved across pop"
    );
}

/// Regression for the deletion-resurrection bug: after stashing a change and
/// then DELETING an unrelated tracked file, `pop` must keep the deletion rather
/// than silently resurrecting the file from the stash snapshot.
#[tokio::test]
#[serial]
async fn test_stash_pop_preserves_unrelated_deletion() {
    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let p = temp.path();
    let _guard = ChangeDirGuard::new(p);

    fs::write(p.join("a.txt"), "A0\n").unwrap();
    fs::write(p.join("b.txt"), "B0\n").unwrap();
    assert!(
        run_libra_command(&["add", "a.txt", "b.txt"], p)
            .status
            .success()
    );
    assert!(
        run_libra_command(&["commit", "-m", "base", "--no-verify"], p)
            .status
            .success()
    );

    // Stash a change to a.txt (full stash; b is unchanged).
    fs::write(p.join("a.txt"), "A1\n").unwrap();
    assert!(run_libra_command(&["stash", "push"], p).status.success());

    // Delete the unrelated file, then pop.
    fs::remove_file(p.join("b.txt")).unwrap();
    assert!(run_libra_command(&["stash", "pop"], p).status.success());

    assert_eq!(
        fs::read_to_string(p.join("a.txt")).unwrap(),
        "A1\n",
        "stashed change restored"
    );
    assert!(
        !p.join("b.txt").exists(),
        "the unrelated deletion must NOT be resurrected by pop"
    );
}

/// A staged-only change (index differs from HEAD while the working tree matches
/// HEAD) is still stashed by a pathspec push — the no-op check must consider the
/// index overlay, not only the working tree.
#[tokio::test]
#[serial]
async fn test_stash_push_pathspec_stashes_staged_only_change() {
    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let p = temp.path();
    let _guard = ChangeDirGuard::new(p);

    fs::write(p.join("a.txt"), "A0\n").unwrap();
    assert!(run_libra_command(&["add", "a.txt"], p).status.success());
    assert!(
        run_libra_command(&["commit", "-m", "base", "--no-verify"], p)
            .status
            .success()
    );

    // Stage A1, then restore the WORKING TREE to A0: index=A1, worktree=A0=HEAD.
    fs::write(p.join("a.txt"), "A1\n").unwrap();
    assert!(run_libra_command(&["add", "a.txt"], p).status.success());
    fs::write(p.join("a.txt"), "A0\n").unwrap();

    let out = run_libra_command(&["stash", "push", "a.txt"], p);
    assert!(
        out.status.success(),
        "staged-only change should be stashed, not a no-op: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("No local changes"),
        "must not report a no-op for a staged-only change"
    );
    // The path is reset to HEAD after the push...
    assert_eq!(fs::read_to_string(p.join("a.txt")).unwrap(), "A0\n");
    // ...and `pop` restores the staged-only change to the working tree, rather
    // than dropping it (Libra has no `--index`, so it is restored losslessly).
    assert!(run_libra_command(&["stash", "pop"], p).status.success());
    assert_eq!(
        fs::read_to_string(p.join("a.txt")).unwrap(),
        "A1\n",
        "staged-only change is restored on pop, not lost"
    );
}

/// `stash push -- .` (the root pathspec) selects every tracked change.
#[tokio::test]
#[serial]
async fn test_stash_push_pathspec_dot_matches_all() {
    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let p = temp.path();
    let _guard = ChangeDirGuard::new(p);

    fs::write(p.join("a.txt"), "A0\n").unwrap();
    fs::write(p.join("b.txt"), "B0\n").unwrap();
    assert!(
        run_libra_command(&["add", "a.txt", "b.txt"], p)
            .status
            .success()
    );
    assert!(
        run_libra_command(&["commit", "-m", "base", "--no-verify"], p)
            .status
            .success()
    );
    fs::write(p.join("a.txt"), "A1\n").unwrap();
    fs::write(p.join("b.txt"), "B1\n").unwrap();

    let out = run_libra_command(&["stash", "push", "."], p);
    assert!(
        out.status.success(),
        "stash push . : {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read_to_string(p.join("a.txt")).unwrap(), "A0\n");
    assert_eq!(fs::read_to_string(p.join("b.txt")).unwrap(), "B0\n");
    assert!(run_libra_command(&["stash", "pop"], p).status.success());
    assert_eq!(fs::read_to_string(p.join("a.txt")).unwrap(), "A1\n");
    assert_eq!(fs::read_to_string(p.join("b.txt")).unwrap(), "B1\n");
}

/// `-u`/`-a`/`-k` combined with a pathspec are rejected (exit 129) rather than
/// silently ignored.
#[tokio::test]
#[serial]
async fn test_stash_push_pathspec_rejects_options() {
    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let p = temp.path();
    let _guard = ChangeDirGuard::new(p);

    fs::write(p.join("a.txt"), "A0\n").unwrap();
    assert!(run_libra_command(&["add", "a.txt"], p).status.success());
    assert!(
        run_libra_command(&["commit", "-m", "base", "--no-verify"], p)
            .status
            .success()
    );
    fs::write(p.join("a.txt"), "A1\n").unwrap();

    for opt in ["-u", "-a", "-k"] {
        let out = run_libra_command(&["stash", "push", opt, "a.txt"], p);
        assert_eq!(
            out.status.code(),
            Some(129),
            "stash push {opt} -- pathspec must be rejected with exit 129"
        );
        // The working tree is left untouched by the rejected push.
        assert_eq!(fs::read_to_string(p.join("a.txt")).unwrap(), "A1\n");
    }
}
