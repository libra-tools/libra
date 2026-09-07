//! Integration tests for `libra op` log/show/restore flows.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::path::Path;

use libra::{
    internal::{
        branch::Branch,
        db::get_db_conn_instance,
        head::Head,
        operation::{OperationService, OperationViewRefRecord},
    },
    utils::test::ChangeDirGuard,
};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::Value;

use super::*;

/// Run `libra op` in JSON mode and parse the command output.
fn run_json_op(repo: &Path, args: &[&str]) -> Value {
    let mut full_args = vec!["--json", "op"];
    full_args.extend_from_slice(args);

    let output = run_libra_command(&full_args, repo);
    assert_cli_success(&output, "op json command should succeed");
    parse_json_stdout(&output)
}

/// Return the newest operation id recorded in the repository.
fn latest_operation_id(repo: &Path) -> String {
    run_json_op(repo, &["log", "-n", "1"])["data"]["operations"][0]["op_id"]
        .as_str()
        .expect("expected latest operation id")
        .to_string()
}

/// Return the total number of operations visible in the repository log.
fn listed_operation_count(repo: &Path) -> u64 {
    run_json_op(repo, &["log", "-n", "20"])["data"]["total"]
        .as_u64()
        .expect("expected operation count")
}

/// Assert the stable invalid-target CLI error contract for `op show`/`op restore`.
fn assert_invalid_target_error(output: &std::process::Output, expected_message: &str) {
    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(129));
    assert!(
        human.contains(expected_message),
        "unexpected stderr: {human}"
    );
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert_eq!(report.category, "cli");
    assert_eq!(report.exit_code, 129);
}

#[test]
/// Verifies that JSON `op log` output is ordered newest-first and reports totals.
fn test_op_log_json_lists_latest_operations_newest_first() {
    let repo = create_committed_repo_via_cli();

    let feature = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&feature, "branch feature");

    let topic = run_libra_command(&["branch", "topic"], repo.path());
    assert_cli_success(&topic, "branch topic");

    let json = run_json_op(repo.path(), &["log", "-n", "10"]);
    assert_eq!(json["command"], Value::String("op".to_string()));

    let data = &json["data"];
    assert_eq!(data["action"], Value::String("log".to_string()));
    assert_eq!(data["page"], Value::from(1));
    assert_eq!(data["per_page"], Value::from(10));
    assert_eq!(data["total"], Value::from(2));

    let operations = data["operations"]
        .as_array()
        .expect("expected operations array");
    assert_eq!(operations.len(), 2);
    assert_eq!(operations[0]["command_name"], "branch");
    assert_eq!(operations[0]["status"], "succeeded");
    assert!(
        operations[0]["description"]
            .as_str()
            .expect("expected description")
            .contains("create branch topic")
    );
    assert!(
        operations[1]["description"]
            .as_str()
            .expect("expected description")
            .contains("create branch feature")
    );
}

#[test]
/// Verifies that verbose log rendering includes the core metadata fields.
fn test_op_log_verbose_includes_core_fields() {
    let repo = create_committed_repo_via_cli();

    let feature = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&feature, "branch feature");

    let output = run_libra_command(&["op", "log", "-n", "1", "--verbose"], repo.path());
    assert_cli_success(&output, "op log --verbose");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("command: branch"),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("description: create branch feature"),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("actor: Test User"),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("status: succeeded"),
        "unexpected stdout: {stdout}"
    );
}

#[test]
/// Verifies that page-two JSON log output returns the next older operation.
fn test_op_log_json_page_two_returns_older_operation() {
    let repo = create_committed_repo_via_cli();

    for branch_name in ["feature", "topic", "release"] {
        let output = run_libra_command(&["branch", branch_name], repo.path());
        assert_cli_success(&output, branch_name);
    }

    let page_one = run_json_op(repo.path(), &["log", "-n", "1", "--page", "1"]);
    let page_two = run_json_op(repo.path(), &["log", "-n", "1", "--page", "2"]);

    assert_eq!(page_one["data"]["page"], Value::from(1));
    assert_eq!(page_one["data"]["per_page"], Value::from(1));
    assert_eq!(page_one["data"]["total"], Value::from(3));
    assert_eq!(page_two["data"]["page"], Value::from(2));
    assert_eq!(page_two["data"]["per_page"], Value::from(1));
    assert_eq!(page_two["data"]["total"], Value::from(3));

    let first = &page_one["data"]["operations"][0];
    let second = &page_two["data"]["operations"][0];
    assert_ne!(first["op_id"], second["op_id"]);
    assert!(
        first["description"]
            .as_str()
            .expect("expected description")
            .contains("create branch release")
    );
    assert!(
        second["description"]
            .as_str()
            .expect("expected description")
            .contains("create branch topic")
    );
}

