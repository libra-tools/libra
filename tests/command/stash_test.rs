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
#[serial(cwd)]
async fn test_stash_push_no_changes() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create initial commit so HEAD exists
    fs::write("base.txt", "base").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
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
#[serial(cwd)]
async fn test_stash_push_no_changes_json_output() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    fs::write("base.txt", "base").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
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
#[serial(cwd)]
async fn test_stash_push_and_pop() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create initial commit
    fs::write("base.txt", "base content").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
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
#[serial(cwd)]
async fn test_stash_push_and_pop_preserves_dotfiles() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    fs::create_dir_all(".config").unwrap();
    fs::write(".gitignore", "target/\n").unwrap();
    fs::write(".config/tool.toml", "mode = \"base\"\n").unwrap();

    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
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
#[serial(cwd)]
async fn test_stash_list() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create initial commit
    fs::write("base.txt", "base").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
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
#[serial(cwd)]
async fn test_stash_drop() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create initial commit
    fs::write("base.txt", "base").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
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
#[serial(cwd)]
async fn test_stash_drop_missing_reflog_returns_no_stash_found() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    fs::write("base.txt", "base").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
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
#[serial(cwd)]
async fn test_stash_json_output() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create initial commit
    fs::write("base.txt", "base").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
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
///
/// §C.12 roster: this is `stash_branch_apply_success_only_then_cas_drop` —
/// the drop is reported only because the apply completed, and the CAS is the
/// only path that removes the entry. The CAS-MISS arm (stack changed between
/// apply and drop → entry kept, never re-resolved by index) is pinned at the
/// unit level by `do_drop_cas_misses_leave_the_stack_untouched` in
/// `src/command/stash.rs`, which `stash branch` shares with pop.
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
#[serial(cwd)]
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
#[serial(cwd)]
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

/// FIX-AD-01: a wildcard pathspec is expanded through the shared pathspec
/// engine, so `*.txt` stashes the literal `*.txt` and `a.txt` (Git parity),
/// while a non-matching path is left untouched.
#[test]
fn test_stash_push_pathspec_glob_stashes_all_matches() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);

    fs::write(p.join("*.txt"), "S0\n").unwrap();
    fs::write(p.join("a.txt"), "A0\n").unwrap();
    fs::write(p.join("notes.md"), "N0\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "*.txt", "a.txt", "notes.md"], p),
        "add",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit",
    );

    fs::write(p.join("*.txt"), "S1\n").unwrap();
    fs::write(p.join("a.txt"), "A1\n").unwrap();
    fs::write(p.join("notes.md"), "N1\n").unwrap();

    let out = run_libra_command(&["stash", "push", "*.txt"], p);
    assert!(
        out.status.success(),
        "stash push *.txt: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read_to_string(p.join("*.txt")).unwrap(), "S0\n");
    assert_eq!(
        fs::read_to_string(p.join("a.txt")).unwrap(),
        "A0\n",
        "the glob must also stash a.txt"
    );
    assert_eq!(
        fs::read_to_string(p.join("notes.md")).unwrap(),
        "N1\n",
        "a non-matching path keeps its change"
    );
}

/// A directory pathspec selects every changed file beneath it; files outside the
/// directory are left dirty.
#[tokio::test]
#[serial(cwd)]
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
#[serial(cwd)]
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
#[serial(cwd)]
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
#[serial(cwd)]
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
#[serial(cwd)]
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
#[serial(cwd)]
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
#[serial(cwd)]
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

/// W2 §C.4.3: a failed `stash branch` apply rolls back the half-created
/// state — the new branch is deleted (tip-conditionally), HEAD returns to
/// the original branch, and the stash entry is kept.
///
/// §C.12 roster: this is `stash_branch_failure_has_zero_side_effects`; the
/// crash-interrupted half of the same contract is
/// `an_interrupted_stash_branch_rollback_completes_from_the_journal` in the
/// worktree-isolation suite.
#[test]
fn stash_branch_failed_apply_rolls_back_branch_and_head() {
    let repo = create_committed_repo_via_cli();
    let path = repo.path();
    // Stash a tracked change, then dirty the SAME file differently so the
    // apply inside `stash branch` conflicts and fails.
    std::fs::write(path.join("tracked.txt"), "stash me\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["stash", "push", "-m", "rollback-probe"], path),
        "stash push",
    );
    std::fs::write(path.join("tracked.txt"), "conflicting local edit\n").unwrap();

    let branched = run_libra_command(&["stash", "branch", "rollback-nb"], path);
    assert_ne!(
        branched.status.code(),
        Some(0),
        "conflicting apply fails the branch command"
    );
    let branches = run_libra_command(&["branch"], path);
    assert_cli_success(&branches, "branch list");
    assert!(
        !String::from_utf8_lossy(&branches.stdout).contains("rollback-nb"),
        "the half-created branch was rolled back: {}",
        String::from_utf8_lossy(&branches.stdout)
    );
    let listed = run_libra_command(&["stash", "list"], path);
    assert_cli_success(&listed, "stash list");
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("rollback-probe"),
        "the stash entry is kept after the failed branch"
    );
    let status = run_libra_command(&["status"], path);
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("main"),
        "HEAD returned to the original branch: {}",
        String::from_utf8_lossy(&status.stdout)
    );
}

