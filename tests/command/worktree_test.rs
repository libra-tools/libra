//! Tests worktree subcommands for core success paths and important error branches.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use clap::Parser;
use libra::{
    command::{
        commit::{self, CommitArgs},
        worktree::{self, WorktreeArgs},
    },
    utils::{output::OutputConfig, test, util},
};
use serde::{Deserialize, Serialize};
use serial_test::serial;
use tempfile::tempdir;

use super::*;

#[test]
#[serial]
fn test_worktree_cli_outside_repository_returns_fatal_128() {
    let temp = tempdir().unwrap();
    let output = run_libra_command(&["worktree", "list"], temp.path());
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal: not a libra repository"),
        "unexpected stderr: {stderr}"
    );
}

/// Regression guard for v0.17.888: `libra worktree --help` previously
/// leaked the implementation-detail rustdoc on `WorktreeArgs` ("CLI
/// arguments for the `worktree` subcommand. This type is wired into
/// the top-level CLI and dispatches to …") into the user-facing help
/// header instead of showing a clean one-liner. Pin the cleaned
/// header and make sure the impl-detail phrasing cannot return.
#[test]
fn test_worktree_help_header_is_user_facing() {
    let temp = tempdir().unwrap();
    let output = run_libra_command(&["worktree", "--help"], temp.path());
    assert_cli_success(&output, "worktree --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("CLI arguments for"),
        "worktree --help leaks impl-detail rustdoc 'CLI arguments for …'. \
         Update WorktreeArgs in src/command/worktree.rs so its doc / \
         long_about reads as user-facing prose. Got:\n{stdout}"
    );
    assert!(
        !stdout.contains("type is wired into"),
        "worktree --help leaks impl-detail rustdoc 'type is wired into …'. \
         See src/command/worktree.rs::WorktreeArgs. Got:\n{stdout}"
    );
    assert!(
        stdout.contains("Manage multiple working trees"),
        "worktree --help missing the user-facing header. Got:\n{stdout}"
    );
}

/// Mirror of the on-disk `WorktreeEntry` used only in tests (registry v2).
///
/// This type allows tests to deserialize `worktrees.json` without depending
/// on internal, non-public structs from the main crate.
#[derive(Clone, Deserialize, Serialize)]
struct TestWorktreeEntry {
    path: String,
    is_main: bool,
    locked: bool,
    lock_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    worktree_id: Option<String>,
}

/// Mirror of the on-disk `WorktreeState` used only in tests (registry v2).
#[derive(Deserialize, Serialize)]
struct TestWorktreeState {
    schema_version: u32,
    entries: Vec<TestWorktreeEntry>,
}

/// Loads the current `worktrees.json` into a test-friendly `TestWorktreeState`.
fn read_worktree_state() -> TestWorktreeState {
    let state_path = util::storage_path().join("worktrees.json");
    let data = fs::read_to_string(state_path).expect("worktrees.json should exist");
    serde_json::from_str(&data).expect("worktrees.json should be valid JSON")
}

/// Returns all worktree paths from the persisted test state.
fn worktree_paths() -> Vec<String> {
    read_worktree_state()
        .entries
        .into_iter()
        .map(|w| w.path)
        .collect()
}

async fn exec_worktree(args: &[&str]) -> libra::CliResult<()> {
    let argv = std::iter::once("worktree")
        .chain(args.iter().copied())
        .collect::<Vec<_>>();
    let parsed = WorktreeArgs::parse_from(argv);
    worktree::execute_safe(parsed, &OutputConfig::default()).await
}

async fn exec_commit(args: &[&str]) -> libra::CliResult<()> {
    let argv = std::iter::once("commit")
        .chain(args.iter().copied())
        .collect::<Vec<_>>();
    let parsed = CommitArgs::parse_from(argv);
    commit::execute_safe(parsed, &OutputConfig::default()).await
}

fn assert_worktree_error(output: &std::process::Output, error_code: &str) -> CliErrorReport {
    assert_ne!(output.status.code(), Some(0), "worktree command must fail");
    assert!(
        output.stdout.is_empty(),
        "failed structured worktree commands must keep stdout clean: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let (_, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, error_code, "unexpected error report");
    report
}

#[tokio::test]
#[serial]
async fn test_worktree_list_json_outputs_structured_entries() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let wt_path = repo_dir.path().join("wt_json");

    let add = run_libra_command(&["worktree", "add", "wt_json"], repo_dir.path());
    assert_cli_success(&add, "worktree add");
    let lock = run_libra_command(
        &["worktree", "lock", "wt_json", "--reason", "review"],
        repo_dir.path(),
    );
    assert_cli_success(&lock, "worktree lock");

    let output = run_libra_command(&["--json", "worktree", "list"], repo_dir.path());
    assert_cli_success(&output, "json worktree list");
    assert!(
        output.stderr.is_empty(),
        "json worktree list should keep stderr clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON on stdout, got: {stdout}\nerror: {e}"));
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["command"], "worktree.list");

    let worktrees = parsed["data"]["worktrees"]
        .as_array()
        .expect("worktrees should be an array");
    let main_path = repo_dir.path().canonicalize().unwrap();
    let main_entry = worktrees
        .iter()
        .find(|entry| entry["path"] == main_path.to_string_lossy().as_ref())
        .expect("json list should include main worktree");
    assert_eq!(main_entry["kind"], "main");
    assert_eq!(main_entry["is_main"], true);
    assert_eq!(main_entry["locked"], false);
    assert_eq!(main_entry["exists"], true);

    let linked_path = wt_path.canonicalize().unwrap();
    let linked_entry = worktrees
        .iter()
        .find(|entry| entry["path"] == linked_path.to_string_lossy().as_ref())
        .expect("json list should include linked worktree");
    assert_eq!(linked_entry["kind"], "worktree");
    assert_eq!(linked_entry["is_main"], false);
    assert_eq!(linked_entry["locked"], true);
    assert_eq!(linked_entry["lock_reason"], "review");
    assert_eq!(linked_entry["exists"], true);
}

#[tokio::test]
#[serial]
async fn test_worktree_list_machine_outputs_single_json_line() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;

    let output = run_libra_command(&["--machine", "worktree", "list"], repo_dir.path());
    assert_cli_success(&output, "machine worktree list");
    assert!(
        output.stderr.is_empty(),
        "machine worktree list should keep stderr clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "machine output should be one JSON line");
    let parsed: serde_json::Value = serde_json::from_str(lines[0])
        .unwrap_or_else(|e| panic!("expected machine JSON line, got: {}\nerror: {e}", lines[0]));
    assert_eq!(parsed["command"], "worktree.list");
}

#[tokio::test]
#[serial]
async fn test_worktree_add_json_reports_created_path() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;

    let output = run_libra_command(
        &["--json", "worktree", "add", "wt_add_json"],
        repo_dir.path(),
    );
    assert_cli_success(&output, "json worktree add");
    assert!(
        output.stderr.is_empty(),
        "json worktree add should keep stderr clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed = parse_json_stdout(&output);
    let canonical = repo_dir.path().join("wt_add_json").canonicalize().unwrap();
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["command"], "worktree.add");
    assert_eq!(parsed["data"]["path"], canonical.to_string_lossy().as_ref());
    assert_eq!(parsed["data"]["already_exists"], false);
}