#[test]
/// `op restore` reproduces the target operation's exact branch set: a branch
/// created after the target operation is pruned on restore, while the branch
/// present in the target view (the default branch) survives.
fn test_op_restore_prunes_branches_absent_from_target_view() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // The default branch present at the snapshot point must survive the restore.
    let current = run_libra_command(&["branch", "--show-current"], p);
    let default_branch = String::from_utf8_lossy(&current.stdout).trim().to_string();
    assert!(!default_branch.is_empty(), "expected a current branch");

    // Establish a base operation whose view contains `keep` (and the default
    // branch) but not the yet-to-be-created `ephemeral`. Operations are only
    // recorded for ref-mutating commands, so a branch creation is the snapshot.
    assert_cli_success(&run_libra_command(&["branch", "keep"], p), "branch keep");
    let base_op = latest_operation_id(p);

    // Create a branch that is absent from the base operation's view.
    assert_cli_success(
        &run_libra_command(&["branch", "ephemeral"], p),
        "branch ephemeral",
    );
    let before = run_libra_command(&["branch"], p);
    assert!(
        String::from_utf8_lossy(&before.stdout).contains("ephemeral"),
        "the extra branch should exist before restore"
    );

    // Restore to the base operation — `ephemeral` must be pruned, while both the
    // default branch and `keep` (present in the target view) survive.
    let restore = run_json_op(p, &["restore", &base_op]);
    assert_eq!(restore["data"]["action"], "restore");

    let after = run_libra_command(&["branch"], p);
    let after_out = String::from_utf8_lossy(&after.stdout);
    assert!(
        !after_out.contains("ephemeral"),
        "the branch absent from the target view should be pruned: {after_out}"
    );
    assert!(
        after_out.contains(&default_branch),
        "the default branch present in the target view should survive: {after_out}"
    );
    assert!(
        after_out.contains("keep"),
        "the non-HEAD branch present in the target view should survive: {after_out}"
    );
}

#[test]
/// Part C W0 (§C.11): `op restore` refuses to prune/rewrite a branch that is
/// checked out in ANOTHER worktree — it would leave that worktree's HEAD
/// dangling. The guard fires before any write.
fn test_op_restore_refuses_branch_checked_out_in_another_worktree() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // Base operation whose view contains the default branch but not `ephemeral`.
    assert_cli_success(&run_libra_command(&["branch", "keep"], p), "branch keep");
    let base_op = latest_operation_id(p);
    assert_cli_success(
        &run_libra_command(&["branch", "ephemeral"], p),
        "branch ephemeral",
    );

    // A linked worktree checks out `ephemeral` (the branch restore would prune).
    let parent = tempdir().unwrap();
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], p),
        "worktree add",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "ephemeral"], &wt),
        "wt switch ephemeral",
    );

    // Restoring to base_op would prune `ephemeral`, but it is checked out in the
    // linked worktree — the restore must be refused before any write.
    let restore = run_libra_command(&["op", "restore", &base_op], p);
    assert_ne!(
        restore.status.code(),
        Some(0),
        "op restore must refuse to prune a branch checked out elsewhere"
    );
    assert!(
        String::from_utf8_lossy(&restore.stderr).contains("checked out"),
        "names the collision: {}",
        String::from_utf8_lossy(&restore.stderr)
    );
    // `ephemeral` must still exist (no write happened).
    let after = run_libra_command(&["branch"], p);
    assert!(
        String::from_utf8_lossy(&after.stdout).contains("ephemeral"),
        "no branch was pruned"
    );
}