#[test]
fn test_stash_push_literal_pathspecs_global() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("x.txt"), "x\n").unwrap();
    fs::write(p.join("*.txt"), "star\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "x.txt", "*.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit",
    );
    fs::write(p.join("x.txt"), "x2\n").unwrap();
    fs::write(p.join("*.txt"), "star2\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["--literal-pathspecs", "stash", "push", "--", "*.txt"], p),
        "stash literal",
    );
    let status = run_libra_command(&["status", "--short"], p);
    let text = String::from_utf8_lossy(&status.stdout);
    assert!(text.contains("x.txt"), "x.txt left dirty: {text}");
    assert!(!text.contains("*.txt"), "*.txt was stashed: {text}");
}

/// FM-02 (M-MAT2 U6): `stash pop` restores an executable file with its bit.
#[cfg(unix)]
#[test]
fn test_stash_pop_materializes_executable_bit() {
    use std::os::unix::fs::PermissionsExt;

    let repo = tempdir().expect("repo");
    let repo_path = repo.path();
    init_repo_via_cli(repo_path);
    configure_identity_via_cli(repo_path);
    let script = repo_path.join("run.sh");
    fs::write(&script, "#!/bin/sh\necho v1\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "run.sh"], repo_path),
        "add script",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "exec", "--no-verify"], repo_path),
        "commit exec",
    );
    fs::write(&script, "#!/bin/sh\necho v2\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["stash", "push"], repo_path),
        "stash push",
    );
    assert_eq!(
        fs::symlink_metadata(&script)
            .expect("script metadata after push")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "stash push must restore the HEAD executable"
    );
    assert_cli_success(
        &run_libra_command(&["stash", "pop"], repo_path),
        "stash pop",
    );
    assert_eq!(
        fs::symlink_metadata(&script)
            .expect("script metadata after pop")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "stash pop must restore the execute bit"
    );
}