#[tokio::test]
#[serial]
async fn test_worktree_lock_unlock_structured_outputs_report_state() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let add = run_libra_command(&["worktree", "add", "wt_lock_json"], repo_dir.path());
    assert_cli_success(&add, "worktree add");

    let lock = run_libra_command(
        &[
            "--json",
            "worktree",
            "lock",
            "wt_lock_json",
            "--reason",
            "review",
        ],
        repo_dir.path(),
    );
    assert_cli_success(&lock, "json worktree lock");
    assert!(
        lock.stderr.is_empty(),
        "json worktree lock should keep stderr clean: {}",
        String::from_utf8_lossy(&lock.stderr)
    );
    let locked = parse_json_stdout(&lock);
    let canonical = repo_dir.path().join("wt_lock_json").canonicalize().unwrap();
    assert_eq!(locked["command"], "worktree.lock");
    assert_eq!(locked["data"]["path"], canonical.to_string_lossy().as_ref());
    assert_eq!(locked["data"]["locked"], true);
    assert_eq!(locked["data"]["lock_reason"], "review");
    assert_eq!(locked["data"]["changed"], true);

    let unlock = run_libra_command(
        &["--machine", "worktree", "unlock", "wt_lock_json"],
        repo_dir.path(),
    );
    assert_cli_success(&unlock, "machine worktree unlock");
    assert!(
        unlock.stderr.is_empty(),
        "machine worktree unlock should keep stderr clean: {}",
        String::from_utf8_lossy(&unlock.stderr)
    );
    let stdout = String::from_utf8_lossy(&unlock.stdout);
    let lines = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "machine output should be one JSON line");
    let unlocked: serde_json::Value = serde_json::from_str(lines[0])
        .unwrap_or_else(|e| panic!("expected machine JSON line, got: {}\nerror: {e}", lines[0]));
    assert_eq!(unlocked["command"], "worktree.unlock");
    assert_eq!(
        unlocked["data"]["path"],
        canonical.to_string_lossy().as_ref()
    );
    assert_eq!(unlocked["data"]["locked"], false);
    assert_eq!(unlocked["data"]["changed"], true);
}

#[tokio::test]
#[serial]
async fn test_worktree_move_json_reports_source_and_destination() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let add = run_libra_command(&["worktree", "add", "wt_move_json"], repo_dir.path());
    assert_cli_success(&add, "worktree add");

    let source = repo_dir.path().join("wt_move_json").canonicalize().unwrap();
    let destination = repo_dir.path().join("wt_move_json_dest");
    let output = run_libra_command(
        &[
            "--json",
            "worktree",
            "move",
            "wt_move_json",
            "wt_move_json_dest",
        ],
        repo_dir.path(),
    );
    assert_cli_success(&output, "json worktree move");
    assert!(
        output.stderr.is_empty(),
        "json worktree move should keep stderr clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed = parse_json_stdout(&output);
    let canonical_destination = destination.canonicalize().unwrap();
    assert_eq!(parsed["command"], "worktree.move");
    assert_eq!(parsed["data"]["source"], source.to_string_lossy().as_ref());
    assert_eq!(
        parsed["data"]["destination"],
        canonical_destination.to_string_lossy().as_ref()
    );
    assert_eq!(parsed["data"]["registry_updated"], true);
    assert_eq!(parsed["data"]["disk_directory_moved"], true);
    assert!(!source.exists(), "source worktree should be moved");
    assert!(destination.is_dir(), "destination worktree should exist");
}

#[tokio::test]
#[serial]
async fn test_worktree_prune_machine_reports_pruned_paths() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let add = run_libra_command(&["worktree", "add", "wt_prune_machine"], repo_dir.path());
    assert_cli_success(&add, "worktree add");
    let wt_path = repo_dir.path().join("wt_prune_machine");
    let canonical = wt_path.canonicalize().unwrap();
    fs::remove_dir_all(&wt_path).expect("failed to remove worktree directory before prune");

    let output = run_libra_command(&["--machine", "worktree", "prune"], repo_dir.path());
    assert_cli_success(&output, "machine worktree prune");
    assert!(
        output.stderr.is_empty(),
        "machine worktree prune should keep stderr clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "machine output should be one JSON line");
    let parsed: serde_json::Value = serde_json::from_str(lines[0])
        .unwrap_or_else(|e| panic!("expected machine JSON line, got: {}\nerror: {e}", lines[0]));
    assert_eq!(parsed["command"], "worktree.prune");
    assert_eq!(parsed["data"]["pruned_count"], 1);
    assert_eq!(
        parsed["data"]["pruned"][0],
        canonical.to_string_lossy().as_ref()
    );
}

#[tokio::test]
#[serial]
async fn test_worktree_repair_json_reports_changed_state() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["add", "wt_repair_json"])
        .await
        .expect("worktree add should succeed");

    let mut state = read_worktree_state();
    let duplicate = state
        .entries
        .iter()
        .find(|w| w.path.ends_with("wt_repair_json"))
        .cloned()
        .expect("expected worktree entry for wt_repair_json");
    state.entries.push(duplicate);

    let state_path = util::storage_path().join("worktrees.json");
    let data = serde_json::to_string_pretty(&state)
        .expect("failed to serialize duplicated worktree state");
    fs::write(&state_path, data).expect("failed to overwrite worktrees.json with duplicates");

    let output = run_libra_command(&["--json", "worktree", "repair"], repo_dir.path());
    assert_cli_success(&output, "json worktree repair");
    assert!(
        output.stderr.is_empty(),
        "json worktree repair should keep stderr clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed = parse_json_stdout(&output);
    assert_eq!(parsed["command"], "worktree.repair");
    assert_eq!(parsed["data"]["changed"], true);
}

#[cfg(unix)]
#[test]
#[serial]
fn test_worktree_umount_json_reports_cleanup() {
    let temp = tempdir().expect("create temp dir");
    let cleanup_root = temp
        .path()
        .join("libra-task-worktree-fuse-29353-019ddec6-de60-7383");
    let workspace = cleanup_root.join("workspace");
    fs::create_dir_all(&workspace).expect("create task workspace");
    let canonical_cleanup_root = cleanup_root.canonicalize().expect("canonical cleanup root");
    let canonical_workspace = workspace.canonicalize().expect("canonical workspace");
    let cleanup_arg = cleanup_root.to_string_lossy().to_string();

    let output = run_libra_command(
        &[
            "--json",
            "worktree",
            "umount",
            cleanup_arg.as_str(),
            "--cleanup",
        ],
        temp.path(),
    );

    assert_cli_success(&output, "json worktree umount --cleanup");
    assert!(
        output.stderr.is_empty(),
        "json worktree umount should keep stderr clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed = parse_json_stdout(&output);
    assert_eq!(parsed["command"], "worktree.umount");
    assert_eq!(
        parsed["data"]["mountpoint"],
        canonical_workspace.to_string_lossy().as_ref()
    );
    assert_eq!(parsed["data"]["unmounted"], true);
    assert_eq!(parsed["data"]["cleanup_requested"], true);
    assert_eq!(
        parsed["data"]["cleanup_root"],
        canonical_cleanup_root.to_string_lossy().as_ref()
    );
    assert_eq!(parsed["data"]["cleanup_root_removed"], true);
    assert!(
        !cleanup_root.exists(),
        "cleanup should remove the task worktree root"
    );
}

#[tokio::test]
#[serial]
async fn test_worktree_lock_json_no_such_worktree_reports_invalid_target() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    exec_worktree(&["repair"])
        .await
        .expect("worktree repair should initialize state");
    let before_paths = worktree_paths();

    let output = run_libra_command(
        &["--json", "worktree", "lock", "missing-worktree"],
        repo_dir.path(),
    );

    let report = assert_worktree_error(&output, "LBR-CLI-003");
    assert_eq!(report.category, "cli");
    assert_eq!(report.exit_code, 129);
    assert!(
        report.message.contains("no such worktree"),
        "error should identify missing worktree: {}",
        report.message
    );
    assert_eq!(
        worktree_paths(),
        before_paths,
        "failed lock must not mutate registry"
    );
}