#[test]
/// `op restore --dry-run` previews the branches it would prune but performs no
/// writes — the branch absent from the target view must still exist afterward.
fn test_op_restore_dry_run_previews_prune_without_deleting() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    assert_cli_success(&run_libra_command(&["branch", "keep"], p), "branch keep");
    let base_op = latest_operation_id(p);
    assert_cli_success(
        &run_libra_command(&["branch", "ephemeral"], p),
        "branch ephemeral",
    );

    // Dry-run should NAME the branch it would prune...
    let preview = run_libra_command(&["op", "restore", &base_op, "--dry-run"], p);
    assert_cli_success(&preview, "op restore --dry-run");
    let preview_out = String::from_utf8_lossy(&preview.stdout);
    assert!(
        preview_out.contains("would be pruned") && preview_out.contains("ephemeral"),
        "dry-run should preview the prune of `ephemeral`: {preview_out}"
    );

    // ...but must NOT actually delete it.
    let after = run_libra_command(&["branch"], p);
    assert!(
        String::from_utf8_lossy(&after.stdout).contains("ephemeral"),
        "dry-run must not prune the branch"
    );
}

#[test]
/// Verifies that command filtering happens before pagination and preserves totals.
fn test_op_log_json_command_filter_preserves_filtered_total_across_pages() {
    let repo = create_committed_repo_via_cli();

    for branch_name in ["feature", "topic", "release"] {
        let output = run_libra_command(&["branch", branch_name], repo.path());
        assert_cli_success(&output, branch_name);
    }

    let target_op_id = latest_operation_id(repo.path());
    let restore = run_json_op(repo.path(), &["restore", &target_op_id]);
    assert_eq!(restore["data"]["action"], "restore");

    let page_one = run_json_op(
        repo.path(),
        &["log", "-n", "2", "--page", "1", "--command", "branch"],
    );
    let page_two = run_json_op(
        repo.path(),
        &["log", "-n", "2", "--page", "2", "--command", "branch"],
    );
    let restore_only = run_json_op(
        repo.path(),
        &["log", "-n", "10", "--page", "1", "--command", "op restore"],
    );

    assert_eq!(page_one["data"]["total"], Value::from(3));
    assert_eq!(page_one["data"]["page"], Value::from(1));
    assert_eq!(page_one["data"]["per_page"], Value::from(2));
    assert_eq!(
        page_one["data"]["operations"]
            .as_array()
            .expect("operations")
            .len(),
        2
    );
    assert_eq!(page_one["data"]["operations"][0]["command_name"], "branch");
    assert_eq!(page_one["data"]["operations"][1]["command_name"], "branch");
    assert!(
        page_one["data"]["operations"][0]["description"]
            .as_str()
            .expect("expected description")
            .contains("create branch release")
    );
    assert!(
        page_one["data"]["operations"][1]["description"]
            .as_str()
            .expect("expected description")
            .contains("create branch topic")
    );

    assert_eq!(page_two["data"]["total"], Value::from(3));
    assert_eq!(page_two["data"]["page"], Value::from(2));
    assert_eq!(page_two["data"]["per_page"], Value::from(2));
    assert_eq!(
        page_two["data"]["operations"]
            .as_array()
            .expect("operations")
            .len(),
        1
    );
    assert_eq!(page_two["data"]["operations"][0]["command_name"], "branch");
    assert!(
        page_two["data"]["operations"][0]["description"]
            .as_str()
            .expect("expected description")
            .contains("create branch feature")
    );

    assert_eq!(restore_only["data"]["total"], Value::from(1));
    assert_eq!(
        restore_only["data"]["operations"][0]["command_name"],
        "op restore"
    );
}

#[test]
/// Verifies that human log output keeps the global `@{n}` index across filtered pages.
fn test_op_log_human_page_two_uses_filtered_global_index() {
    let repo = create_committed_repo_via_cli();

    for branch_name in ["feature", "topic", "release"] {
        let output = run_libra_command(&["branch", branch_name], repo.path());
        assert_cli_success(&output, branch_name);
    }

    let output = run_libra_command(
        &["op", "log", "-n", "1", "--page", "2", "--command", "branch"],
        repo.path(),
    );
    assert_cli_success(&output, "op log page 2 branch filter");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("shown 1"), "unexpected stdout: {stdout}");
    assert!(
        stdout.contains("@{1} branch"),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("create branch topic"),
        "unexpected stdout: {stdout}"
    );
}