/// FM-04 (M-DET D5, plan-20260918): a mode-only change is stashed and restored.
#[cfg(unix)]
#[test]
fn test_stash_push_pop_mode_only_change() {
    use std::os::unix::fs::PermissionsExt;

    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    let run = root.join("run.sh");
    fs::write(&run, "#!/bin/sh\n").expect("write run");
    fs::set_permissions(&run, fs::Permissions::from_mode(0o755)).expect("chmod run");
    assert_cli_success(&run_libra_command(&["add", "run.sh"], root), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "commit",
    );
    fs::set_permissions(&run, fs::Permissions::from_mode(0o644)).expect("chmod 644");

    assert_cli_success(&run_libra_command(&["stash", "push"], root), "stash push");
    assert_eq!(
        fs::symlink_metadata(&run).unwrap().permissions().mode() & 0o777,
        0o755,
        "push restores the HEAD executable"
    );
    assert_cli_success(&run_libra_command(&["stash", "pop"], root), "stash pop");
    assert_eq!(
        fs::symlink_metadata(&run).unwrap().permissions().mode() & 0o777,
        0o644,
        "pop restores the stashed non-executable mode"
    );

    // D6: with core.fileMode=false a mode-only change is not a stash candidate.
    assert_cli_success(
        &run_libra_command(&["config", "set", "core.fileMode", "false"], root),
        "disable fileMode",
    );
    fs::set_permissions(&run, fs::Permissions::from_mode(0o755)).expect("chmod 755");
    let out = run_libra_command(&["stash", "push"], root);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("No local changes to save"),
        "D6 stash push: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// WT-08 (M-BARE B1–B6, issues/476): an omitted `stash` subcommand is
/// `stash push`; an unexpected first token is a usage error.
#[test]
fn test_bare_stash_is_push_matrix() {
    fn committed_repo() -> (tempfile::TempDir, std::path::PathBuf) {
        let repo = tempdir().expect("tempdir");
        let root = repo.path().to_path_buf();
        init_repo_via_cli(&root);
        configure_identity_via_cli(&root);
        fs::write(root.join("a.txt"), "one\n").expect("write");
        assert_cli_success(&run_libra_command(&["add", "a.txt"], &root), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "init", "--no-verify"], &root),
            "commit",
        );
        (repo, root)
    }

    // B1: bare `stash` with a change saves a stash, like `stash push`.
    let (_repo, root) = committed_repo();
    fs::write(root.join("a.txt"), "two\n").expect("modify");
    assert_cli_success(&run_libra_command(&["stash"], &root), "bare stash");
    let list = run_libra_command(&["stash", "list"], &root);
    assert!(
        String::from_utf8_lossy(&list.stdout).contains("stash@{0}"),
        "B1 created a stash: {}",
        String::from_utf8_lossy(&list.stdout)
    );
    assert_eq!(
        fs::read_to_string(root.join("a.txt")).expect("reverted"),
        "one\n",
        "B1 reverts the worktree"
    );

    // B1 (no changes): bare `stash` is a successful no-op.
    let (_repo, root) = committed_repo();
    let noop = run_libra_command(&["stash"], &root);
    assert_cli_success(&noop, "bare stash no changes");
    assert!(
        String::from_utf8_lossy(&noop.stdout).contains("No local changes to save"),
        "B1 no-op: {}",
        String::from_utf8_lossy(&noop.stdout)
    );

    // B2/B3: the push options parse without a subcommand.
    let (_repo, root) = committed_repo();
    fs::write(root.join("a.txt"), "three\n").expect("modify");
    assert_cli_success(
        &run_libra_command(&["stash", "-m", "bare-msg"], &root),
        "B2 stash -m",
    );
    let listed = run_libra_command(&["stash", "list"], &root);
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("On main: bare-msg"),
        "B2/WT-10 message: {}",
        String::from_utf8_lossy(&listed.stdout)
    );
    let (_repo, root) = committed_repo();
    fs::write(root.join("untracked.txt"), "u\n").expect("write untracked");
    assert_cli_success(&run_libra_command(&["stash", "-u"], &root), "B3 stash -u");
    assert!(
        !root.join("untracked.txt").exists(),
        "B3 -u stashes the untracked file"
    );
    let (_repo, root) = committed_repo();
    fs::write(root.join("a.txt"), "kept\n").expect("modify");
    assert_cli_success(&run_libra_command(&["add", "a.txt"], &root), "stage");
    assert_cli_success(&run_libra_command(&["stash", "-k"], &root), "B3 stash -k");

    // B4: `stash -- <pathspec>` is `push -- <pathspec>`.
    let (_repo, root) = committed_repo();
    fs::write(root.join("a.txt"), "pathed\n").expect("modify");
    assert_cli_success(&run_libra_command(&["stash", "--", "a.txt"], &root), "B4");
    assert_eq!(
        fs::read_to_string(root.join("a.txt")).expect("reverted"),
        "one\n"
    );

    // B5: an existing subcommand still works (regression).
    let (_repo, root) = committed_repo();
    assert_cli_success(&run_libra_command(&["stash", "list"], &root), "B5 list");

    // B6: an unexpected first token is refused with Git's wording.
    let (_repo, root) = committed_repo();
    let refused = run_libra_command(&["stash", "foo"], &root);
    assert_eq!(refused.status.code(), Some(129), "B6 exit");
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("'push' can't be assumed due to unexpected token 'foo'"),
        "B6 wording: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
}