#[tokio::test]
#[serial]
async fn test_worktree_remove_machine_rejects_main_with_stable_error() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    exec_worktree(&["repair"])
        .await
        .expect("worktree repair should initialize state");
    let before_paths = worktree_paths();
    let main_path = repo_dir.path().canonicalize().unwrap();
    let main_arg = main_path.to_string_lossy().to_string();

    let output = run_libra_command(
        &["--machine", "worktree", "remove", main_arg.as_str()],
        repo_dir.path(),
    );

    let report = assert_worktree_error(&output, "LBR-CLI-003");
    assert!(
        report.message.contains("cannot remove main worktree"),
        "error should identify protected main worktree: {}",
        report.message
    );
    assert_eq!(
        worktree_paths(),
        before_paths,
        "failed main remove must not mutate registry"
    );
    assert!(main_path.is_dir(), "main worktree must remain on disk");
}

#[tokio::test]
#[serial]
async fn test_worktree_remove_json_rejects_locked_with_stable_error() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    exec_worktree(&["add", "wt_locked_error"])
        .await
        .expect("worktree add should succeed");
    exec_worktree(&["lock", "wt_locked_error"])
        .await
        .expect("worktree lock should succeed");
    let wt_path = repo_dir.path().join("wt_locked_error");
    let before_paths = worktree_paths();

    let output = run_libra_command(
        &["--json", "worktree", "remove", "wt_locked_error"],
        repo_dir.path(),
    );

    let report = assert_worktree_error(&output, "LBR-CLI-003");
    assert!(
        report.message.contains("cannot remove locked worktree"),
        "error should identify locked worktree: {}",
        report.message
    );
    assert_eq!(
        worktree_paths(),
        before_paths,
        "failed locked remove must not mutate registry"
    );
    assert!(wt_path.is_dir(), "locked worktree must remain on disk");
}

#[tokio::test]
#[serial]
async fn test_worktree_move_machine_destination_exists_reports_conflict() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    exec_worktree(&["add", "wt_move_error"])
        .await
        .expect("worktree add should succeed");
    let src_path = repo_dir.path().join("wt_move_error");
    let dest_path = repo_dir.path().join("wt_move_dest_exists");
    fs::create_dir(&dest_path).expect("create destination collision");
    let before_paths = worktree_paths();

    let output = run_libra_command(
        &[
            "--machine",
            "worktree",
            "move",
            "wt_move_error",
            "wt_move_dest_exists",
        ],
        repo_dir.path(),
    );

    let report = assert_worktree_error(&output, "LBR-CONFLICT-002");
    assert_eq!(report.category, "conflict");
    assert!(
        report.message.contains("destination already exists"),
        "error should identify destination collision: {}",
        report.message
    );
    assert_eq!(
        worktree_paths(),
        before_paths,
        "failed move must not mutate registry"
    );
    assert!(src_path.is_dir(), "source worktree must remain on disk");
    assert!(
        dest_path.is_dir(),
        "destination collision directory must remain on disk"
    );
}

#[tokio::test]
#[serial]
async fn test_worktree_add_json_rejects_storage_path_as_invalid_target() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    exec_worktree(&["repair"])
        .await
        .expect("worktree repair should initialize state");
    let before_paths = worktree_paths();
    let inside_storage = util::storage_path().join("wt_inside_storage");
    let inside_arg = inside_storage.to_string_lossy().to_string();

    let output = run_libra_command(
        &["--json", "worktree", "add", inside_arg.as_str()],
        repo_dir.path(),
    );

    let report = assert_worktree_error(&output, "LBR-CLI-003");
    assert!(
        report
            .message
            .contains("worktree path cannot be inside .libra storage"),
        "error should identify storage path refusal: {}",
        report.message
    );
    assert_eq!(
        worktree_paths(),
        before_paths,
        "failed add must not mutate registry"
    );
    assert!(
        !inside_storage.exists(),
        "failed add must not create the rejected worktree path"
    );
}

#[tokio::test]
#[serial]
async fn test_worktree_list_json_corrupt_state_reports_repo_corrupt() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    exec_worktree(&["repair"])
        .await
        .expect("worktree repair should initialize state");
    let state_path = util::storage_path().join("worktrees.json");
    fs::write(&state_path, b"{ invalid json").expect("failed to corrupt state file");
    let before = fs::read_to_string(&state_path).unwrap();

    let output = run_libra_command(&["--json", "worktree", "list"], repo_dir.path());

    let report = assert_worktree_error(&output, "LBR-REPO-002");
    assert_eq!(report.category, "repo");
    assert!(
        report.message.contains("worktree state") && report.message.contains("is corrupt"),
        "error should identify corrupt worktree state: {}",
        report.message
    );
    let after = fs::read_to_string(&state_path).unwrap();
    assert_eq!(
        after, before,
        "failed state load must not rewrite corrupted worktree state"
    );
}

#[tokio::test]
#[serial]
/// `worktree add` creates a linked directory with a `.libra` storage link.
async fn test_worktree_add_creates_linked_directory() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    let result = exec_worktree(&["add", "wt1"]).await;
    assert!(result.is_ok(), "worktree add failed: {:?}", result.err());

    let wt_path = repo_dir.path().join("wt1");
    assert!(wt_path.is_dir(), "worktree directory should exist");

    let link = wt_path.join(".libra");
    assert!(
        link.exists(),
        ".libra storage link should exist in worktree"
    );
    let metadata = fs::symlink_metadata(&link).unwrap();
    assert!(
        metadata.file_type().is_symlink() || link.is_dir(),
        ".libra should be a directory symlink or a shared storage directory"
    );
}

#[tokio::test]
#[serial]
/// `worktree add` stores a stable canonical path even if input uses a missing parent plus `..`.
async fn test_worktree_add_normalizes_missing_parent_with_dotdot() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["add", "missing_parent/../wt_norm"])
        .await
        .expect("worktree add should succeed");

    let expected = repo_dir.path().join("wt_norm").canonicalize().unwrap();
    let state = read_worktree_state();
    let entry = state
        .entries
        .iter()
        .find(|w| w.path.ends_with("wt_norm"))
        .expect("state should contain the added worktree");
    assert_eq!(
        entry.path,
        expected.to_string_lossy().as_ref(),
        "stored worktree path should be canonical and normalized"
    );

    exec_worktree(&["lock", "wt_norm"])
        .await
        .expect("worktree lock should succeed");
    exec_worktree(&["unlock", "wt_norm"])
        .await
        .expect("worktree unlock should succeed");
    exec_worktree(&["remove", "wt_norm"])
        .await
        .expect("worktree remove should succeed");
}