#[test]
/// Verifies `switch -c` does not record a branch-only intermediate snapshot.
fn test_switch_create_branch_does_not_record_branch_operation() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["switch", "-c", "feature"], repo.path());
    assert_cli_success(&output, "switch -c feature");

    let json = run_json_op(repo.path(), &["log", "-n", "10", "--command", "branch"]);
    assert_eq!(json["data"]["total"], Value::from(0));
    assert_eq!(
        json["data"]["operations"]
            .as_array()
            .expect("operations array")
            .len(),
        0
    );
}

#[test]
/// Verifies that a command filter with no matches returns an empty JSON page.
fn test_op_log_json_no_match_filter_returns_empty_page() {
    let repo = create_committed_repo_via_cli();

    let feature = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&feature, "branch feature");

    let json = run_json_op(repo.path(), &["log", "-n", "5", "--command", "merge"]);
    assert_eq!(json["data"]["page"], Value::from(1));
    assert_eq!(json["data"]["per_page"], Value::from(5));
    assert_eq!(json["data"]["total"], Value::from(0));
    assert_eq!(
        json["data"]["operations"]
            .as_array()
            .expect("operations array")
            .len(),
        0
    );
}

#[test]
/// Verifies that page `0` and page-size `0` are normalized to safe defaults.
fn test_op_log_normalizes_zero_page_and_page_size() {
    let repo = create_committed_repo_via_cli();

    let feature = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&feature, "branch feature");

    let json = run_json_op(repo.path(), &["log", "-n", "0", "--page", "0"]);
    assert_eq!(json["data"]["page"], Value::from(1));
    assert_eq!(json["data"]["per_page"], Value::from(50));
    assert_eq!(json["data"]["total"], Value::from(1));
    assert_eq!(
        json["data"]["operations"]
            .as_array()
            .expect("operations array")
            .len(),
        1
    );
}

#[test]
/// Verifies that `op show @{0}` resolves to the newest recorded operation.
fn test_op_show_json_latest_index_resolves_to_branch_operation() {
    let repo = create_committed_repo_via_cli();

    let feature = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&feature, "branch feature");

    let json = run_json_op(repo.path(), &["show", "@{0}"]);
    assert_eq!(json["command"], Value::String("op".to_string()));

    let data = &json["data"];
    assert_eq!(data["action"], Value::String("show".to_string()));
    assert_eq!(data["command_name"], "branch");
    assert_eq!(data["actor"], "Test User");
    assert_eq!(data["status"], "succeeded");
    assert!(
        data["description"]
            .as_str()
            .expect("expected description")
            .contains("create branch feature")
    );
    assert!(data["op_id"].as_str().expect("expected op id").len() > 8);
    assert!(data["view_id"].as_str().expect("expected view id").len() > 8);
}

#[test]
/// Verifies that `op show --view` prints the captured snapshot refs and HEAD.
fn test_op_show_view_human_includes_snapshot_refs() {
    let repo = create_committed_repo_via_cli();

    let feature = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&feature, "branch feature");

    let output = run_libra_command(&["op", "show", "@{0}", "--view"], repo.path());
    assert_cli_success(&output, "op show --view");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("View Snapshot:"),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("HEAD: main (branch)"),
        "unexpected stdout: {stdout}"
    );
    assert!(stdout.contains("feature:"), "unexpected stdout: {stdout}");
    assert!(stdout.contains("main:"), "unexpected stdout: {stdout}");
}

#[test]
/// Verifies that out-of-range indexed references map to the invalid-target error contract.
fn test_op_show_out_of_range_index_reports_invalid_target() {
    let repo = create_committed_repo_via_cli();

    let feature = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&feature, "branch feature");

    let output = run_libra_command(&["op", "show", "@{99}"], repo.path());
    let (_human, report) = parse_cli_error_stderr(&output.stderr);

    assert_invalid_target_error(&output, "fatal: operation index 99 out of range");
    assert_eq!(
        report.hints,
        vec!["use 'libra op log' to see available operations"]
    );
}