/// WT-09 (M-PRE P1–P5, issues/476): `stash push` checks the initial commit
/// before the change set, and `-q` silences that failure's human error.
#[test]
fn test_stash_push_no_initial_commit_matrix() {
    // P1: no initial commit + only untracked files -> the HEAD check fails.
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    fs::write(root.join("untracked.txt"), "u\n").expect("write");
    let failed = run_libra_command(&["stash", "push"], root);
    assert_eq!(
        failed.status.code(),
        Some(128),
        "P1 exit: {}",
        String::from_utf8_lossy(&failed.stderr)
    );
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("you do not have the initial commit yet"),
        "P1 wording: {}",
        String::from_utf8_lossy(&failed.stderr)
    );
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("LBR-REPO-003"),
        "P1 code: {}",
        String::from_utf8_lossy(&failed.stderr)
    );
    assert!(
        !root.join(".libra/refs/stash").exists(),
        "P1 must not create refs/stash"
    );
    assert!(
        root.join("untracked.txt").is_file(),
        "P1 must leave the untracked file in place"
    );

    // P2: no initial commit + a staged change -> the same failure.
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    fs::write(root.join("staged.txt"), "s\n").expect("write");
    assert_cli_success(&run_libra_command(&["add", "staged.txt"], root), "add");
    let failed = run_libra_command(&["stash", "push"], root);
    assert_eq!(failed.status.code(), Some(128), "P2 exit");
    assert!(
        !root.join(".libra/refs/stash").exists(),
        "P2 must not create refs/stash"
    );
    assert!(
        root.join("staged.txt").is_file(),
        "P2 must leave the staged file in place"
    );

    // P3/P4: `-q` prints nothing on the failure; a clean tree stays a no-op.
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    fs::write(root.join("untracked.txt"), "u\n").expect("write");
    let quiet = run_libra_command(&["--quiet", "stash", "push"], root);
    assert_eq!(quiet.status.code(), Some(128), "P3 exit");
    assert!(quiet.stdout.is_empty(), "P3 stdout must be empty");
    assert!(quiet.stderr.is_empty(), "P3 stderr must be empty under -q");

    // P5: the structured envelope still carries the failure under `--json`.
    let json = run_libra_command(&["--json", "stash", "push"], root);
    assert_eq!(json.status.code(), Some(128), "P5 exit");
    let parsed: serde_json::Value =
        serde_json::from_slice(&json.stderr).expect("P5 error envelope");
    assert_eq!(parsed["ok"], serde_json::json!(false), "P5 ok=false");
    assert_eq!(parsed["error_code"], "LBR-REPO-003", "P5 code");
    assert!(
        parsed["message"]
            .as_str()
            .unwrap_or_default()
            .contains("initial commit"),
        "P5 message: {parsed}"
    );

    // Regression: with an initial commit the ordinary paths still work.
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    fs::write(root.join("a.txt"), "one\n").expect("write");
    assert_cli_success(&run_libra_command(&["add", "a.txt"], root), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "commit",
    );
    let clean = run_libra_command(&["stash", "push"], root);
    assert_cli_success(&clean, "clean tree");
    assert!(
        String::from_utf8_lossy(&clean.stdout).contains("No local changes to save"),
        "regression no-op: {}",
        String::from_utf8_lossy(&clean.stdout)
    );
    let quiet_clean = run_libra_command(&["--quiet", "stash", "push"], root);
    assert_cli_success(&quiet_clean, "P4 quiet no-op");
    assert!(quiet_clean.stdout.is_empty(), "P4 stdout silent");
}