#[tokio::test]
#[serial]
/// Adding with `../` must still allow later `lock/unlock/remove .` from inside that worktree.
async fn test_worktree_add_parent_relative_then_operate_with_dot_from_linked_worktree() {
    let root_dir = tempdir().unwrap();
    let repo_path = root_dir.path().join("repo");
    fs::create_dir_all(&repo_path).unwrap();
    test::setup_with_new_libra_in(&repo_path).await;

    let _guard_repo = test::ChangeDirGuard::new(&repo_path);
    exec_worktree(&["add", "../wt_lock_dot"])
        .await
        .expect("worktree add with parent-relative path should succeed");

    let linked = root_dir.path().join("wt_lock_dot");
    let _guard_linked = test::ChangeDirGuard::new(&linked);
    exec_worktree(&["lock", "."])
        .await
        .expect("worktree lock with '.' should resolve the registered entry");
    exec_worktree(&["unlock", "."])
        .await
        .expect("worktree unlock with '.' should resolve the registered entry");
    exec_worktree(&["remove", "."])
        .await
        .expect("worktree remove with '.' should resolve the registered entry");
}

#[tokio::test]
#[serial]
/// Adding the same path via `../...` and absolute form should deduplicate to one canonical entry.
async fn test_worktree_add_parent_relative_and_absolute_path_are_equivalent() {
    let root_dir = tempdir().unwrap();
    let repo_path = root_dir.path().join("repo");
    fs::create_dir_all(&repo_path).unwrap();
    test::setup_with_new_libra_in(&repo_path).await;
    let _guard = test::ChangeDirGuard::new(&repo_path);

    exec_worktree(&["add", "../wt_rel_abs"])
        .await
        .expect("first worktree add should succeed");

    let abs_target = root_dir.path().join("wt_rel_abs").canonicalize().unwrap();
    let abs_target_str = abs_target.to_string_lossy().to_string();

    exec_worktree(&["add", abs_target_str.as_str()])
        .await
        .expect("second worktree add with absolute path should succeed");

    let paths = worktree_paths();
    let matches = paths
        .iter()
        .filter(|p| p.as_str() == abs_target.to_string_lossy().as_ref())
        .count();
    assert_eq!(
        matches, 1,
        "same worktree path should only be registered once"
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial]
/// Symlink inputs should be canonicalized to the real target path.
async fn test_worktree_add_symlink_path_is_canonicalized_to_real_path() {
    let root_dir = tempdir().unwrap();
    let repo_path = root_dir.path().join("repo");
    fs::create_dir_all(&repo_path).unwrap();
    test::setup_with_new_libra_in(&repo_path).await;
    let _guard = test::ChangeDirGuard::new(&repo_path);

    let real_target = root_dir.path().join("wt_real");
    fs::create_dir_all(&real_target).unwrap();
    let symlink_path = repo_path.join("wt_link");
    symlink(&real_target, &symlink_path).expect("failed to create symlink for test");

    exec_worktree(&["add", "wt_link"])
        .await
        .expect("worktree add through symlink should succeed");

    let real_canonical = real_target.canonicalize().unwrap();
    let symlink_abs = symlink_path.canonicalize().unwrap();
    assert_eq!(
        real_canonical, symlink_abs,
        "sanity check: symlink should resolve to real target"
    );

    let paths = worktree_paths();
    assert!(
        paths
            .iter()
            .any(|p| p.as_str() == real_canonical.to_string_lossy().as_ref()),
        "state should store canonical real path instead of symlink path"
    );

    exec_worktree(&["lock", "wt_link"])
        .await
        .expect("lock by symlink path should resolve the registered entry");
    exec_worktree(&["unlock", "wt_link"])
        .await
        .expect("unlock by symlink path should resolve the registered entry");
    exec_worktree(&["remove", "wt_link"])
        .await
        .expect("remove by symlink path should resolve the registered entry");
}

#[cfg(unix)]
#[tokio::test]
#[serial]
/// Adding once through a symlinked parent and once through the real path should not create duplicates.
async fn test_worktree_add_symlink_and_real_path_are_deduplicated() {
    let root_dir = tempdir().unwrap();
    let repo_path = root_dir.path().join("repo");
    fs::create_dir_all(&repo_path).unwrap();
    test::setup_with_new_libra_in(&repo_path).await;
    let _guard = test::ChangeDirGuard::new(&repo_path);

    let real_parent = root_dir.path().join("real_parent");
    fs::create_dir_all(&real_parent).unwrap();
    let alias_parent = root_dir.path().join("alias_parent");
    symlink(&real_parent, &alias_parent).expect("failed to create symlink parent");

    let via_symlink = alias_parent.join("wt_dup_sym");
    let via_real = real_parent.join("wt_dup_sym");
    let via_symlink_str = via_symlink.to_string_lossy().to_string();
    let via_real_str = via_real.to_string_lossy().to_string();

    exec_worktree(&["add", via_symlink_str.as_str()])
        .await
        .expect("add via symlinked parent should succeed");
    exec_worktree(&["add", via_real_str.as_str()])
        .await
        .expect("add via real parent should not fail");

    let canonical = via_real.canonicalize().unwrap();
    let paths = worktree_paths();
    let matches = paths
        .iter()
        .filter(|p| p.as_str() == canonical.to_string_lossy().as_ref())
        .count();
    assert_eq!(
        matches, 1,
        "symlink and real paths should deduplicate to one canonical worktree entry"
    );
}

#[tokio::test]
#[serial]
/// Adding into an existing non-empty directory is rejected and preserves local files.
async fn test_worktree_add_rejects_existing_non_empty_directory() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    test::ensure_file("a.txt", Some("repo-version"));
    add::execute(AddArgs {
        pathspec: vec!["a.txt".to_string()],
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
    exec_commit(&["-m", "initial"])
        .await
        .expect("initial commit should succeed");

    let wt_path = repo_dir.path().join("wt_non_empty");
    fs::create_dir_all(&wt_path).expect("failed to create pre-existing worktree target");
    fs::write(wt_path.join("a.txt"), b"local-data")
        .expect("failed to seed pre-existing target content");

    assert!(
        exec_worktree(&["add", "wt_non_empty"]).await.is_err(),
        "adding worktree to non-empty directory should fail"
    );

    assert!(
        !wt_path.join(".libra").exists(),
        "rejected add should not create .libra link in non-empty target"
    );
    let preserved =
        fs::read_to_string(wt_path.join("a.txt")).expect("target file should still exist");
    assert_eq!(
        preserved, "local-data",
        "rejected add should preserve existing directory contents"
    );

    let canonical_target = wt_path.canonicalize().unwrap();
    let paths = worktree_paths();
    assert!(
        !paths
            .iter()
            .any(|p| p == canonical_target.to_string_lossy().as_ref()),
        "rejected add should not register the non-empty target as a worktree"
    );
}

#[tokio::test]
#[serial]
/// Duplicate `worktree add` should not recreate a missing directory when the path is already registered.
async fn test_worktree_add_duplicate_registered_path_does_not_create_directory() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["add", "wt_dup"])
        .await
        .expect("initial worktree add should succeed");

    let wt_path = repo_dir.path().join("wt_dup");
    assert!(wt_path.is_dir());

    fs::remove_dir_all(&wt_path).expect("failed to remove existing worktree directory");
    assert!(!wt_path.exists(), "worktree directory should be missing");

    let before_paths = worktree_paths();
    exec_worktree(&["add", "wt_dup"])
        .await
        .expect("duplicate worktree add command itself should not fail");
    let after_paths = worktree_paths();

    assert_eq!(
        before_paths, after_paths,
        "duplicate add should not mutate registered worktree state"
    );
    assert!(
        !wt_path.exists(),
        "duplicate add should not create a new directory for an already registered path"
    );
}