#[test]
/// Verifies that malformed indexed references return the invalid-arguments contract.
fn test_op_show_invalid_index_format_reports_invalid_arguments() {
    let repo = create_committed_repo_via_cli();

    let feature = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&feature, "branch feature");

    let output = run_libra_command(&["op", "show", "@{abc}"], repo.path());
    let (human, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert!(
        human.contains("fatal: invalid operation index: @{abc}"),
        "unexpected stderr: {human}"
    );
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert_eq!(report.category, "cli");
    assert_eq!(report.exit_code, 129);
    assert!(report.hints.is_empty());
}

#[test]
/// Verifies that a missing direct operation id returns the invalid-target contract.
fn test_op_show_unknown_operation_id_reports_invalid_target() {
    let repo = create_committed_repo_via_cli();

    let feature = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&feature, "branch feature");

    let missing_id = "00000000-0000-0000-0000-000000000000";
    let output = run_libra_command(&["op", "show", missing_id], repo.path());
    let (_human, report) = parse_cli_error_stderr(&output.stderr);

    assert_invalid_target_error(
        &output,
        "fatal: operation '00000000-0000-0000-0000-000000000000' not found",
    );
    assert_eq!(
        report.hints,
        vec!["use 'libra op log' to list available operations"]
    );
}

#[test]
/// Verifies that `op restore --dry-run` previews work without recording a new operation.
fn test_op_restore_dry_run_does_not_record_new_operation() {
    let repo = create_committed_repo_via_cli();

    let feature = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&feature, "branch feature");

    let before = listed_operation_count(repo.path());
    let output = run_libra_command(&["op", "restore", "@{0}", "--dry-run"], repo.path());
    assert_cli_success(&output, "op restore --dry-run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Would restore to operation"),
        "unexpected stdout: {stdout}"
    );

    let after = listed_operation_count(repo.path());
    assert_eq!(after, before, "dry-run must not append a new operation");
}

#[test]
/// Verifies that an out-of-range restore target fails without appending history.
fn test_op_restore_out_of_range_index_reports_invalid_target() {
    let repo = create_committed_repo_via_cli();

    let feature = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&feature, "branch feature");

    let before = listed_operation_count(repo.path());
    let output = run_libra_command(&["op", "restore", "@{99}"], repo.path());
    let (_human, report) = parse_cli_error_stderr(&output.stderr);

    assert_invalid_target_error(&output, "fatal: operation index 99 out of range");
    assert_eq!(
        report.hints,
        vec!["use 'libra op log' to see available operations"]
    );
    assert_eq!(listed_operation_count(repo.path()), before);
}

#[test]
/// Verifies that restore rejects a dirty worktree unless `--force` is supplied.
fn test_op_restore_dirty_worktree_is_rejected_without_recording_new_operation() {
    let repo = create_committed_repo_via_cli();

    let feature = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&feature, "branch feature");

    std::fs::write(repo.path().join("tracked.txt"), "tracked\ndirty change\n")
        .expect("failed to dirty tracked file");

    let before = listed_operation_count(repo.path());
    let output = run_libra_command(&["op", "restore", "@{0}"], repo.path());
    let (human, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert!(
        human.contains("fatal: working tree has uncommitted changes"),
        "unexpected stderr: {human}"
    );
    assert_eq!(report.error_code, "LBR-CONFLICT-001");
    assert_eq!(report.category, "conflict");
    assert_eq!(report.exit_code, 128);
    assert_eq!(
        report.hints,
        vec!["use --force to restore anyway, or commit/stash changes first"]
    );

    let after = listed_operation_count(repo.path());
    assert_eq!(
        after, before,
        "rejected restore must not record a new operation"
    );
}