/// WT-10 (M-MSG S1–S6, issues/476): stash messages skip `gpgsig`, prefix `-m`,
/// use `(no branch)` when detached, and keep reading unprefixed legacy entries.
#[test]
fn test_stash_message_format_matrix() {
    fn head_abbrev7(root: &Path) -> String {
        let out = run_libra_command(&["rev-parse", "HEAD"], root);
        assert_cli_success(&out, "rev-parse HEAD");
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .chars()
            .take(7)
            .collect()
    }

    fn stash_list(root: &Path) -> String {
        let out = run_libra_command(&["stash", "list"], root);
        assert_cli_success(&out, "stash list");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn assert_no_signature_leak(text: &str, label: &str) {
        assert!(
            !text.contains("gpgsig") && !text.contains("BEGIN PGP SIGNATURE"),
            "{label} leaked a signature header: {text}"
        );
    }

    fn rewrite_stash_log_message(root: &Path, new_message: &str) {
        let log_path = root.join(".libra/logs/refs/stash");
        let log = fs::read_to_string(&log_path).expect("stash log");
        let rewritten = log
            .lines()
            .map(|line| {
                if let Some((meta, rest)) = line.split_once('\t') {
                    if let Some((_, generation)) = rest.rsplit_once('\t')
                        && generation.starts_with("gen=")
                    {
                        return format!("{meta}\t{new_message}\t{generation}");
                    }
                    format!("{meta}\t{new_message}")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut body = rewritten;
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
        fs::write(&log_path, body).expect("rewrite stash log");
    }

    // S1: vault-signed HEAD — subject is `init`, never the gpgsig header.
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], root),
        "S1 init",
    );
    configure_identity_via_cli(root);
    assert_cli_success(
        &run_libra_command(&["config", "generate-gpg-key"], root),
        "S1 generate-gpg-key",
    );
    fs::write(root.join("a.txt"), "one\n").expect("write");
    assert_cli_success(&run_libra_command(&["add", "a.txt"], root), "S1 add");
    let signed = run_libra_command(&["--json", "commit", "-m", "init", "--no-verify"], root);
    assert_cli_success(&signed, "S1 signed commit");
    assert_eq!(
        parse_json_stdout(&signed)["data"]["signed"].as_bool(),
        Some(true),
        "S1 HEAD must be signed"
    );
    let abbrev = head_abbrev7(root);
    fs::write(root.join("a.txt"), "two\n").expect("modify");
    let push = run_libra_command(&["stash", "push"], root);
    assert_cli_success(&push, "S1 stash push");
    let expected = format!("WIP on main: {abbrev} init");
    let saved = format!("Saved working directory and index state {expected}");
    let stdout = String::from_utf8_lossy(&push.stdout);
    assert!(stdout.contains(&saved), "S1 push stdout: {stdout}");
    assert_no_signature_leak(&stdout, "S1 push");
    let listed = stash_list(root);
    assert!(listed.contains(&expected), "S1 list: {listed}");
    assert_no_signature_leak(&listed, "S1 list");

    // S2: unsigned HEAD still uses the first subject line.
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    fs::write(root.join("a.txt"), "one\n").expect("write");
    assert_cli_success(&run_libra_command(&["add", "a.txt"], root), "S2 add");
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "init", "--no-verify", "--no-gpg-sign"],
            root,
        ),
        "S2 unsigned commit",
    );
    let cat = run_libra_command(&["cat-file", "-p", "HEAD"], root);
    assert!(
        !String::from_utf8_lossy(&cat.stdout).contains("gpgsig"),
        "S2 commit must be unsigned"
    );
    let abbrev = head_abbrev7(root);
    fs::write(root.join("a.txt"), "two\n").expect("modify");
    let push = run_libra_command(&["stash", "push"], root);
    assert_cli_success(&push, "S2 stash push");
    let expected = format!("WIP on main: {abbrev} init");
    let stdout = String::from_utf8_lossy(&push.stdout);
    assert!(stdout.contains(&expected), "S2 push stdout: {stdout}");
    assert!(!expected.ends_with(' '), "S2 subject must be non-empty");

    // S3: `-m` (ordinary and pathspec push) becomes `On <branch>: <msg>`.
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    fs::write(root.join("a.txt"), "one\n").expect("write");
    assert_cli_success(&run_libra_command(&["add", "a.txt"], root), "S3 add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "S3 commit",
    );
    fs::write(root.join("a.txt"), "two\n").expect("modify");
    let push = run_libra_command(&["stash", "push", "-m", "named"], root);
    assert_cli_success(&push, "S3 -m");
    let listed = stash_list(root);
    assert!(listed.contains("On main: named"), "S3 list: {listed}");
    fs::write(root.join("a.txt"), "three\n").expect("modify");
    let pathspec = run_libra_command(&["stash", "push", "-m", "path-msg", "--", "a.txt"], root);
    assert_cli_success(&pathspec, "S3 pathspec -m");
    let listed = stash_list(root);
    assert!(
        listed.contains("On main: path-msg"),
        "S3 pathspec list: {listed}"
    );

    // S4: detached HEAD uses Git's `(no branch)` label.
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    fs::write(root.join("a.txt"), "one\n").expect("write");
    assert_cli_success(&run_libra_command(&["add", "a.txt"], root), "S4 add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "S4 commit",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "--detach"], root),
        "S4 detach",
    );
    let abbrev = head_abbrev7(root);
    fs::write(root.join("a.txt"), "two\n").expect("modify");
    let push = run_libra_command(&["stash", "push"], root);
    assert_cli_success(&push, "S4 default");
    let expected = format!("WIP on (no branch): {abbrev} init");
    let listed = stash_list(root);
    assert!(listed.contains(&expected), "S4 list: {listed}");
    fs::write(root.join("a.txt"), "three\n").expect("modify");
    let named = run_libra_command(&["stash", "push", "-m", "x"], root);
    assert_cli_success(&named, "S4 -m");
    let listed = stash_list(root);
    assert!(
        listed.contains("On (no branch): x"),
        "S4 named list: {listed}"
    );

    // S5: a legacy unprefixed reflog message still lists / shows / applies.
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    fs::write(root.join("a.txt"), "one\n").expect("write");
    assert_cli_success(&run_libra_command(&["add", "a.txt"], root), "S5 add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "S5 commit",
    );
    fs::write(root.join("a.txt"), "legacy\n").expect("modify");
    assert_cli_success(&run_libra_command(&["stash", "push"], root), "S5 push");
    rewrite_stash_log_message(root, "legacy unprefixed message");
    let listed = stash_list(root);
    assert!(
        listed.contains("legacy unprefixed message"),
        "S5 list: {listed}"
    );
    assert_cli_success(&run_libra_command(&["stash", "show"], root), "S5 show");
    assert_cli_success(&run_libra_command(&["stash", "apply"], root), "S5 apply");
    assert_eq!(
        fs::read_to_string(root.join("a.txt")).expect("read"),
        "legacy\n"
    );
    assert_cli_success(&run_libra_command(&["stash", "drop"], root), "S5 drop");

    // S6: rebase/merge autostash uses the same helper (`On <branch>: autostash`).
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    fs::write(root.join("shared.txt"), "ORIG\n").expect("write");
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], root), "S6 add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], root),
        "S6 base",
    );
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], root),
        "S6 branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], root),
        "S6 checkout feature",
    );
    fs::write(root.join("shared.txt"), "FEATURE\n").expect("write");
    assert_cli_success(
        &run_libra_command(&["add", "shared.txt"], root),
        "S6 feature add",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "feature", "--no-verify"], root),
        "S6 feature commit",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], root),
        "S6 checkout main",
    );
    fs::write(root.join("shared.txt"), "MAIN\n").expect("write");
    assert_cli_success(
        &run_libra_command(&["add", "shared.txt"], root),
        "S6 main add",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main", "--no-verify"], root),
        "S6 main commit",
    );
    fs::write(root.join("extra.txt"), "precious\n").expect("write extra");
    assert_cli_success(
        &run_libra_command(&["add", "extra.txt"], root),
        "S6 dirty add",
    );
    let merge = run_libra_command(&["merge", "feature", "--autostash"], root);
    assert_eq!(merge.status.code(), Some(128), "S6 conflict");
    let sidecar: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join(".libra/merge-autostash.json")).expect("sidecar"),
    )
    .expect("sidecar json");
    let oid = sidecar["stash_commit"].as_str().expect("held stash oid");
    let held = run_libra_command(&["cat-file", "-p", oid], root);
    assert_cli_success(&held, "S6 cat-file held stash");
    let held_text = String::from_utf8_lossy(&held.stdout);
    assert!(
        held_text.contains("On main: autostash"),
        "S6 held message: {held_text}"
    );
    assert_no_signature_leak(&held_text, "S6 held");
}