#[tokio::test]
#[serial]
/// If population fails after writing the link file, `worktree add` rolls back partial artifacts.
async fn test_worktree_add_rolls_back_link_on_restore_failure() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    test::ensure_file("conflict/file.txt", Some("v1"));
    add::execute(AddArgs {
        pathspec: vec!["conflict/file.txt".to_string()],
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
    exec_commit(&["-m", "initial"])
        .await
        .expect("initial commit should succeed");

    let wt_path = repo_dir.path().join("wt_restore_fail");
    fs::create_dir_all(&wt_path).expect("failed to create existing target directory");
    fs::write(wt_path.join("conflict"), b"blocking file")
        .expect("failed to create conflicting path in target");

    assert!(
        exec_worktree(&["add", "wt_restore_fail"]).await.is_err(),
        "adding worktree with conflicting file should fail"
    );

    assert!(
        !wt_path.join(".libra").exists(),
        "failed restore should remove the partial .libra link"
    );

    let canonical_target = wt_path.canonicalize().unwrap();
    let paths = worktree_paths();
    assert!(
        !paths
            .iter()
            .any(|p| p == canonical_target.to_string_lossy().as_ref()),
        "failed restore should not register the worktree in state"
    );
    assert!(
        wt_path.join("conflict").is_file(),
        "pre-existing target content should be preserved on rollback"
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial]
/// If state persistence fails after restore, rollback removes partially restored files in an existing target.
async fn test_worktree_add_rolls_back_populated_files_when_state_save_fails() {
    if skip_permission_denied_test_if_root(
        "test_worktree_add_rolls_back_populated_files_when_state_save_fails",
    ) {
        return;
    }

    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    test::ensure_file("tracked.txt", Some("v1"));
    add::execute(AddArgs {
        pathspec: vec!["tracked.txt".to_string()],
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
    exec_commit(&["-m", "initial"])
        .await
        .expect("initial commit should succeed");

    let wt_path = repo_dir.path().join("wt_state_save_fail");
    fs::create_dir_all(&wt_path).expect("failed to create existing empty target");

    exec_worktree(&["repair"])
        .await
        .expect("worktree repair should initialize worktree state");
    assert!(
        util::storage_path().join("worktrees.json").exists(),
        "worktrees.json should exist before forcing save_state failure"
    );

    let storage_dir = util::storage_path();
    let original_mode = fs::metadata(&storage_dir)
        .expect("failed to stat storage directory")
        .permissions()
        .mode();
    let mut read_only = fs::metadata(&storage_dir)
        .expect("failed to stat storage directory")
        .permissions();
    read_only.set_mode(original_mode & !0o222);
    fs::set_permissions(&storage_dir, read_only)
        .expect("failed to set storage directory read-only");

    assert!(
        exec_worktree(&["add", "wt_state_save_fail"]).await.is_err(),
        "adding worktree with unwritable state should fail"
    );

    let mut restore_mode = fs::metadata(&storage_dir)
        .expect("failed to stat storage directory")
        .permissions();
    restore_mode.set_mode(original_mode);
    fs::set_permissions(&storage_dir, restore_mode)
        .expect("failed to restore storage directory permissions");

    assert!(
        !wt_path.join(".libra").exists(),
        "failed save_state should remove the partial .libra link"
    );
    assert!(
        fs::read_dir(&wt_path)
            .expect("target directory should still exist")
            .next()
            .is_none(),
        "rollback should clear partially restored files from existing target directory"
    );

    let canonical_target = wt_path.canonicalize().unwrap();
    let paths = worktree_paths();
    assert!(
        !paths
            .iter()
            .any(|p| p == canonical_target.to_string_lossy().as_ref()),
        "failed save_state should not register the worktree in state"
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial]
/// Cross-filesystem moves should fail cleanly and keep registry/state unchanged when test env provides separate devices.
async fn test_worktree_move_across_filesystems_rolls_back_when_supported() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["add", "wt_cross_src"])
        .await
        .expect("worktree add should succeed");
    let src_path = repo_dir.path().join("wt_cross_src");
    assert!(src_path.is_dir());

    let other_fs_dir = match tempfile::tempdir_in("/tmp") {
        Ok(d) => d,
        Err(_) => return, // Environment does not allow creating this probe directory.
    };

    let repo_dev = fs::metadata(repo_dir.path()).unwrap().dev();
    let other_dev = fs::metadata(other_fs_dir.path()).unwrap().dev();
    if repo_dev == other_dev {
        return; // Not a cross-filesystem setup on this machine; skip.
    }

    let dest_path = other_fs_dir.path().join("wt_cross_dest");
    let dest_str = dest_path.to_string_lossy().to_string();
    let before_paths = worktree_paths();

    exec_worktree(&["move", "wt_cross_src", dest_str.as_str()])
        .await
        .expect("worktree move command itself should not fail");

    let after_paths = worktree_paths();
    assert_eq!(
        before_paths, after_paths,
        "failed cross-filesystem move should keep worktree registry unchanged"
    );
    assert!(
        src_path.exists(),
        "source directory should remain after failed cross-filesystem move"
    );
    assert!(
        !dest_path.exists(),
        "destination directory should not be created by failed cross-filesystem move"
    );
}

#[tokio::test]
#[serial]
/// Corrupted `worktrees.json` should fail commands gracefully without mutating state or creating directories.
async fn test_worktree_corrupted_state_file_is_handled_without_side_effects() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["repair"])
        .await
        .expect("worktree repair should initialize state first");

    let state_path = util::storage_path().join("worktrees.json");
    fs::write(&state_path, b"{ invalid json").expect("failed to corrupt state file");
    let before = fs::read_to_string(&state_path).unwrap();

    assert!(
        exec_worktree(&["list"]).await.is_err(),
        "listing worktrees with corrupt state should fail"
    );

    let after = fs::read_to_string(&state_path).unwrap();
    assert_eq!(
        before, after,
        "failed state load should not rewrite corrupted worktree state"
    );

    let new_path = repo_dir.path().join("wt_from_corrupt");
    assert!(!new_path.exists());
    assert!(
        exec_worktree(&["add", "wt_from_corrupt"]).await.is_err(),
        "adding worktree with corrupt state should fail"
    );
    assert!(
        !new_path.exists(),
        "add should not create target directory when worktree state cannot be loaded"
    );
}

#[tokio::test]
#[serial]
/// Basic lock/unlock/remove happy path for a non-main worktree.
async fn test_worktree_lock_unlock_and_remove() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["add", "wt2"])
        .await
        .expect("worktree add should succeed");

    exec_worktree(&["lock", "wt2"])
        .await
        .expect("worktree lock should succeed");

    exec_worktree(&["unlock", "wt2"])
        .await
        .expect("worktree unlock should succeed");

    exec_worktree(&["remove", "wt2"])
        .await
        .expect("worktree remove should succeed");
}