#[tokio::test]
#[serial(cwd)]
/// Verifies that `op restore --force` proceeds on a dirty worktree and records history.
async fn test_op_restore_force_allows_dirty_worktree_and_emits_confirmation() {
    let repo = create_committed_repo_via_cli();

    let branch_output = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&branch_output, "branch feature");
    let target_op_id = latest_operation_id(repo.path());

    let _guard = ChangeDirGuard::new(repo.path());
    let base_commit = Head::current_commit()
        .await
        .expect("expected base HEAD commit")
        .to_string();

    let switch_output = run_libra_command(&["switch", "feature"], repo.path());
    assert_cli_success(&switch_output, "switch feature");

    std::fs::write(repo.path().join("tracked.txt"), "tracked\nfeature commit\n")
        .expect("failed to update tracked file");
    let add_output = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert_cli_success(&add_output, "add tracked.txt");
    let commit_output = run_libra_command(
        &["commit", "-m", "feature update", "--no-verify"],
        repo.path(),
    );
    assert_cli_success(&commit_output, "commit feature update");

    std::fs::write(
        repo.path().join("tracked.txt"),
        "tracked\nfeature commit\ndirty worktree\n",
    )
    .expect("failed to dirty tracked file");

    let before = listed_operation_count(repo.path());
    let output = run_libra_command(&["op", "restore", &target_op_id, "--force"], repo.path());
    assert_cli_success(&output, "op restore --force");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Restored to operation"),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("New operation recorded:"),
        "unexpected stdout: {stdout}"
    );
    assert_eq!(listed_operation_count(repo.path()), before + 1);

    match Head::current().await {
        Head::Branch(branch_name) => assert_eq!(branch_name, "main"),
        other => panic!("expected HEAD to restore to main branch, got {other:?}"),
    }

    let feature_branch = Branch::find_branch_result("feature", None)
        .await
        .expect("feature branch lookup should succeed")
        .expect("feature branch should still exist after force restore");
    assert_eq!(feature_branch.commit.to_string(), base_commit);
}

#[test]
/// Verifies the first-batch happy path across `op log`, `op show`, and `op restore --dry-run`.
fn test_op_command_smoke_flow_covers_first_batch_chain() {
    let repo = create_committed_repo_via_cli();

    let branch_output = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&branch_output, "branch feature");

    let log_output = run_libra_command(&["op", "log", "-n", "10"], repo.path());
    assert_cli_success(&log_output, "op log");
    let log_stdout = String::from_utf8_lossy(&log_output.stdout);
    assert!(
        log_stdout.contains("branch"),
        "unexpected stdout: {log_stdout}"
    );

    let show_output = run_libra_command(&["op", "show", "@{0}"], repo.path());
    assert_cli_success(&show_output, "op show @{0}");
    let show_stdout = String::from_utf8_lossy(&show_output.stdout);
    assert!(
        show_stdout.contains("Command: branch"),
        "unexpected stdout: {show_stdout}"
    );

    let restore_output = run_libra_command(&["op", "restore", "@{0}", "--dry-run"], repo.path());
    assert_cli_success(&restore_output, "op restore --dry-run");
    let restore_stdout = String::from_utf8_lossy(&restore_output.stdout);
    assert!(
        restore_stdout.contains("Would restore to operation"),
        "unexpected stdout: {restore_stdout}"
    );
}