/// WT-11 (M-IDX X1–X8, issues/476): default apply restages new files;
/// `--index` restores both layers; `stash branch` uses `--index` semantics.
#[test]
fn test_stash_index_restoration_matrix() {
    fn mixed_stash_repo() -> (tempfile::TempDir, std::path::PathBuf) {
        let repo = tempdir().expect("tempdir");
        let root = repo.path().to_path_buf();
        init_repo_via_cli(&root);
        configure_identity_via_cli(&root);
        fs::write(root.join("a.txt"), "one\n").expect("write");
        assert_cli_success(&run_libra_command(&["add", "a.txt"], &root), "add a.txt");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "init", "--no-verify"], &root),
            "commit",
        );
        fs::write(root.join("a.txt"), "staged\n").expect("stage a.txt");
        assert_cli_success(&run_libra_command(&["add", "a.txt"], &root), "stage a.txt");
        fs::write(root.join("a.txt"), "worktree\n").expect("dirty a.txt");
        fs::write(root.join("s.txt"), "new\n").expect("write s.txt");
        assert_cli_success(&run_libra_command(&["add", "s.txt"], &root), "stage s.txt");
        assert_cli_success(&run_libra_command(&["stash", "push"], &root), "stash");
        (repo, root)
    }

    fn assert_status_contains(root: &Path, expected: &[&str], label: &str) {
        let got = status_short(root);
        for line in expected {
            assert!(
                got.lines().any(|actual| actual == *line),
                "{label}: missing `{line}` in status:\n{got}"
            );
        }
    }

    // X1: default pop / apply restage only the new file.
    let (_repo, root) = mixed_stash_repo();
    assert_cli_success(&run_libra_command(&["stash", "pop"], &root), "X1 pop");
    assert_eq!(
        fs::read_to_string(root.join("a.txt")).expect("a"),
        "worktree\n"
    );
    assert_eq!(fs::read_to_string(root.join("s.txt")).expect("s"), "new\n");
    assert_status_contains(&root, &[" M a.txt", "A  s.txt"], "X1 pop");

    let (_repo, root) = mixed_stash_repo();
    assert_cli_success(&run_libra_command(&["stash", "apply"], &root), "X1 apply");
    assert_status_contains(&root, &[" M a.txt", "A  s.txt"], "X1 apply");

    // X2: --index restores both layers (t3903:122).
    let (_repo, root) = mixed_stash_repo();
    assert_cli_success(
        &run_libra_command(&["stash", "apply", "--index"], &root),
        "X2 apply --index",
    );
    assert_status_contains(&root, &["MM a.txt", "A  s.txt"], "X2 apply");
    let (_repo, root) = mixed_stash_repo();
    assert_cli_success(
        &run_libra_command(&["stash", "pop", "--index"], &root),
        "X2 pop --index",
    );
    assert_status_contains(&root, &["MM a.txt", "A  s.txt"], "X2 pop");

    // X3: a changed index refuses --index with no writes.
    let (_repo, root) = mixed_stash_repo();
    fs::write(root.join("a.txt"), "post\n").expect("post");
    assert_cli_success(&run_libra_command(&["add", "a.txt"], &root), "X3 stage");
    let before_a = fs::read_to_string(root.join("a.txt")).expect("a");
    let before_index = fs::read(root.join(".libra/index")).expect("index");
    let failed = run_libra_command(&["stash", "pop", "--index"], &root);
    assert_eq!(failed.status.code(), Some(128), "X3 exit");
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(
        stderr.contains("conflicts in index. Try without --index."),
        "X3 message: {stderr}"
    );
    assert!(stderr.contains("LBR-CONFLICT-001"), "X3 code: {stderr}");
    assert_eq!(
        fs::read_to_string(root.join("a.txt")).expect("a after"),
        before_a
    );
    assert!(!root.join("s.txt").exists(), "X3 must not write s.txt");
    assert_eq!(
        fs::read(root.join(".libra/index")).expect("index after"),
        before_index
    );
    let listed = run_libra_command(&["stash", "list"], &root);
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("stash@{0}"),
        "X3 stash kept: {}",
        String::from_utf8_lossy(&listed.stdout)
    );

    // X4: stash branch uses --index semantics (t3903:284).
    let (_repo, root) = mixed_stash_repo();
    assert_cli_success(
        &run_libra_command(&["stash", "branch", "nb"], &root),
        "X4 branch",
    );
    assert_status_contains(&root, &["MM a.txt", "A  s.txt"], "X4");

    // X5: -q --index is silent on success (t3903:314, :339).
    let (_repo, root) = mixed_stash_repo();
    let quiet_apply = run_libra_command(&["--quiet", "stash", "apply", "--index"], &root);
    assert_cli_success(&quiet_apply, "X5 apply -q");
    assert!(
        quiet_apply.stdout.is_empty() && quiet_apply.stderr.is_empty(),
        "X5 apply quiet"
    );
    let (_repo, root) = mixed_stash_repo();
    let quiet_pop = run_libra_command(&["--quiet", "stash", "pop", "--index"], &root);
    assert_cli_success(&quiet_pop, "X5 pop -q");
    assert!(
        quiet_pop.stdout.is_empty() && quiet_pop.stderr.is_empty(),
        "X5 pop quiet"
    );

    // X6: untracked parent still restores untracked files.
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    fs::write(root.join("a.txt"), "one\n").expect("write");
    assert_cli_success(&run_libra_command(&["add", "a.txt"], root), "X6 add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "X6 commit",
    );
    fs::write(root.join("a.txt"), "two\n").expect("modify");
    fs::write(root.join("u.txt"), "untracked\n").expect("untracked");
    assert_cli_success(&run_libra_command(&["stash", "push", "-u"], root), "X6 -u");
    assert_cli_success(
        &run_libra_command(&["stash", "apply", "--index"], root),
        "X6 apply",
    );
    assert_eq!(
        fs::read_to_string(root.join("u.txt")).expect("u"),
        "untracked\n"
    );

    // X7: stash branch with no stash / no name is zero-write (t3903:643, :672).
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    fs::write(root.join("a.txt"), "one\n").expect("write");
    assert_cli_success(&run_libra_command(&["add", "a.txt"], root), "X7 add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "X7 commit",
    );
    let no_stash = run_libra_command(&["stash", "branch", "nb"], root);
    assert_eq!(no_stash.status.code(), Some(129), "X7 no stash");
    let branches = run_libra_command(&["branch"], root);
    assert!(
        !String::from_utf8_lossy(&branches.stdout).contains("nb"),
        "X7 must not create nb: {}",
        String::from_utf8_lossy(&branches.stdout)
    );
    let no_name = run_libra_command(&["stash", "branch"], root);
    assert_eq!(no_name.status.code(), Some(129), "X7 no name");

    // X8: --json pop --index reports index_restored.
    let (_repo, root) = mixed_stash_repo();
    let json = run_libra_command(&["--json", "stash", "pop", "--index"], &root);
    assert_cli_success(&json, "X8 json");
    let parsed = parse_json_stdout(&json);
    assert_eq!(parsed["data"]["action"], "pop", "X8 action");
    assert_eq!(
        parsed["data"]["index_restored"].as_bool(),
        Some(true),
        "X8 index_restored: {parsed}"
    );
}