#[tokio::test]
#[serial]
/// Creating a worktree must not disturb existing staged changes in the index.
async fn test_worktree_add_does_not_reset_index() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    test::ensure_file("tracked.txt", Some("v1"));
    add::execute(AddArgs {
        pathspec: vec!["tracked.txt".to_string()],
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

    exec_commit(&["-m", "initial"])
        .await
        .expect("initial commit should succeed");

    test::ensure_file("tracked.txt", Some("v2"));
    add::execute(AddArgs {
        pathspec: vec!["tracked.txt".to_string()],
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

    let staged_before = changes_to_be_committed().await;
    assert!(
        staged_before
            .modified
            .iter()
            .any(|p| p.to_str().unwrap() == "tracked.txt"),
        "tracked.txt should be staged before worktree add"
    );

    exec_worktree(&["add", "wt_index"])
        .await
        .expect("worktree add should succeed even when index has staged changes");

    let staged_after = changes_to_be_committed().await;
    assert!(
        staged_after
            .modified
            .iter()
            .any(|p| p.to_str().unwrap() == "tracked.txt"),
        "tracked.txt should remain staged after worktree add"
    );
}

#[tokio::test]
#[serial]
/// New worktree population should use `HEAD` content instead of staged index-only updates.
async fn test_worktree_add_populates_from_head_not_staged_index() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    test::ensure_file("tracked.txt", Some("v1"));
    add::execute(AddArgs {
        pathspec: vec!["tracked.txt".to_string()],
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
    exec_commit(&["-m", "initial"])
        .await
        .expect("initial commit should succeed");

    test::ensure_file("tracked.txt", Some("v2"));
    add::execute(AddArgs {
        pathspec: vec!["tracked.txt".to_string()],
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

    exec_worktree(&["add", "wt_head_content"])
        .await
        .expect("worktree add should succeed");

    let linked_content =
        fs::read_to_string(repo_dir.path().join("wt_head_content").join("tracked.txt")).unwrap();
    assert_eq!(
        linked_content, "v1",
        "new worktree should be populated from HEAD, not staged index updates"
    );
}

#[tokio::test]
#[serial]
/// `worktree list` should include both main and added worktrees and be read-only.
async fn test_worktree_list_includes_main_and_added_worktrees() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["add", "wt_list"])
        .await
        .expect("worktree add should succeed");

    let before_paths = worktree_paths();
    assert!(
        before_paths.iter().any(|p| p.ends_with("wt_list")),
        "state should contain the added worktree"
    );

    exec_worktree(&["list"])
        .await
        .expect("worktree list should succeed");

    let after_paths = worktree_paths();
    assert_eq!(
        before_paths.len(),
        after_paths.len(),
        "worktree list should not mutate state"
    );
}

#[tokio::test]
#[serial]
/// Moving an unlocked, non-main worktree updates both the filesystem and state.
async fn test_worktree_move_moves_unlocked_non_main_worktree() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    let src = repo_dir.path().join("wt_move_src");
    let dest = repo_dir.path().join("wt_move_dest");
    exec_worktree(&["add", "wt_move_src"])
        .await
        .expect("worktree add should succeed");

    assert!(
        src.is_dir(),
        "source directory should exist after worktree add"
    );
    assert!(!dest.exists());

    let src_canonical = src.canonicalize().unwrap();

    exec_worktree(&["move", "wt_move_src", "wt_move_dest"])
        .await
        .expect("worktree move should succeed");

    assert!(!src.exists(), "source directory should be moved away");
    assert!(dest.is_dir(), "destination directory should be created");

    let dest_canonical = dest.canonicalize().unwrap();
    let paths = worktree_paths();
    assert!(
        paths
            .iter()
            .any(|p| p == dest_canonical.to_string_lossy().as_ref()),
        "state should contain moved worktree path"
    );
    assert!(
        !paths
            .iter()
            .any(|p| p == src_canonical.to_string_lossy().as_ref()),
        "state should not contain old worktree path"
    );
}

#[tokio::test]
#[serial]
/// Moving the main worktree is rejected without creating or registering a destination.
async fn test_worktree_move_main_is_rejected_without_side_effects() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["repair"])
        .await
        .expect("worktree repair should initialize worktree state");

    let before_paths = worktree_paths();
    let main_path = repo_dir.path().canonicalize().unwrap();
    assert!(
        before_paths
            .iter()
            .any(|p| p == main_path.to_string_lossy().as_ref()),
        "state should contain main worktree entry"
    );

    let dest = repo_dir.path().join("moved_main");
    assert!(!dest.exists());

    assert!(
        exec_worktree(&["move", ".", "moved_main"]).await.is_err(),
        "moving main worktree should fail"
    );

    assert!(
        !dest.exists(),
        "moving main worktree should not create destination directory"
    );

    let after_paths = worktree_paths();
    assert!(
        after_paths
            .iter()
            .any(|p| p == main_path.to_string_lossy().as_ref()),
        "main worktree should still be present after failed move"
    );
    assert!(
        !after_paths
            .iter()
            .any(|p| p == dest.to_string_lossy().as_ref()),
        "failed move should not register destination as worktree"
    );
}

#[tokio::test]
#[serial]
/// Moving a locked worktree is rejected without changing its path or lock state.
async fn test_worktree_move_locked_is_rejected_without_side_effects() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    let src = repo_dir.path().join("wt_locked");
    let dest = repo_dir.path().join("wt_locked_moved");
    exec_worktree(&["add", "wt_locked"])
        .await
        .expect("worktree add should succeed");

    exec_worktree(&["lock", "wt_locked"])
        .await
        .expect("worktree lock should succeed");

    assert!(src.is_dir());
    assert!(!dest.exists());

    let src_canonical = src.canonicalize().unwrap();

    assert!(
        exec_worktree(&["move", "wt_locked", "wt_locked_moved"])
            .await
            .is_err(),
        "moving locked worktree should fail"
    );

    assert!(
        src.is_dir(),
        "locked worktree directory should remain at original location"
    );
    assert!(
        !dest.exists(),
        "locked worktree move should not create destination directory"
    );

    let state = read_worktree_state();
    let locked_entry = state
        .entries
        .into_iter()
        .find(|w| w.path == src_canonical.to_string_lossy())
        .expect("locked worktree entry should still exist");
    assert!(locked_entry.locked, "worktree should remain locked");
}