#[tokio::test]
#[serial(cwd)]
/// Verifies that JSON restore updates HEAD and refs while recording a new restore operation.
async fn test_op_restore_json_records_new_operation_and_restores_head_and_branch_ref() {
    let repo = create_committed_repo_via_cli();

    let branch_output = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&branch_output, "branch feature");
    let target_op_id = latest_operation_id(repo.path());

    let _guard = ChangeDirGuard::new(repo.path());
    let base_commit = Head::current_commit()
        .await
        .expect("expected base HEAD commit")
        .to_string();

    let switch_output = run_libra_command(&["switch", "feature"], repo.path());
    assert_cli_success(&switch_output, "switch feature");

    std::fs::write(repo.path().join("tracked.txt"), "tracked\nfeature change\n")
        .expect("failed to update tracked file");

    let add_output = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert_cli_success(&add_output, "add tracked.txt");

    let commit_output = run_libra_command(
        &["commit", "-m", "feature update", "--no-verify"],
        repo.path(),
    );
    assert_cli_success(&commit_output, "commit feature update");

    let feature_tip_before_restore = Head::current_commit()
        .await
        .expect("expected feature HEAD commit")
        .to_string();
    assert_ne!(feature_tip_before_restore, base_commit);

    let restore_json = run_json_op(repo.path(), &["restore", &target_op_id]);
    assert_eq!(restore_json["command"], Value::String("op".to_string()));
    assert_eq!(
        restore_json["data"]["action"],
        Value::String("restore".to_string())
    );
    assert_eq!(restore_json["data"]["target_op_id"], target_op_id);

    let new_op_id = restore_json["data"]["new_op_id"]
        .as_str()
        .expect("expected new operation id")
        .to_string();
    assert_ne!(
        new_op_id, target_op_id,
        "restore must record a new operation"
    );

    match Head::current().await {
        Head::Branch(branch_name) => assert_eq!(branch_name, "main"),
        other => panic!("expected HEAD to restore to main branch, got {other:?}"),
    }

    let restored_head = Head::current_commit()
        .await
        .expect("expected restored HEAD commit")
        .to_string();
    assert_eq!(restored_head, base_commit);

    let feature_branch = Branch::find_branch_result("feature", None)
        .await
        .expect("feature branch lookup should succeed")
        .expect("feature branch should still exist after restore");
    assert_eq!(feature_branch.commit.to_string(), base_commit);

    let latest_log = run_json_op(repo.path(), &["log", "-n", "1"]);
    assert_eq!(latest_log["data"]["operations"][0]["op_id"], new_op_id);
    assert_eq!(
        latest_log["data"]["operations"][0]["command_name"],
        "op restore"
    );
}

#[tokio::test]
#[serial(cwd)]
/// Legacy operation views may contain Memory, but restore must preserve its authority and watermark.
async fn op_restore_preserves_memory_ref_and_projection_watermark() {
    let repo = create_committed_repo_via_cli();
    let branch_output = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&branch_output, "branch feature");
    let target_op_id = latest_operation_id(repo.path());

    let _guard = ChangeDirGuard::new(repo.path());
    let current_oid = Head::current_commit()
        .await
        .expect("current commit")
        .to_string();
    Branch::update_branch("libra/memory/repo", &current_oid, None)
        .await
        .expect("seed Memory authority");

    let db = get_db_conn_instance().await;
    let graph = OperationService::load_restore_view_by_operation_with_conn(&db, &target_op_id)
        .await
        .expect("load target operation")
        .expect("target operation graph");
    let mut refs = graph.refs.clone();
    refs.push(OperationViewRefRecord {
        view_id: graph.view.view_id.clone(),
        ref_kind: "branch".to_string(),
        ref_name: "libra/memory/repo".to_string(),
        ref_remote: None,
        target_oid: "2222222222222222222222222222222222222222".to_string(),
    });
    OperationService::replace_view_refs_with_conn(&db, &graph.view.view_id, &refs)
        .await
        .expect("seed legacy operation view");
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO memory_projection_state(scope_key, projected_ref_oid, last_event_seq, schema_version, policy_version, rebuilt_at) VALUES (?, ?, 42, 1, 'repo-default-v1', 1)",
        ["test-scope".into(), current_oid.clone().into()],
    ))
    .await
    .expect("seed projection watermark");

    let preview = run_json_op(repo.path(), &["restore", &target_op_id, "--dry-run"]);
    assert_eq!(preview["data"]["action"], "restore-preview");
    assert_eq!(
        preview["data"]["skipped_owned_refs"],
        serde_json::json!(["libra/memory/repo"])
    );

    let restored = run_json_op(repo.path(), &["restore", &target_op_id]);
    assert_eq!(
        restored["data"]["skipped_owned_refs"],
        serde_json::json!(["libra/memory/repo"])
    );
    let memory = Branch::find_branch_result("libra/memory/repo", None)
        .await
        .expect("query Memory authority")
        .expect("Memory authority remains");
    assert_eq!(memory.commit.to_string(), current_oid);

    let row = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT projected_ref_oid, last_event_seq FROM memory_projection_state WHERE scope_key = 'test-scope'"
                .to_string(),
        ))
        .await
        .expect("query watermark")
        .expect("watermark row");
    let projected: String = row.try_get("", "projected_ref_oid").unwrap();
    let sequence: i64 = row.try_get("", "last_event_seq").unwrap();
    assert_eq!(projected, current_oid);
    assert_eq!(sequence, 42);
}