/// WT-01 (M-GUARD G3–G6, issues/476): no-change / untracked-only / `-q` / `--all`.
#[test]
fn test_stash_no_change_untracked_only_and_quiet_guards() {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();

    let clean = run_libra_command(&["stash", "push"], root);
    assert_cli_success(&clean, "G3 clean push");
    assert!(
        String::from_utf8_lossy(&clean.stdout).contains("No local changes to save"),
        "G3 clean: {}",
        String::from_utf8_lossy(&clean.stdout)
    );
    let list = run_libra_command(&["stash", "list"], root);
    assert_cli_success(&list, "G3 list after clean");
    assert!(
        String::from_utf8_lossy(&list.stdout).trim().is_empty(),
        "G3 stash list must stay empty: {}",
        String::from_utf8_lossy(&list.stdout)
    );

    fs::write(root.join("only-untracked.txt"), "u\n").expect("untracked");
    let untracked_only = run_libra_command(&["stash", "push"], root);
    assert_cli_success(&untracked_only, "G3 untracked-only");
    assert!(
        String::from_utf8_lossy(&untracked_only.stdout).contains("No local changes to save"),
        "G3 untracked-only: {}",
        String::from_utf8_lossy(&untracked_only.stdout)
    );
    assert!(
        root.join("only-untracked.txt").is_file(),
        "G3 must leave the untracked file in place"
    );
    let list = run_libra_command(&["stash", "list"], root);
    assert!(
        String::from_utf8_lossy(&list.stdout).trim().is_empty(),
        "G3 still no stash: {}",
        String::from_utf8_lossy(&list.stdout)
    );

    let include = run_libra_command(&["stash", "push", "-u"], root);
    assert_cli_success(&include, "G4 -u");
    assert!(
        !root.join("only-untracked.txt").exists(),
        "G4 -u must remove the untracked file"
    );
    let list = run_libra_command(&["stash", "list"], root);
    assert!(
        !String::from_utf8_lossy(&list.stdout).trim().is_empty(),
        "G4 must create a stash: {}",
        String::from_utf8_lossy(&list.stdout)
    );
    assert_cli_success(&run_libra_command(&["stash", "drop"], root), "G4 drop");

    fs::write(root.join("tracked.txt"), "modified\n").expect("dirty");
    let quiet_ok = run_libra_command(&["stash", "push", "-q"], root);
    assert_cli_success(&quiet_ok, "G5 -q success");
    assert!(
        quiet_ok.stdout.is_empty() && quiet_ok.stderr.is_empty(),
        "G5 success must be silent: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&quiet_ok.stdout),
        String::from_utf8_lossy(&quiet_ok.stderr)
    );
    let quiet_noop = run_libra_command(&["stash", "push", "-q"], root);
    assert_cli_success(&quiet_noop, "G5 -q no-change");
    assert!(
        quiet_noop.stdout.is_empty() && quiet_noop.stderr.is_empty(),
        "G5 no-change must be silent: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&quiet_noop.stdout),
        String::from_utf8_lossy(&quiet_noop.stderr)
    );
    assert_cli_success(&run_libra_command(&["stash", "drop"], root), "G5 drop");

    fs::write(root.join(".libraignore"), "ignored.log\n").expect("ignore");
    fs::write(root.join("ignored.log"), "ignored\n").expect("ignored file");
    fs::write(root.join("visible.txt"), "visible\n").expect("visible");
    let all = run_libra_command(&["stash", "push", "--all"], root);
    assert_cli_success(&all, "G6 --all");
    assert!(
        !root.join("ignored.log").exists(),
        "G6 --all must remove the ignored file"
    );
    assert!(
        !root.join("visible.txt").exists(),
        "G6 --all must remove the visible untracked file"
    );
    let list = run_libra_command(&["stash", "list"], root);
    assert!(
        !String::from_utf8_lossy(&list.stdout).trim().is_empty(),
        "G6 must create a stash: {}",
        String::from_utf8_lossy(&list.stdout)
    );
}