#[tokio::test]
#[serial]
/// Moving a worktree onto an existing worktree path is rejected without mutation.
async fn test_worktree_move_rejects_duplicate_destination() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["add", "wt_a"])
        .await
        .expect("first worktree add should succeed");
    exec_worktree(&["add", "wt_b"])
        .await
        .expect("second worktree add should succeed");

    let src = repo_dir.path().join("wt_a");
    let dest = repo_dir.path().join("wt_b");
    assert!(src.is_dir());
    assert!(dest.is_dir());

    let before_paths = worktree_paths();

    assert!(
        exec_worktree(&["move", "wt_a", "wt_b"]).await.is_err(),
        "moving worktree to occupied path should fail"
    );

    assert!(
        src.is_dir(),
        "move to existing worktree should keep source directory"
    );
    assert!(dest.is_dir(), "destination directory should remain");

    let after_paths = worktree_paths();
    assert_eq!(
        before_paths.len(),
        after_paths.len(),
        "duplicate-destination move should not change number of registered worktrees"
    );
}

#[tokio::test]
#[serial]
/// Moving a worktree into `.libra` storage is rejected without mutating filesystem or state.
async fn test_worktree_move_rejects_destination_inside_storage() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["add", "wt_storage_src"])
        .await
        .expect("worktree add should succeed");

    let src = repo_dir.path().join("wt_storage_src");
    let blocked_dest = repo_dir.path().join(".libra").join("moved_inside_storage");
    assert!(src.is_dir());
    assert!(!blocked_dest.exists());

    let before_paths = worktree_paths();

    assert!(
        exec_worktree(&["move", "wt_storage_src", ".libra/moved_inside_storage",])
            .await
            .is_err(),
        "moving worktree into storage should fail"
    );

    assert!(
        src.is_dir(),
        "source directory should remain after rejected move into storage"
    );
    assert!(
        !blocked_dest.exists(),
        "destination inside storage should not be created"
    );

    let after_paths = worktree_paths();
    assert_eq!(
        before_paths, after_paths,
        "rejected move into storage should not mutate worktree registry"
    );
}

#[tokio::test]
#[serial]
/// `worktree prune` removes missing non-main worktrees from the registry.
async fn test_worktree_prune_removes_missing_non_main_worktrees() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["add", "wt_prune"])
        .await
        .expect("worktree add should succeed");

    let wt_path = repo_dir.path().join("wt_prune");
    assert!(wt_path.is_dir());

    fs::remove_dir_all(&wt_path).expect("failed to remove worktree directory");
    assert!(
        !wt_path.exists(),
        "worktree directory should be removed before prune"
    );

    let before_paths = worktree_paths();

    exec_worktree(&["prune"])
        .await
        .expect("worktree prune should succeed");

    let after_paths = worktree_paths();
    assert!(
        after_paths.len() < before_paths.len(),
        "prune should remove missing non-main worktrees"
    );
}

#[tokio::test]
#[serial]
/// `worktree prune` keeps locked worktrees even when their directories are missing.
async fn test_worktree_prune_keeps_locked_worktrees() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["add", "wt_locked_prune"])
        .await
        .expect("worktree add should succeed");
    exec_worktree(&["lock", "wt_locked_prune"])
        .await
        .expect("worktree lock should succeed");

    let wt_path = repo_dir.path().join("wt_locked_prune");
    let canonical = wt_path
        .canonicalize()
        .expect("locked worktree path should canonicalize before removal")
        .to_string_lossy()
        .to_string();
    fs::remove_dir_all(&wt_path).expect("failed to remove locked worktree directory");

    exec_worktree(&["prune"])
        .await
        .expect("worktree prune should succeed");

    let state = read_worktree_state();
    let locked_entry = state
        .entries
        .into_iter()
        .find(|w| w.path == canonical)
        .expect("locked worktree should remain registered");
    assert!(locked_entry.locked, "locked worktree should remain locked");
}

#[tokio::test]
#[serial]
/// Removing a locked worktree is rejected without changing state or directory.
async fn test_worktree_remove_locked_is_rejected_without_side_effects() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["add", "wt_for_remove"])
        .await
        .expect("worktree add should succeed");
    exec_worktree(&["lock", "wt_for_remove"])
        .await
        .expect("worktree lock should succeed");

    let wt_path = repo_dir.path().join("wt_for_remove");
    assert!(wt_path.is_dir());

    let before_paths = worktree_paths();

    assert!(
        exec_worktree(&["remove", "wt_for_remove"]).await.is_err(),
        "removing locked worktree should fail"
    );

    assert!(
        wt_path.is_dir(),
        "locked worktree directory should still exist after failed remove"
    );

    let after_paths = worktree_paths();
    assert_eq!(
        before_paths.len(),
        after_paths.len(),
        "removing locked worktree should not change number of registered worktrees"
    );
}

#[tokio::test]
#[serial]
/// `worktree repair` removes duplicate entries that point to the same path.
async fn test_worktree_repair_deduplicates_entries() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["add", "wt_repair"])
        .await
        .expect("worktree add should succeed");

    let mut state = read_worktree_state();
    let duplicate = state
        .entries
        .iter()
        .find(|w| w.path.ends_with("wt_repair"))
        .cloned()
        .expect("expected worktree entry for wt_repair");
    state.entries.push(duplicate);

    let state_path = util::storage_path().join("worktrees.json");
    let data = serde_json::to_string_pretty(&state)
        .expect("failed to serialize duplicated worktree state");
    fs::write(&state_path, data).expect("failed to overwrite worktrees.json with duplicates");

    exec_worktree(&["repair"])
        .await
        .expect("worktree repair should succeed");

    let repaired = read_worktree_state();
    let paths: Vec<String> = repaired.entries.iter().map(|w| w.path.clone()).collect();
    let unique_paths = paths
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        unique_paths.len(),
        paths.len(),
        "repair should remove duplicate worktree entries"
    );
}

#[tokio::test]
#[serial]
/// `worktree repair` persists main-flag fixes even when there are no duplicate paths.
async fn test_worktree_repair_persists_main_flag_fix_without_duplicates() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["add", "wt_main_fix"])
        .await
        .expect("worktree add should succeed");

    let mut state = read_worktree_state();
    for w in &mut state.entries {
        w.is_main = false;
    }

    let state_path = util::storage_path().join("worktrees.json");
    let data = serde_json::to_string_pretty(&state)
        .expect("failed to serialize worktree state with broken main flags");
    fs::write(&state_path, data).expect("failed to overwrite worktrees.json");

    exec_worktree(&["repair"])
        .await
        .expect("worktree repair should succeed");

    let repaired = read_worktree_state();
    let main_entries: Vec<_> = repaired.entries.iter().filter(|w| w.is_main).collect();
    assert_eq!(
        main_entries.len(),
        1,
        "repair should persist exactly one main worktree flag"
    );
    assert_eq!(
        main_entries[0].path,
        repo_dir.path().canonicalize().unwrap().to_string_lossy(),
        "repair should persist the original repository root as main"
    );
}

#[tokio::test]
#[serial]
/// The main worktree flag remains unique and anchored to the original repo root.
async fn test_worktree_main_flag_remains_single_and_stable() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;

    let _guard = test::ChangeDirGuard::new(repo_dir.path());
    exec_worktree(&["add", "wt_main"])
        .await
        .expect("worktree add should succeed");

    let repo_main = repo_dir.path().canonicalize().unwrap();

    let wt_path = repo_dir.path().join("wt_main");
    let _guard_wt = test::ChangeDirGuard::new(&wt_path);
    exec_worktree(&["list"])
        .await
        .expect("worktree list from linked worktree should succeed");

    let state = read_worktree_state();
    let main_entries: Vec<_> = state.entries.iter().filter(|w| w.is_main).collect();
    assert_eq!(
        main_entries.len(),
        1,
        "there should be exactly one main worktree entry"
    );
    assert_eq!(
        main_entries[0].path,
        repo_main.to_string_lossy(),
        "main worktree entry should remain the original repo directory"
    );
}

// ── C5 surface tests: `worktree remove --delete-dir` ──────────────────────────────────────

#[tokio::test]
#[serial]
/// Default `worktree remove` (no flag) preserves the directory on disk.
async fn test_worktree_remove_default_keeps_disk_directory() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["add", "wt_keep"])
        .await
        .expect("worktree add should succeed");

    let wt_path = repo_dir.path().join("wt_keep");
    assert!(wt_path.is_dir());

    exec_worktree(&["remove", "wt_keep"])
        .await
        .expect("worktree remove (default) should succeed");

    assert!(
        wt_path.is_dir(),
        "default remove must preserve the directory on disk"
    );
    let paths = worktree_paths();
    assert!(
        !paths.iter().any(|p| p.ends_with("wt_keep")),
        "registry should no longer track wt_keep, paths: {paths:?}"
    );
}

#[tokio::test]
#[serial]
/// `worktree remove --json` reports that the registry entry was removed while the directory remained.
async fn test_worktree_remove_json_reports_kept_directory() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["add", "wt_keep_json"])
        .await
        .expect("worktree add should succeed");

    let wt_path = repo_dir.path().join("wt_keep_json");
    let canonical = wt_path.canonicalize().expect("worktree should exist");
    let output = run_libra_command(
        &["--json", "worktree", "remove", "wt_keep_json"],
        repo_dir.path(),
    );
    assert_cli_success(&output, "json worktree remove");
    assert!(
        output.stderr.is_empty(),
        "json worktree remove should keep stderr clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON on stdout, got: {stdout}\nerror: {e}"));
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["command"], "worktree.remove");
    assert_eq!(parsed["data"]["path"], canonical.to_string_lossy().as_ref());
    assert_eq!(parsed["data"]["registry_removed"], true);
    assert_eq!(parsed["data"]["disk_directory_deleted"], false);
    assert!(
        wt_path.is_dir(),
        "json remove without --delete-dir must keep directory"
    );
}

#[tokio::test]
#[serial]
/// `worktree remove --delete-dir` removes both the registry entry and the
/// on-disk directory when the worktree is clean.
async fn test_worktree_remove_with_delete_dir_clean_path() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["add", "wt_delete"])
        .await
        .expect("worktree add should succeed");

    let wt_path = repo_dir.path().join("wt_delete");
    assert!(wt_path.is_dir());

    exec_worktree(&["remove", "--delete-dir", "wt_delete"])
        .await
        .expect("worktree remove --delete-dir on a clean worktree should succeed");

    assert!(
        !wt_path.exists(),
        "--delete-dir must remove the directory on disk"
    );
    let paths = worktree_paths();
    assert!(
        !paths.iter().any(|p| p.ends_with("wt_delete")),
        "registry should no longer track wt_delete, paths: {paths:?}"
    );
}

#[tokio::test]
#[serial]
/// `worktree remove --delete-dir --machine` reports single-line JSON and deletes the directory.
async fn test_worktree_remove_machine_reports_deleted_directory() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["add", "wt_delete_machine"])
        .await
        .expect("worktree add should succeed");

    let wt_path = repo_dir.path().join("wt_delete_machine");
    let canonical = wt_path.canonicalize().expect("worktree should exist");
    let output = run_libra_command(
        &[
            "--machine",
            "worktree",
            "remove",
            "--delete-dir",
            "wt_delete_machine",
        ],
        repo_dir.path(),
    );
    assert_cli_success(&output, "machine worktree remove --delete-dir");
    assert!(
        output.stderr.is_empty(),
        "machine worktree remove should keep stderr clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "machine output should be one JSON line");
    let parsed: serde_json::Value = serde_json::from_str(lines[0])
        .unwrap_or_else(|e| panic!("expected machine JSON line, got: {}\nerror: {e}", lines[0]));
    assert_eq!(parsed["command"], "worktree.remove");
    assert_eq!(parsed["data"]["path"], canonical.to_string_lossy().as_ref());
    assert_eq!(parsed["data"]["registry_removed"], true);
    assert_eq!(parsed["data"]["disk_directory_deleted"], true);
    assert!(
        !wt_path.exists(),
        "machine remove --delete-dir must delete directory"
    );
}

#[tokio::test]
#[serial]
/// `worktree remove --delete-dir` refuses dirty worktrees and leaves both disk and registry intact.
async fn test_worktree_remove_with_delete_dir_dirty_path_is_rejected() {
    let repo_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(repo_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(repo_dir.path());

    exec_worktree(&["add", "wt_dirty_delete"])
        .await
        .expect("worktree add should succeed");

    let wt_path = repo_dir.path().join("wt_dirty_delete");
    fs::write(wt_path.join("dirty.txt"), "dirty\n").expect("failed to dirty worktree");

    let output = run_libra_command(
        &[
            "--json",
            "worktree",
            "remove",
            "--delete-dir",
            "wt_dirty_delete",
        ],
        repo_dir.path(),
    );
    assert_ne!(
        output.status.code(),
        Some(0),
        "dirty --delete-dir must fail"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let report: serde_json::Value = serde_json::from_str(stderr.trim())
        .unwrap_or_else(|e| panic!("expected JSON error on stderr, got: {stderr}\nerror: {e}"));
    assert_eq!(report["ok"], false);
    assert_eq!(report["error_code"], "LBR-CONFLICT-002");
    assert!(
        report["message"]
            .as_str()
            .is_some_and(|message| message.contains("cannot delete dirty worktree")),
        "error should explain dirty worktree refusal: {report}",
    );

    assert!(
        wt_path.is_dir(),
        "dirty rejected --delete-dir must keep the directory on disk"
    );
    let paths = worktree_paths();
    assert!(
        paths.iter().any(|p| p.ends_with("wt_dirty_delete")),
        "dirty rejected --delete-dir must keep registry entry, paths: {paths:?}"
    );
}

#[test]
fn worktree_list_porcelain_emits_attribute_lines() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    let out = run_libra_command(&["worktree", "list", "--porcelain"], p);
    assert_cli_success(&out, "worktree list --porcelain");
    let text = String::from_utf8_lossy(&out.stdout);

    // Git-style porcelain: a `worktree <path>` line, this worktree's own
    // `HEAD <sha>` line, and — since the main worktree is on a branch — a
    // `branch refs/heads/<name>` line, terminated by a blank line (Part C
    // §C.3.3: each worktree reports its own HEAD, not a shared one).
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    assert!(
        first.starts_with("worktree "),
        "first line is `worktree <path>`: {text:?}"
    );
    assert!(
        text.lines().any(|l| l.starts_with("HEAD ")),
        "the worktree's HEAD line is present: {text:?}"
    );
    assert!(
        text.lines().any(|l| l.starts_with("branch refs/heads/")),
        "the main worktree's branch line is present: {text:?}"
    );
    assert!(
        text.ends_with("\n\n"),
        "entry is terminated by a blank line: {text:?}"
    );
}
