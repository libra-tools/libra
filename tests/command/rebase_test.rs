//! Tests rebase command applying commits onto new bases and handling conflicts.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

#![cfg(test)]
use std::{collections::VecDeque, fs, path::Path};

use libra::{
    command::rebase::{RebaseArgs, execute},
    common_utils::parse_commit_msg,
};
use serial_test::serial;
use tempfile::tempdir;

use super::*;

#[test]
fn test_rebase_cli_outside_repository_returns_fatal_128() {
    let temp = tempdir().unwrap();
    let output = run_libra_command(&["rebase", "main"], temp.path());
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal: not a libra repository"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn test_rebase_cli_missing_upstream_returns_usage_129() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["rebase"], repo.path());
    assert_eq!(output.status.code(), Some(129));
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(stderr.contains("Usage:"), "unexpected stderr: {stderr}");
}

#[test]
fn test_rebase_cli_invalid_upstream_returns_fatal_128() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["rebase", "nonexistent-upstream"], repo.path());
    assert_eq!(output.status.code(), Some(129));
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        stderr.contains("fatal:"),
        "expected fatal error for invalid upstream, got: {stderr}"
    );
    assert!(
        stderr.contains("nonexistent-upstream"),
        "stderr should include invalid ref name, got: {stderr}"
    );
}

#[test]
fn test_rebase_json_no_state_subcommands_return_repo_state_code() {
    let repo = create_committed_repo_via_cli();

    for flag in ["--continue", "--abort", "--skip"] {
        let output = run_libra_command(&["--json", "rebase", flag], repo.path());
        assert_eq!(output.status.code(), Some(128), "flag: {flag}");
        assert!(
            output.stdout.is_empty(),
            "expected empty stdout for {flag}, got: {}",
            String::from_utf8_lossy(&output.stdout)
        );

        let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
        assert_eq!(report.error_code, "LBR-REPO-003", "flag: {flag}");
        assert_eq!(report.category, "repo", "flag: {flag}");
        assert_eq!(report.exit_code, 128, "flag: {flag}");
        assert_eq!(report.message, "no rebase in progress", "flag: {flag}");
        assert!(
            report
                .hints
                .iter()
                .any(|hint| hint.contains(&format!("cannot {flag}"))),
            "expected hint for {flag}, got {:?}",
            report.hints
        );
    }
}

#[tokio::test]
#[serial]
async fn test_rebase_json_abort_outputs_restored_branch() {
    use libra::{command::rebase::RebaseState, internal::head::Head};

    let repo = create_committed_repo_via_cli();
    let repo_path = repo.path();
    let _guard = ChangeDirGuard::new(repo_path);
    let head = Head::current_commit()
        .await
        .expect("committed repo should have HEAD");

    RebaseState {
        head_name: "main".to_string(),
        onto: head,
        orig_head: head,
        todo: VecDeque::new(),
        todo_actions: VecDeque::new(),
        done: Vec::new(),
        stopped_sha: None,
        current_head: head,
        autosquash: false,
        empty_mode: libra::command::rebase::RebaseEmptyMode::Keep,
    }
    .save()
    .await
    .expect("failed to save rebase state");

    let output = run_libra_command(&["--json", "rebase", "--abort"], repo_path);
    assert_cli_success(&output, "json rebase abort");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "rebase");
    assert_eq!(json["data"]["action"], "abort");
    assert_eq!(json["data"]["branch"], "main");
    assert_eq!(json["data"]["commit"], head.to_string());
    assert_eq!(json["data"]["previous_commit"], head.to_string());
    assert_eq!(json["data"]["restored"], true);
    assert!(output.stderr.is_empty());
    assert!(
        !RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state")
    );
}

#[test]
fn test_rebase_machine_no_state_subcommands_return_repo_state_code() {
    let repo = create_committed_repo_via_cli();

    for flag in ["--continue", "--abort", "--skip"] {
        let output = run_libra_command(&["--machine", "rebase", flag], repo.path());
        assert_eq!(output.status.code(), Some(128), "flag: {flag}");
        assert!(
            output.stdout.is_empty(),
            "expected empty stdout for {flag}, got: {}",
            String::from_utf8_lossy(&output.stdout)
        );

        let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
        assert_eq!(report.error_code, "LBR-REPO-003", "flag: {flag}");
        assert_eq!(report.category, "repo", "flag: {flag}");
        assert_eq!(report.exit_code, 128, "flag: {flag}");
        assert_eq!(report.message, "no rebase in progress", "flag: {flag}");
        assert!(
            report
                .hints
                .iter()
                .any(|hint| hint.contains(&format!("cannot {flag}"))),
            "expected hint for {flag}, got {:?}",
            report.hints
        );
    }
}

#[test]
fn test_rebase_machine_abort_outputs_restored_branch() {
    use libra::{command::rebase::RebaseState, internal::head::Head};

    let repo = create_committed_repo_via_cli();
    let repo_path = repo.path();
    let _guard = ChangeDirGuard::new(repo_path);
    let runtime = tokio::runtime::Runtime::new().expect("failed to create runtime");
    let head = runtime
        .block_on(Head::current_commit())
        .expect("committed repo should have HEAD");

    runtime
        .block_on(async {
            RebaseState {
                head_name: "main".to_string(),
                onto: head,
                orig_head: head,
                todo: VecDeque::new(),
                todo_actions: VecDeque::new(),
                done: Vec::new(),
                stopped_sha: None,
                current_head: head,
                autosquash: false,
                empty_mode: libra::command::rebase::RebaseEmptyMode::Keep,
            }
            .save()
            .await
        })
        .expect("failed to save rebase state");

    let output = run_libra_command(&["--machine", "rebase", "--abort"], repo_path);
    assert_cli_success(&output, "machine rebase abort");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "rebase");
    assert_eq!(json["data"]["action"], "abort");
    assert_eq!(json["data"]["branch"], "main");
    assert_eq!(json["data"]["commit"], head.to_string());
    assert_eq!(json["data"]["previous_commit"], head.to_string());
    assert_eq!(json["data"]["restored"], true);
    assert!(output.stderr.is_empty());
    assert!(
        !runtime
            .block_on(RebaseState::is_in_progress())
            .expect("failed to query rebase state")
    );
}

#[test]
fn test_rebase_machine_continue_outputs_completed_result() {
    let repo = create_cli_rebase_conflict_repo();

    fs::write(repo.path().join("conflict.txt"), "merged\n").expect("failed to resolve conflict");
    let output = run_libra_command(&["add", "conflict.txt"], repo.path());
    assert_cli_success(&output, "failed to stage resolved conflict");

    let output = run_libra_command(&["--machine", "rebase", "--continue"], repo.path());
    assert_cli_success(&output, "machine rebase continue");
    assert!(output.stderr.is_empty());

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "rebase");
    assert_eq!(json["data"]["action"], "continue");
    assert_eq!(json["data"]["status"], "completed");
    assert_eq!(json["data"]["branch"], "feature");
    assert_eq!(json["data"]["remaining"], 0);
    assert_eq!(json["data"]["applied_commits"].as_array().unwrap().len(), 1);
    assert_eq!(
        json["data"]["applied_commits"][0]["subject"],
        "Feature modifies conflict.txt"
    );
    assert!(json["data"]["commit"].as_str().unwrap().len() >= 7);
    assert!(json["data"]["onto"].as_str().unwrap().len() >= 7);
    assert!(json["data"]["previous_commit"].as_str().unwrap().len() >= 7);
}

fn commit_file_via_cli(repo: &Path, path: &str, contents: &str, message: &str) {
    fs::write(repo.join(path), contents).expect("failed to write test file");

    let output = run_libra_command(&["add", path], repo);
    assert_cli_success(&output, "failed to stage test file");

    let output = run_libra_command(&["commit", "-m", message, "--no-verify"], repo);
    assert_cli_success(&output, "failed to commit test file");
}

fn create_cli_rebase_conflict_ready_repo() -> tempfile::TempDir {
    let repo = tempdir().expect("failed to create temp repo");
    let repo_path = repo.path();
    init_repo_via_cli(repo_path);
    configure_identity_via_cli(repo_path);

    commit_file_via_cli(repo_path, "conflict.txt", "base\n", "Base");

    let output = run_libra_command(&["switch", "-c", "feature"], repo_path);
    assert_cli_success(&output, "failed to create feature branch");
    commit_file_via_cli(
        repo_path,
        "conflict.txt",
        "feature\n",
        "Feature modifies conflict.txt",
    );

    let output = run_libra_command(&["switch", "main"], repo_path);
    assert_cli_success(&output, "failed to switch to main");
    commit_file_via_cli(
        repo_path,
        "conflict.txt",
        "main\n",
        "Main modifies conflict.txt",
    );

    let output = run_libra_command(&["switch", "feature"], repo_path);
    assert_cli_success(&output, "failed to switch to feature");

    repo
}

fn create_cli_rebase_conflict_repo() -> tempfile::TempDir {
    let repo = create_cli_rebase_conflict_ready_repo();
    let repo_path = repo.path();

    let output = run_libra_command(&["rebase", "main"], repo_path);
    assert_eq!(output.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("conflict.txt"),
        "expected conflict setup to stop rebase, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    repo
}

fn create_cli_rebase_success_repo() -> tempfile::TempDir {
    let repo = tempdir().expect("failed to create temp repo");
    let repo_path = repo.path();
    init_repo_via_cli(repo_path);
    configure_identity_via_cli(repo_path);

    commit_file_via_cli(repo_path, ".libraignore", "", "Track ignore file");
    commit_file_via_cli(repo_path, "base.txt", "base\n", "Base");

    let output = run_libra_command(&["switch", "-c", "feature"], repo_path);
    assert_cli_success(&output, "failed to create feature branch");
    commit_file_via_cli(repo_path, "feature.txt", "feature\n", "Feature adds file");

    let output = run_libra_command(&["switch", "main"], repo_path);
    assert_cli_success(&output, "failed to switch to main");
    commit_file_via_cli(repo_path, "main.txt", "main\n", "Main adds file");

    let output = run_libra_command(&["switch", "feature"], repo_path);
    assert_cli_success(&output, "failed to switch to feature");

    repo
}

#[cfg(unix)]
#[test]
fn test_rebase_preserves_executable_mode_in_rewritten_commit() {
    use std::os::unix::fs::PermissionsExt;

    use git_internal::internal::object::{
        commit::Commit,
        tree::{Tree, TreeItemMode},
    };
    use libra::{command::load_object, internal::head::Head, utils::object_ext::TreeExt};

    let repo = tempdir().expect("failed to create temp repo");
    let repo_path = repo.path();
    init_repo_via_cli(repo_path);
    configure_identity_via_cli(repo_path);

    commit_file_via_cli(repo_path, ".libraignore", "", "Track ignore file");
    commit_file_via_cli(repo_path, "base.txt", "base\n", "Base");

    let output = run_libra_command(&["switch", "-c", "feature"], repo_path);
    assert_cli_success(&output, "failed to create feature branch");

    let script_path = repo_path.join("script.sh");
    fs::write(&script_path, "#!/bin/sh\necho feature\n").expect("write script");
    let mut permissions = fs::metadata(&script_path)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script_path, permissions).expect("chmod script executable");

    let output = run_libra_command(&["add", "script.sh"], repo_path);
    assert_cli_success(&output, "failed to stage executable script");
    let output = run_libra_command(
        &["commit", "-m", "Feature adds executable", "--no-verify"],
        repo_path,
    );
    assert_cli_success(&output, "failed to commit executable script");

    let output = run_libra_command(&["switch", "main"], repo_path);
    assert_cli_success(&output, "failed to switch to main");
    commit_file_via_cli(repo_path, "main.txt", "main\n", "Main adds file");

    let output = run_libra_command(&["switch", "feature"], repo_path);
    assert_cli_success(&output, "failed to switch to feature");
    let output = run_libra_command(&["rebase", "main"], repo_path);
    assert_cli_success(&output, "rebase should rewrite feature branch");

    let _guard = ChangeDirGuard::new(repo_path);
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let head = runtime
        .block_on(Head::current_commit())
        .expect("rebased HEAD");
    let commit: Commit = load_object(&head).expect("load rebased commit");
    let tree: Tree = load_object(&commit.tree_id).expect("load rebased tree");
    let script_mode = tree
        .get_plain_items_with_mode()
        .into_iter()
        .find_map(|(path, _hash, mode)| (path == Path::new("script.sh")).then_some(mode))
        .expect("rebased tree should contain script.sh");
    assert_eq!(
        script_mode,
        TreeItemMode::BlobExecutable,
        "rebased commit must preserve script.sh executable bit"
    );

    let status = run_libra_command(&["status", "--short"], repo_path);
    assert_cli_success(&status, "status after rebase");
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "mode loss should not leave the worktree dirty: {}",
        String::from_utf8_lossy(&status.stdout)
    );
}

fn create_cli_rebase_fast_forward_repo() -> tempfile::TempDir {
    let repo = tempdir().expect("failed to create temp repo");
    let repo_path = repo.path();
    init_repo_via_cli(repo_path);
    configure_identity_via_cli(repo_path);

    commit_file_via_cli(repo_path, "base.txt", "base\n", "Base");

    let output = run_libra_command(&["switch", "-c", "feature"], repo_path);
    assert_cli_success(&output, "failed to create feature branch");

    let output = run_libra_command(&["switch", "main"], repo_path);
    assert_cli_success(&output, "failed to switch to main");
    commit_file_via_cli(repo_path, "main.txt", "main\n", "Main adds file");

    let output = run_libra_command(&["switch", "feature"], repo_path);
    assert_cli_success(&output, "failed to switch to feature");

    repo
}

fn create_cli_rebase_ahead_repo() -> tempfile::TempDir {
    let repo = tempdir().expect("failed to create temp repo");
    let repo_path = repo.path();
    init_repo_via_cli(repo_path);
    configure_identity_via_cli(repo_path);

    commit_file_via_cli(repo_path, "base.txt", "base\n", "Base");

    let output = run_libra_command(&["switch", "-c", "feature"], repo_path);
    assert_cli_success(&output, "failed to create feature branch");
    commit_file_via_cli(repo_path, "feature.txt", "feature\n", "Feature adds file");

    repo
}

#[test]
fn test_rebase_autosquash_folds_fixup_commit() {
    let repo = tempdir().expect("failed to create temp repo");
    let repo_path = repo.path();
    init_repo_via_cli(repo_path);
    configure_identity_via_cli(repo_path);

    commit_file_via_cli(repo_path, "base.txt", "base\n", "Base");

    let output = run_libra_command(&["switch", "-c", "feature"], repo_path);
    assert_cli_success(&output, "failed to create feature branch");
    commit_file_via_cli(repo_path, "feature.txt", "feature\n", "Feature adds file");
    commit_file_via_cli(
        repo_path,
        "feature.txt",
        "feature\nfixup\n",
        "fixup! Feature adds file",
    );

    let output = run_libra_command(&["switch", "main"], repo_path);
    assert_cli_success(&output, "failed to switch to main");
    commit_file_via_cli(repo_path, "main.txt", "main\n", "Main adds file");

    let output = run_libra_command(&["switch", "feature"], repo_path);
    assert_cli_success(&output, "failed to switch to feature");

    let output = run_libra_command(&["--json", "rebase", "--autosquash", "main"], repo_path);
    assert_cli_success(&output, "autosquash rebase should succeed");

    assert_eq!(
        fs::read_to_string(repo_path.join("feature.txt")).unwrap(),
        "feature\nfixup\n"
    );

    let output = run_libra_command(&["log", "--oneline", "-n", "4"], repo_path);
    assert_cli_success(&output, "log after autosquash rebase");
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(
        log.contains("Feature adds file"),
        "autosquashed history should keep target commit subject, got: {log}"
    );
    assert!(
        !log.contains("fixup! Feature adds file"),
        "autosquashed history should fold the fixup commit, got: {log}"
    );
}

#[test]
fn test_rebase_autosquash_keeps_unmatched_fixup_as_pick() {
    let repo = tempdir().expect("failed to create temp repo");
    let repo_path = repo.path();
    init_repo_via_cli(repo_path);
    configure_identity_via_cli(repo_path);

    commit_file_via_cli(repo_path, "base.txt", "base\n", "Base");

    let output = run_libra_command(&["switch", "-c", "feature"], repo_path);
    assert_cli_success(&output, "failed to create feature branch");
    commit_file_via_cli(repo_path, "feature.txt", "feature\n", "Feature adds file");
    commit_file_via_cli(
        repo_path,
        "feature.txt",
        "feature\nunmatched\n",
        "fixup! Missing target",
    );

    let output = run_libra_command(&["switch", "main"], repo_path);
    assert_cli_success(&output, "failed to switch to main");
    commit_file_via_cli(repo_path, "main.txt", "main\n", "Main adds file");

    let output = run_libra_command(&["switch", "feature"], repo_path);
    assert_cli_success(&output, "failed to switch to feature");

    let output = run_libra_command(&["--json", "rebase", "--autosquash", "main"], repo_path);
    assert_cli_success(&output, "autosquash rebase should succeed");

    assert_eq!(
        fs::read_to_string(repo_path.join("feature.txt")).unwrap(),
        "feature\nunmatched\n"
    );

    let output = run_libra_command(&["log", "--oneline", "-n", "4"], repo_path);
    assert_cli_success(&output, "log after autosquash rebase");
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(
        log.contains("Feature adds file"),
        "target commit should remain in history, got: {log}"
    );
    assert!(
        log.contains("fixup! Missing target"),
        "unmatched fixup should remain a standalone pick, got: {log}"
    );
}

#[test]
fn test_rebase_autosquash_keeps_unmatched_fixup_at_original_position() {
    let repo = tempdir().expect("failed to create temp repo");
    let repo_path = repo.path();
    init_repo_via_cli(repo_path);
    configure_identity_via_cli(repo_path);

    commit_file_via_cli(repo_path, "base.txt", "base\n", "Base");

    let output = run_libra_command(&["switch", "-c", "feature"], repo_path);
    assert_cli_success(&output, "failed to create feature branch");
    commit_file_via_cli(repo_path, "feature.txt", "feature\n", "Feature adds file");
    commit_file_via_cli(
        repo_path,
        "unmatched.txt",
        "unmatched\n",
        "fixup! Missing target",
    );
    commit_file_via_cli(repo_path, "later.txt", "later\n", "Feature follow-up");

    let output = run_libra_command(&["switch", "main"], repo_path);
    assert_cli_success(&output, "failed to switch to main");
    commit_file_via_cli(repo_path, "main.txt", "main\n", "Main adds file");

    let output = run_libra_command(&["switch", "feature"], repo_path);
    assert_cli_success(&output, "failed to switch to feature");

    let output = run_libra_command(&["--json", "rebase", "--autosquash", "main"], repo_path);
    assert_cli_success(&output, "autosquash rebase should succeed");

    let output = run_libra_command(&["log", "--reverse", "--oneline", "-n", "6"], repo_path);
    assert_cli_success(&output, "reverse log after autosquash rebase");
    let log = String::from_utf8_lossy(&output.stdout);
    let feature_idx = log
        .find("Feature adds file")
        .expect("feature commit should remain in history");
    let fixup_idx = log
        .find("fixup! Missing target")
        .expect("unmatched fixup should remain in history");
    let later_idx = log
        .find("Feature follow-up")
        .expect("later commit should remain in history");
    assert!(
        feature_idx < fixup_idx && fixup_idx < later_idx,
        "unmatched fixup should keep its original position, got: {log}"
    );
}

#[test]
fn test_rebase_autosquash_peels_stacked_fixup_markers() {
    let repo = tempdir().expect("failed to create temp repo");
    let repo_path = repo.path();
    init_repo_via_cli(repo_path);
    configure_identity_via_cli(repo_path);

    commit_file_via_cli(repo_path, "base.txt", "base\n", "Base");

    let output = run_libra_command(&["switch", "-c", "feature"], repo_path);
    assert_cli_success(&output, "failed to create feature branch");
    commit_file_via_cli(repo_path, "feature.txt", "feature\n", "Feature target");
    commit_file_via_cli(
        repo_path,
        "feature.txt",
        "feature\nnested fixup\n",
        "fixup! fixup! Feature target",
    );

    let output = run_libra_command(&["switch", "main"], repo_path);
    assert_cli_success(&output, "failed to switch to main");
    commit_file_via_cli(repo_path, "main.txt", "main\n", "Main adds file");

    let output = run_libra_command(&["switch", "feature"], repo_path);
    assert_cli_success(&output, "failed to switch to feature");

    let output = run_libra_command(&["--json", "rebase", "--autosquash", "main"], repo_path);
    assert_cli_success(&output, "autosquash rebase should succeed");

    assert_eq!(
        fs::read_to_string(repo_path.join("feature.txt")).unwrap(),
        "feature\nnested fixup\n"
    );

    let output = run_libra_command(&["log", "--oneline", "-n", "5"], repo_path);
    assert_cli_success(&output, "log after stacked autosquash rebase");
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(
        log.contains("Feature target"),
        "target commit should remain in history, got: {log}"
    );
    assert!(
        !log.contains("fixup! fixup! Feature target"),
        "stacked fixup should fold into the peeled target, got: {log}"
    );
}

#[test]
fn test_rebase_autosquash_prefers_exact_target_before_prefix_target() {
    let repo = tempdir().expect("failed to create temp repo");
    let repo_path = repo.path();
    init_repo_via_cli(repo_path);
    configure_identity_via_cli(repo_path);

    commit_file_via_cli(repo_path, "base.txt", "base\n", "Base");

    let output = run_libra_command(&["switch", "-c", "feature"], repo_path);
    assert_cli_success(&output, "failed to create feature branch");
    commit_file_via_cli(repo_path, "feature.txt", "extra\n", "Feature extra");
    commit_file_via_cli(repo_path, "feature.txt", "feature\n", "Feature");
    commit_file_via_cli(
        repo_path,
        "feature.txt",
        "feature\nfixup\n",
        "fixup! Feature",
    );

    let output = run_libra_command(&["switch", "main"], repo_path);
    assert_cli_success(&output, "failed to switch to main");
    commit_file_via_cli(repo_path, "main.txt", "main\n", "Main adds file");

    let output = run_libra_command(&["switch", "feature"], repo_path);
    assert_cli_success(&output, "failed to switch to feature");

    let output = run_libra_command(&["--json", "rebase", "--autosquash", "main"], repo_path);
    assert_cli_success(&output, "autosquash rebase should succeed");

    assert_eq!(
        fs::read_to_string(repo_path.join("feature.txt")).unwrap(),
        "feature\nfixup\n"
    );

    let output = run_libra_command(&["log", "--oneline", "-n", "5"], repo_path);
    assert_cli_success(&output, "log after autosquash rebase");
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(
        log.contains("Feature extra"),
        "prefix commit should remain in history, got: {log}"
    );
    assert!(
        log.contains("Feature"),
        "exact target commit should remain in history, got: {log}"
    );
    assert!(
        !log.contains("fixup! Feature"),
        "fixup should fold into exact target, got: {log}"
    );
}

#[test]
fn test_rebase_autosquash_does_not_fold_into_later_target() {
    let repo = tempdir().expect("failed to create temp repo");
    let repo_path = repo.path();
    init_repo_via_cli(repo_path);
    configure_identity_via_cli(repo_path);

    commit_file_via_cli(repo_path, "base.txt", "base\n", "Base");

    let output = run_libra_command(&["switch", "-c", "feature"], repo_path);
    assert_cli_success(&output, "failed to create feature branch");
    commit_file_via_cli(repo_path, "early.txt", "early\n", "fixup! Feature");
    commit_file_via_cli(repo_path, "feature.txt", "feature\n", "Feature");

    let output = run_libra_command(&["switch", "main"], repo_path);
    assert_cli_success(&output, "failed to switch to main");
    commit_file_via_cli(repo_path, "main.txt", "main\n", "Main adds file");

    let output = run_libra_command(&["switch", "feature"], repo_path);
    assert_cli_success(&output, "failed to switch to feature");

    let output = run_libra_command(&["--json", "rebase", "--autosquash", "main"], repo_path);
    assert_cli_success(&output, "autosquash rebase should succeed");

    assert_eq!(
        fs::read_to_string(repo_path.join("early.txt")).unwrap(),
        "early\n"
    );
    assert_eq!(
        fs::read_to_string(repo_path.join("feature.txt")).unwrap(),
        "feature\n"
    );

    let output = run_libra_command(&["log", "--oneline", "-n", "5"], repo_path);
    assert_cli_success(&output, "log after autosquash rebase");
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(
        log.contains("fixup! Feature"),
        "fixup before its target should remain a standalone pick, got: {log}"
    );
    assert!(
        log.contains("Feature"),
        "later target commit should remain in history, got: {log}"
    );
}

#[test]
fn test_rebase_autosquash_skip_target_keeps_dependent_fixup_as_pick() {
    let repo = tempdir().expect("failed to create temp repo");
    let repo_path = repo.path();
    init_repo_via_cli(repo_path);
    configure_identity_via_cli(repo_path);

    commit_file_via_cli(repo_path, "conflict.txt", "base\n", "Base");

    let output = run_libra_command(&["switch", "-c", "feature"], repo_path);
    assert_cli_success(&output, "failed to create feature branch");
    commit_file_via_cli(repo_path, "conflict.txt", "feature\n", "Feature target");
    commit_file_via_cli(repo_path, "fixup.txt", "fixup\n", "fixup! Feature target");

    let output = run_libra_command(&["switch", "main"], repo_path);
    assert_cli_success(&output, "failed to switch to main");
    commit_file_via_cli(repo_path, "conflict.txt", "main\n", "Main target");

    let output = run_libra_command(&["switch", "feature"], repo_path);
    assert_cli_success(&output, "failed to switch to feature");

    let output = run_libra_command(&["--json", "rebase", "--autosquash", "main"], repo_path);
    assert_eq!(output.status.code(), Some(128));
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-001");
    assert!(
        report.message.contains("Feature target"),
        "rebase should stop on the autosquash target, got: {}",
        report.message
    );

    let output = run_libra_command(&["--json", "rebase", "--skip"], repo_path);
    assert_cli_success(&output, "autosquash rebase skip should complete");

    assert_eq!(
        fs::read_to_string(repo_path.join("conflict.txt")).unwrap(),
        "main\n"
    );
    assert_eq!(
        fs::read_to_string(repo_path.join("fixup.txt")).unwrap(),
        "fixup\n"
    );

    let output = run_libra_command(&["log", "--oneline", "-n", "5"], repo_path);
    assert_cli_success(&output, "log after autosquash skip");
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(
        log.contains("fixup! Feature target"),
        "dependent fixup should become a standalone pick after its target is skipped, got: {log}"
    );
}

#[test]
fn test_rebase_autosquash_skip_fixup_preserves_later_fixup_action() {
    let repo = tempdir().expect("failed to create temp repo");
    let repo_path = repo.path();
    init_repo_via_cli(repo_path);
    configure_identity_via_cli(repo_path);

    commit_file_via_cli(repo_path, "conflict.txt", "base\n", "Base");

    let output = run_libra_command(&["switch", "-c", "feature"], repo_path);
    assert_cli_success(&output, "failed to create feature branch");
    commit_file_via_cli(repo_path, "feature.txt", "target\n", "Feature target");
    commit_file_via_cli(
        repo_path,
        "conflict.txt",
        "first fixup\n",
        "fixup! Feature target",
    );
    commit_file_via_cli(repo_path, "second.txt", "second\n", "fixup! Feature target");

    let output = run_libra_command(&["switch", "main"], repo_path);
    assert_cli_success(&output, "failed to switch to main");
    commit_file_via_cli(repo_path, "conflict.txt", "main\n", "Main target");

    let output = run_libra_command(&["switch", "feature"], repo_path);
    assert_cli_success(&output, "failed to switch to feature");

    let output = run_libra_command(&["--json", "rebase", "--autosquash", "main"], repo_path);
    assert_eq!(output.status.code(), Some(128));
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-001");
    assert!(
        report.message.contains("fixup! Feature target"),
        "rebase should stop on the first fixup, got: {}",
        report.message
    );

    let output = run_libra_command(&["--json", "rebase", "--skip"], repo_path);
    assert_cli_success(&output, "autosquash rebase skip should complete");

    assert_eq!(
        fs::read_to_string(repo_path.join("conflict.txt")).unwrap(),
        "main\n"
    );
    assert_eq!(
        fs::read_to_string(repo_path.join("feature.txt")).unwrap(),
        "target\n"
    );
    assert_eq!(
        fs::read_to_string(repo_path.join("second.txt")).unwrap(),
        "second\n"
    );

    let output = run_libra_command(&["log", "--oneline", "-n", "5"], repo_path);
    assert_cli_success(&output, "log after skipping conflicted fixup");
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(
        !log.contains("fixup! Feature target"),
        "later fixup should still fold after an earlier fixup is skipped, got: {log}"
    );
}

#[test]
fn test_rebase_autosquash_amend_replaces_target_message() {
    use git_internal::internal::object::commit::Commit;
    use libra::{command::load_object, internal::head::Head};

    let repo = tempdir().expect("failed to create temp repo");
    let repo_path = repo.path();
    init_repo_via_cli(repo_path);
    configure_identity_via_cli(repo_path);

    commit_file_via_cli(repo_path, "base.txt", "base\n", "Base");

    let output = run_libra_command(&["switch", "-c", "feature"], repo_path);
    assert_cli_success(&output, "failed to create feature branch");
    commit_file_via_cli(
        repo_path,
        "feature.txt",
        "feature\n",
        "Feature adds file\n\nOriginal body",
    );
    commit_file_via_cli(
        repo_path,
        "feature.txt",
        "feature\namended\n",
        "amend! Feature adds file\n\nReplacement subject\n\nReplacement body",
    );

    let output = run_libra_command(&["switch", "main"], repo_path);
    assert_cli_success(&output, "failed to switch to main");
    commit_file_via_cli(repo_path, "main.txt", "main\n", "Main adds file");

    let output = run_libra_command(&["switch", "feature"], repo_path);
    assert_cli_success(&output, "failed to switch to feature");

    let output = run_libra_command(&["--json", "rebase", "--autosquash", "main"], repo_path);
    assert_cli_success(&output, "autosquash rebase should succeed");

    assert_eq!(
        fs::read_to_string(repo_path.join("feature.txt")).unwrap(),
        "feature\namended\n"
    );

    let _guard = ChangeDirGuard::new(repo_path);
    let runtime = tokio::runtime::Runtime::new().expect("failed to create runtime");
    let head = runtime
        .block_on(Head::current_commit())
        .expect("rebased branch should have HEAD");
    let commit: Commit = load_object(&head).expect("failed to load rebased HEAD commit");
    let (clean_message, _trailers) = parse_commit_msg(&commit.message);
    let subject = clean_message.lines().next().unwrap_or("");
    assert_eq!(subject, "Replacement subject");
    assert!(
        clean_message.contains("Replacement body"),
        "amend commit message should replace target message: {:?}",
        clean_message
    );
    assert!(
        !clean_message.contains("amend! Feature adds file"),
        "amend marker should be stripped from replacement message: {:?}",
        clean_message
    );
    assert!(
        !clean_message.contains("Original body"),
        "amend should not keep target commit message: {:?}",
        clean_message
    );
}

#[test]
fn test_rebase_reapply_cherry_picks_flag_is_accepted() {
    let repo = create_cli_rebase_success_repo();

    let output = run_libra_command(
        &["--json", "rebase", "--reapply-cherry-picks", "main"],
        repo.path(),
    );
    assert_cli_success(&output, "rebase --reapply-cherry-picks");

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["status"], "completed");
    assert_eq!(json["data"]["replay_count"], 1);
}

#[test]
fn test_rebase_no_autostash_flag_is_accepted_noop() {
    let repo = create_cli_rebase_success_repo();

    // `--no-autostash` is accepted and a no-op: Libra's rebase never autostashes
    // (it requires a clean tree), so the rebase proceeds normally.
    let output = run_libra_command(&["--json", "rebase", "--no-autostash", "main"], repo.path());
    assert_cli_success(&output, "rebase --no-autostash");

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["status"], "completed");
    assert_eq!(json["data"]["replay_count"], 1);
}

#[test]
fn test_rebase_no_rerere_autoupdate_flag_is_accepted_noop() {
    let repo = create_cli_rebase_success_repo();

    // `--no-rerere-autoupdate` is accepted and a no-op: Libra has no rerere, so
    // the rebase proceeds normally.
    let output = run_libra_command(
        &["--json", "rebase", "--no-rerere-autoupdate", "main"],
        repo.path(),
    );
    assert_cli_success(&output, "rebase --no-rerere-autoupdate");

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["status"], "completed");
    assert_eq!(json["data"]["replay_count"], 1);
}

#[test]
fn test_rebase_human_start_uses_structured_runner_output() {
    let repo = create_cli_rebase_success_repo();

    let output = run_libra_command(&["rebase", "main"], repo.path());
    assert_cli_success(&output, "human rebase start");
    assert!(output.stderr.is_empty());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Found common ancestor:"),
        "expected common ancestor progress, got: {stdout}"
    );
    assert!(
        stdout.contains("Rebasing 1 commits from `feature` onto `main`..."),
        "expected replay progress, got: {stdout}"
    );
    assert!(
        stdout.contains("Applied:") && stdout.contains("Feature adds file"),
        "expected applied commit summary, got: {stdout}"
    );
    assert!(
        stdout.contains("Successfully rebased branch 'feature' onto"),
        "expected final success message, got: {stdout}"
    );
}

#[test]
fn test_rebase_human_start_conflict_returns_structured_failure() {
    let repo = create_cli_rebase_conflict_ready_repo();

    let output = run_libra_command(&["rebase", "main"], repo.path());
    assert_eq!(output.status.code(), Some(128));
    assert!(
        output.stdout.is_empty(),
        "expected empty stdout on conflict, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        stderr.contains("fatal: rebase stopped while applying"),
        "unexpected stderr: {stderr}"
    );
    assert_eq!(report.error_code, "LBR-CONFLICT-001");
    assert_eq!(report.category, "conflict");
    assert_eq!(report.details["paths"][0], "conflict.txt");
    assert!(
        report
            .hints
            .iter()
            .any(|hint| hint.contains("conflict.txt")),
        "expected conflicted path hint, got {:?}",
        report.hints
    );

    let abort = run_libra_command(&["rebase", "--abort"], repo.path());
    assert_cli_success(&abort, "abort after human rebase conflict");
}

#[test]
fn test_rebase_json_start_outputs_completed_result() {
    let repo = create_cli_rebase_success_repo();

    let output = run_libra_command(&["--json", "rebase", "main"], repo.path());
    assert_cli_success(&output, "json rebase start");
    assert!(output.stderr.is_empty());

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "rebase");
    assert_eq!(json["data"]["action"], "start");
    assert_eq!(json["data"]["status"], "completed");
    assert_eq!(json["data"]["branch"], "feature");
    assert_eq!(json["data"]["upstream"], "main");
    assert_eq!(json["data"]["remaining"], 0);
    assert_eq!(json["data"]["replay_count"], 1);
    assert_eq!(json["data"]["applied_commits"].as_array().unwrap().len(), 1);
    assert_eq!(
        json["data"]["applied_commits"][0]["subject"],
        "Feature adds file"
    );
    assert!(json["data"]["commit"].as_str().unwrap().len() >= 7);
    assert!(json["data"]["onto"].as_str().unwrap().len() >= 7);
    assert!(json["data"]["common_ancestor"].as_str().unwrap().len() >= 7);
    assert!(json["data"]["previous_commit"].as_str().unwrap().len() >= 7);
}

#[test]
fn test_rebase_machine_start_outputs_single_json_line() {
    let repo = create_cli_rebase_success_repo();

    let output = run_libra_command(&["--machine", "rebase", "main"], repo.path());
    assert_cli_success(&output, "machine rebase start");
    assert!(output.stderr.is_empty());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().count(),
        1,
        "machine rebase output should be exactly one line: {stdout}"
    );
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["action"], "start");
    assert_eq!(json["data"]["status"], "completed");
}

#[test]
fn test_rebase_json_start_outputs_fast_forward_result() {
    let repo = create_cli_rebase_fast_forward_repo();

    let output = run_libra_command(&["--json", "rebase", "main"], repo.path());
    assert_cli_success(&output, "json rebase fast-forward start");
    assert!(output.stderr.is_empty());

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["action"], "start");
    assert_eq!(json["data"]["status"], "fast-forwarded");
    assert_eq!(json["data"]["branch"], "feature");
    assert_eq!(json["data"]["remaining"], 0);
    assert_eq!(json["data"]["commit"], json["data"]["onto"]);
    assert!(json["data"]["applied_commits"].is_null());
}

#[test]
fn test_rebase_json_start_outputs_already_up_to_date_result() {
    let repo = create_cli_rebase_ahead_repo();

    let output = run_libra_command(&["--json", "rebase", "main"], repo.path());
    assert_cli_success(&output, "json rebase already-up-to-date start");
    assert!(output.stderr.is_empty());

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["action"], "start");
    assert_eq!(json["data"]["status"], "already-up-to-date");
    assert_eq!(json["data"]["branch"], "feature");
    assert_eq!(json["data"]["remaining"], 0);
    assert_eq!(json["data"]["commit"], json["data"]["previous_commit"]);
    assert!(json["data"]["applied_commits"].is_null());
}

#[test]
fn test_rebase_json_start_conflict_returns_structured_error() {
    let repo = create_cli_rebase_conflict_ready_repo();

    let output = run_libra_command(&["--json", "rebase", "main"], repo.path());
    assert_eq!(output.status.code(), Some(128));
    assert!(
        output.stdout.is_empty(),
        "expected empty stdout, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-001");
    assert_eq!(report.category, "conflict");
    assert!(
        report.message.contains("rebase stopped while applying"),
        "unexpected message: {}",
        report.message
    );
    assert_eq!(report.details["paths"][0], "conflict.txt");

    let abort = run_libra_command(&["--json", "rebase", "--abort"], repo.path());
    assert_cli_success(&abort, "abort after json rebase conflict");
}

#[test]
fn test_rebase_json_continue_with_unresolved_conflicts_returns_structured_error() {
    let repo = create_cli_rebase_conflict_repo();

    let output = run_libra_command(&["--json", "rebase", "--continue"], repo.path());
    assert_eq!(output.status.code(), Some(128));
    assert!(
        output.stdout.is_empty(),
        "expected empty stdout, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-001");
    assert_eq!(report.category, "conflict");
    assert_eq!(
        report.message,
        "you must resolve all conflicts before continuing"
    );
    assert!(
        report
            .hints
            .iter()
            .any(|hint| hint.contains("libra add <file>")),
        "expected conflict-resolution hint, got {:?}",
        report.hints
    );
}

#[test]
fn test_rebase_json_continue_outputs_completed_result() {
    let repo = create_cli_rebase_conflict_repo();
    fs::write(repo.path().join("conflict.txt"), "merged\n").expect("failed to resolve conflict");
    let output = run_libra_command(&["add", "conflict.txt"], repo.path());
    assert_cli_success(&output, "failed to stage resolved conflict");

    let output = run_libra_command(&["--json", "rebase", "--continue"], repo.path());
    assert_cli_success(&output, "json rebase continue");
    assert!(output.stderr.is_empty());

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "rebase");
    assert_eq!(json["data"]["action"], "continue");
    assert_eq!(json["data"]["status"], "completed");
    assert_eq!(json["data"]["branch"], "feature");
    assert_eq!(json["data"]["remaining"], 0);
    assert_eq!(json["data"]["applied_commits"].as_array().unwrap().len(), 1);
    assert_eq!(
        json["data"]["applied_commits"][0]["subject"],
        "Feature modifies conflict.txt"
    );
    assert!(json["data"]["commit"].as_str().unwrap().len() >= 7);
    assert!(json["data"]["onto"].as_str().unwrap().len() >= 7);
    assert!(json["data"]["previous_commit"].as_str().unwrap().len() >= 7);
}

#[test]
fn test_rebase_machine_skip_outputs_completed_result() {
    let repo = create_cli_rebase_conflict_repo();

    let output = run_libra_command(&["--machine", "rebase", "--skip"], repo.path());
    assert_cli_success(&output, "machine rebase skip");
    assert!(output.stderr.is_empty());

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "rebase");
    assert_eq!(json["data"]["action"], "skip");
    assert_eq!(json["data"]["status"], "completed");
    assert_eq!(json["data"]["branch"], "feature");
    assert_eq!(json["data"]["remaining"], 0);
    assert_eq!(
        json["data"]["skipped_subject"],
        "Feature modifies conflict.txt"
    );
    assert!(json["data"]["skipped_commit"].as_str().unwrap().len() >= 7);
    assert!(json["data"]["commit"].as_str().unwrap().len() >= 7);
    assert!(json["data"]["onto"].as_str().unwrap().len() >= 7);
}

fn commit_messages_from_head(start: &ObjectHash, max: usize) -> Vec<String> {
    let mut messages = Vec::new();
    let mut current = Some(*start);
    while let Some(hash) = current {
        let commit = load_object::<Commit>(&hash).unwrap();
        let (message, _) = parse_commit_msg(&commit.message);
        messages.push(message.trim().to_string());

        current = commit.parent_commit_ids.first().copied();
        if messages.len() >= max {
            break;
        }
    }
    messages
}

#[tokio::test]
#[serial]
async fn test_basic_rebase() {
    use libra::internal::head::Head;

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // 1. Create initial commits on master
    fs::write(temp_path.path().join("file.txt"), "content1").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".to_string()],
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
        message: Some("C1: Add file.txt on master".to_string()),
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

    fs::write(temp_path.path().join("file.txt"), "content1\ncontent2").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".to_string()],
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
        message: Some("C2: Modify file.txt on master".to_string()),
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

    // 2. Create and switch to feature branch
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".to_string()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    // 3. Create commits on feature branch
    fs::write(temp_path.path().join("feature_a.txt"), "featureA").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["feature_a.txt".to_string()],
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
        message: Some("F1: Add feature_a.txt on feature branch".to_string()),
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

    fs::write(temp_path.path().join("feature_b.txt"), "featureB").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["feature_b.txt".to_string()],
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
        message: Some("F2: Add feature_b.txt on feature branch".to_string()),
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

    // 4. Switch back to master and make it diverge
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("master_only.txt"), "master_change").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["master_only.txt".to_string()],
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
        message: Some("C3: Add master_only.txt on master".to_string()),
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

    // 5. Switch back to feature and perform rebase
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: Some("main".to_string()),
        continue_rebase: false,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    // 6. Verify the rebase result
    // Check that all files exist after rebase
    assert!(temp_path.path().join("file.txt").exists());
    assert!(temp_path.path().join("feature_a.txt").exists());
    assert!(temp_path.path().join("feature_b.txt").exists());
    assert!(temp_path.path().join("master_only.txt").exists());

    // Check file contents
    assert_eq!(
        fs::read_to_string(temp_path.path().join("file.txt")).unwrap(),
        "content1\ncontent2"
    );
    assert_eq!(
        fs::read_to_string(temp_path.path().join("feature_a.txt")).unwrap(),
        "featureA"
    );
    assert_eq!(
        fs::read_to_string(temp_path.path().join("feature_b.txt")).unwrap(),
        "featureB"
    );
    assert_eq!(
        fs::read_to_string(temp_path.path().join("master_only.txt")).unwrap(),
        "master_change"
    );

    let head_commit = Head::current_commit().await.expect("expected HEAD commit");
    let messages = commit_messages_from_head(&head_commit, 5);
    assert_eq!(
        messages,
        vec![
            "F2: Add feature_b.txt on feature branch",
            "F1: Add feature_a.txt on feature branch",
            "C3: Add master_only.txt on master",
            "C2: Modify file.txt on master",
            "C1: Add file.txt on master"
        ]
    );
}

#[tokio::test]
#[serial]
async fn test_rebase_preserves_untracked_files() {
    use libra::internal::head::Head;

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Base commit on master
    fs::write(temp_path.path().join("file.txt"), "base").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".to_string()],
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
        message: Some("Base commit".to_string()),
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

    // Create feature branch and add a commit
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".to_string()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("feature.txt"), "feature").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["feature.txt".to_string()],
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
        message: Some("Feature commit".to_string()),
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

    // Advance master to force a real rebase
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("file.txt"), "base\nmaster").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".to_string()],
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
        message: Some("Advance master".to_string()),
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

    // Switch back to feature and create an untracked file
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    let feature_head_before = Head::current_commit().await.unwrap();
    fs::write(temp_path.path().join("notes.txt"), "keep me").unwrap();

    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: Some("main".to_string()),
        continue_rebase: false,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    let feature_head_after = Head::current_commit().await.unwrap();
    assert_ne!(
        feature_head_after, feature_head_before,
        "Rebase should move feature HEAD"
    );

    let file_contents = fs::read_to_string(temp_path.path().join("file.txt")).unwrap();
    assert_eq!(file_contents, "base\nmaster");

    let notes_contents = fs::read_to_string(temp_path.path().join("notes.txt")).unwrap();
    assert_eq!(notes_contents, "keep me");
}

#[tokio::test]
#[serial]
async fn test_rebase_already_up_to_date() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create commits on master
    fs::write(temp_path.path().join("file1.txt"), "content1").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file1.txt".to_string()],
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
        message: Some("First commit".to_string()),
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

    fs::write(temp_path.path().join("file2.txt"), "content2").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file2.txt".to_string()],
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
        message: Some("Second commit".to_string()),
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

    // Create feature branch from current master (no divergence)
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".to_string()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    // Try to rebase feature onto master (should be up to date)
    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: Some("main".to_string()),
        continue_rebase: false,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    // Should complete without errors (already up to date)
}

#[tokio::test]
#[serial]
async fn test_rebase_abort_when_no_rebase_in_progress() {
    use libra::{command::rebase::RebaseState, internal::head::Head};

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create initial commit on master
    fs::write(temp_path.path().join("file.txt"), "base content").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".to_string()],
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

    // Create feature branch and make a commit
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".to_string()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("feature.txt"), "feature content").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["feature.txt".to_string()],
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
        message: Some("Feature commit".to_string()),
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

    // Switch back to master and make a conflicting commit
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("master.txt"), "master content").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["master.txt".to_string()],
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
        message: Some("Master commit".to_string()),
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

    // Switch back to feature
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    // Start rebase
    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: Some("main".to_string()),
        continue_rebase: false,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    let head_before_abort = Head::current_commit().await.expect("expected HEAD commit");
    let messages_before_abort = commit_messages_from_head(&head_before_abort, 3);
    assert_eq!(
        messages_before_abort,
        vec!["Feature commit", "Master commit", "Initial commit"]
    );

    // Rebase should complete (no conflict in this case)
    // But let's test abort when no rebase is in progress
    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: None,
        continue_rebase: false,
        abort: true,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    let head_after_abort = Head::current_commit().await.expect("expected HEAD commit");
    assert_eq!(
        head_after_abort, head_before_abort,
        "Abort without rebase should not move HEAD"
    );
    let messages_after_abort = commit_messages_from_head(&head_after_abort, 3);
    assert_eq!(messages_after_abort, messages_before_abort);

    // Should handle gracefully (no rebase in progress)
    assert!(
        !RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state")
    );
}

#[tokio::test]
#[serial]
async fn test_rebase_abort_restores_branch_after_finalize_failure() {
    use std::collections::VecDeque;

    use libra::{
        command::rebase::RebaseState,
        internal::{branch::Branch, head::Head},
    };

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create base commit on master
    fs::write(temp_path.path().join("base.txt"), "base").unwrap();
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
        message: Some("Base commit".to_string()),
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

    // Create feature branch and commit
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".to_string()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;
    fs::write(temp_path.path().join("feature.txt"), "feature").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["feature.txt".to_string()],
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
        message: Some("Feature commit".to_string()),
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
    let orig_head = Head::current_commit().await.expect("expected feature HEAD");

    // Advance master to force a rebase
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;
    fs::write(temp_path.path().join("master.txt"), "main").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["master.txt".to_string()],
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
        message: Some("Master commit".to_string()),
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
    let master_head = Head::current_commit().await.expect("expected master HEAD");

    // Rebase feature onto master
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;
    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: Some("main".to_string()),
        continue_rebase: false,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    let rebased_head = Head::current_commit().await.expect("expected rebased HEAD");
    assert_ne!(
        rebased_head, orig_head,
        "rebase should rewrite the feature tip"
    );
    // Migrated from lossy `Branch::find_branch` per docs/development/commands/branch.md —
    // storage errors no longer collapse into "feature branch should exist".
    let branch_after_rebase = Branch::find_branch_result("feature", None)
        .await
        .expect("failed to query feature branch")
        .expect("feature branch should exist");
    assert_eq!(branch_after_rebase.commit, rebased_head);

    // Simulate a failed finalize: branch already moved, but rebase state still exists.
    let state = RebaseState {
        head_name: "feature".to_string(),
        onto: master_head,
        orig_head,
        todo: VecDeque::new(),
        todo_actions: VecDeque::new(),
        done: Vec::new(),
        stopped_sha: None,
        current_head: rebased_head,
        autosquash: false,
        empty_mode: libra::command::rebase::RebaseEmptyMode::Keep,
    };
    state
        .save()
        .await
        .expect("failed to save simulated rebase state");
    assert!(
        RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state")
    );

    // Abort should restore the original branch ref.
    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: None,
        continue_rebase: false,
        abort: true,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    // Migrated from lossy `Branch::find_branch` per docs/development/commands/branch.md.
    let branch_after_abort = Branch::find_branch_result("feature", None)
        .await
        .expect("failed to query feature branch after abort")
        .expect("feature branch should exist");
    assert_eq!(
        branch_after_abort.commit, orig_head,
        "abort should restore branch ref to orig_head"
    );
    let head_after_abort = Head::current_commit().await.expect("expected HEAD commit");
    assert_eq!(
        head_after_abort, orig_head,
        "abort should restore HEAD to orig_head"
    );
    assert!(
        !RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state")
    );
}

#[tokio::test]
#[serial]
async fn test_rebase_continue_no_rebase() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create initial commit
    fs::write(temp_path.path().join("file.txt"), "content").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".to_string()],
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

    // Try to continue when no rebase is in progress
    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: None,
        continue_rebase: true,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    // Should handle gracefully (outputs error message)
}

#[tokio::test]
#[serial]
async fn test_rebase_skip_no_rebase() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create initial commit
    fs::write(temp_path.path().join("file.txt"), "content").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".to_string()],
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

    // Try to skip when no rebase is in progress
    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: None,
        continue_rebase: false,
        abort: false,
        skip: true,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    // Should handle gracefully (outputs error message)
}

#[tokio::test]
#[serial]
async fn test_rebase_with_conflict_and_abort() {
    use libra::{command::rebase::RebaseState, internal::head::Head};

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // 1. Create initial commit on master with a file
    fs::write(temp_path.path().join("conflict.txt"), "base content").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string()],
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
        message: Some("Base commit".to_string()),
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

    // 2. Create feature branch and modify the file
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".to_string()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(
        temp_path.path().join("conflict.txt"),
        "feature modification",
    )
    .unwrap();
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string()],
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
        message: Some("Feature modifies conflict.txt".to_string()),
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

    // 3. Switch to master and make a conflicting modification
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("conflict.txt"), "master modification").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string()],
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
        message: Some("Master modifies conflict.txt".to_string()),
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

    // 4. Switch back to feature and attempt rebase (should conflict)
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: Some("main".to_string()),
        continue_rebase: false,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    // 5. Rebase should be in progress (conflict should have stopped it)
    assert!(
        RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state"),
        "Expected conflict to stop rebase"
    );

    // Verify conflict markers in file
    let content = fs::read_to_string(temp_path.path().join("conflict.txt")).unwrap();
    assert!(
        content.contains("<<<<<<<") || content.contains("=======") || content.contains(">>>>>>>"),
        "Expected conflict markers in file"
    );

    // 6. Abort the rebase
    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: None,
        continue_rebase: false,
        abort: true,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    // 7. Verify rebase is no longer in progress
    assert!(
        !RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state"),
        "Rebase should not be in progress after abort"
    );

    // 8. Verify we're back on feature branch
    match Head::current().await {
        Head::Branch(name) => assert_eq!(name, "feature", "Should be back on feature branch"),
        _ => panic!("Should be on a branch after abort"),
    }

    // 9. Verify file content is restored to feature branch version
    let restored_content = fs::read_to_string(temp_path.path().join("conflict.txt")).unwrap();
    assert_eq!(
        restored_content, "feature modification",
        "File should be restored to feature branch content after abort"
    );

    let head_commit = Head::current_commit().await.expect("expected HEAD commit");
    let messages = commit_messages_from_head(&head_commit, 2);
    assert_eq!(
        messages,
        vec!["Feature modifies conflict.txt", "Base commit"]
    );
}

#[tokio::test]
#[serial]
async fn test_rebase_binary_conflict_writes_markers() {
    use libra::command::rebase::RebaseState;

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    let file_path = temp_path.path().join("binary.bin");
    let base_bytes = vec![0x00, 0xFF, 0x01, 0x02];
    let feature_bytes = vec![0x10, 0xFF, 0x20, 0x21];
    let master_bytes = vec![0x30, 0xFF, 0x40, 0x41];

    // 1. Base commit on master with binary content
    fs::write(&file_path, &base_bytes).unwrap();
    add::execute(AddArgs {
        pathspec: vec!["binary.bin".to_string()],
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
        message: Some("Base binary".to_string()),
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

    // 2. Feature branch modifies binary content
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".to_string()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;
    fs::write(&file_path, &feature_bytes).unwrap();
    add::execute(AddArgs {
        pathspec: vec!["binary.bin".to_string()],
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
        message: Some("Feature binary".to_string()),
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

    // 3. Master modifies binary content differently
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;
    fs::write(&file_path, &master_bytes).unwrap();
    add::execute(AddArgs {
        pathspec: vec!["binary.bin".to_string()],
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
        message: Some("Master binary".to_string()),
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

    // 4. Rebase feature onto master (should conflict)
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;
    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: Some("main".to_string()),
        continue_rebase: false,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    assert!(
        RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state"),
        "Expected conflict to stop rebase"
    );

    // Binary conflict should still write conflict markers for resolution.
    let current = fs::read_to_string(&file_path).unwrap();
    assert!(
        current.contains("<<<<<<< HEAD"),
        "Expected conflict markers in binary conflict file"
    );
    assert!(
        current.contains("[binary content, 4 bytes]"),
        "Expected binary placeholder content"
    );

    // Cleanup: abort rebase
    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: None,
        continue_rebase: false,
        abort: true,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;
    assert!(
        !RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state"),
        "Rebase should not be in progress after abort"
    );
}

#[tokio::test]
#[serial]
async fn test_rebase_with_conflict_and_skip() {
    use libra::command::rebase::RebaseState;

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // 1. Create initial commit on master
    fs::write(temp_path.path().join("conflict.txt"), "base content").unwrap();
    fs::write(temp_path.path().join("other.txt"), "other base").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string(), "other.txt".to_string()],
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
        message: Some("Base commit".to_string()),
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

    // 2. Create feature branch with two commits
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".to_string()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    // First feature commit - will conflict
    fs::write(
        temp_path.path().join("conflict.txt"),
        "feature modification",
    )
    .unwrap();
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string()],
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
        message: Some("Feature commit 1 - conflicts".to_string()),
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

    // Second feature commit - no conflict
    fs::write(
        temp_path.path().join("feature_only.txt"),
        "feature only content",
    )
    .unwrap();
    add::execute(AddArgs {
        pathspec: vec!["feature_only.txt".to_string()],
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
        message: Some("Feature commit 2 - no conflict".to_string()),
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

    // 3. Switch to master and make a conflicting change
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("conflict.txt"), "master modification").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string()],
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
        message: Some("Master modifies conflict.txt".to_string()),
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

    // 4. Switch back to feature and attempt rebase
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: Some("main".to_string()),
        continue_rebase: false,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    // 5. Rebase should stop due to conflict; skip the conflicting commit
    assert!(
        RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state"),
        "Expected conflict to stop rebase"
    );

    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: None,
        continue_rebase: false,
        abort: false,
        skip: true,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    // After skip, rebase should complete and apply the non-conflicting commit
    assert!(
        !RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state"),
        "Rebase should complete after skip"
    );
    assert!(
        temp_path.path().join("feature_only.txt").exists(),
        "feature_only.txt should exist after skip and continue"
    );
}

#[tokio::test]
#[serial]
async fn test_rebase_with_conflict_and_continue() {
    use libra::{command::rebase::RebaseState, internal::head::Head};

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // 1. Create initial commit on master
    fs::write(temp_path.path().join("conflict.txt"), "base content").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string()],
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
        message: Some("Base commit".to_string()),
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

    // 2. Create feature branch and modify the file
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".to_string()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(
        temp_path.path().join("conflict.txt"),
        "feature modification",
    )
    .unwrap();
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string()],
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
        message: Some("Feature modifies conflict.txt".to_string()),
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

    // 3. Switch to master and make a conflicting modification
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("conflict.txt"), "master modification").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string()],
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
        message: Some("Master modifies conflict.txt".to_string()),
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

    // 4. Switch back to feature and attempt rebase
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: Some("main".to_string()),
        continue_rebase: false,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    // 5. Rebase should stop due to conflict, resolve it and continue
    assert!(
        RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state"),
        "Expected conflict to stop rebase"
    );

    // Resolve the conflict by writing merged content
    fs::write(
        temp_path.path().join("conflict.txt"),
        "merged content from both branches",
    )
    .unwrap();

    // Stage the resolved file
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string()],
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

    // Continue the rebase
    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: None,
        continue_rebase: true,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    // Verify rebase completed
    assert!(
        !RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state"),
        "Rebase should be complete after continue"
    );

    // Verify the merged content
    let final_content = fs::read_to_string(temp_path.path().join("conflict.txt")).unwrap();
    assert_eq!(
        final_content, "merged content from both branches",
        "File should contain merged content"
    );

    // Verify we're on feature branch
    match Head::current().await {
        Head::Branch(name) => assert_eq!(name, "feature", "Should be on feature branch"),
        _ => panic!("Should be on a branch after rebase"),
    }
}

#[tokio::test]
#[serial]
async fn test_rebase_multiple_commits_partial_conflict() {
    use libra::command::rebase::RebaseState;

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // 1. Create initial commit on master
    fs::write(temp_path.path().join("file1.txt"), "base1").unwrap();
    fs::write(temp_path.path().join("file2.txt"), "base2").unwrap();
    fs::write(temp_path.path().join("file3.txt"), "base3").unwrap();
    add::execute(AddArgs {
        pathspec: vec![
            "file1.txt".to_string(),
            "file2.txt".to_string(),
            "file3.txt".to_string(),
        ],
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
        message: Some("Base commit with 3 files".to_string()),
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

    // 2. Create feature branch with 3 commits
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".to_string()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    // Commit 1: modify file1 (will conflict)
    fs::write(temp_path.path().join("file1.txt"), "feature1").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file1.txt".to_string()],
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
        message: Some("F1: modify file1".to_string()),
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

    // Commit 2: add new file (no conflict)
    fs::write(
        temp_path.path().join("new_feature.txt"),
        "new feature content",
    )
    .unwrap();
    add::execute(AddArgs {
        pathspec: vec!["new_feature.txt".to_string()],
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
        message: Some("F2: add new_feature.txt".to_string()),
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

    // Commit 3: modify file3 (no conflict)
    fs::write(temp_path.path().join("file3.txt"), "feature3").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file3.txt".to_string()],
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
        message: Some("F3: modify file3".to_string()),
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

    // 3. Switch to master and make conflicting change to file1
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("file1.txt"), "master1").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file1.txt".to_string()],
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
        message: Some("M1: modify file1".to_string()),
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

    // 4. Switch back to feature and attempt rebase
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: Some("main".to_string()),
        continue_rebase: false,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    // 5. Handle conflicts - skip the first conflicting commit
    assert!(
        RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state"),
        "Expected conflict to stop rebase"
    );

    // Skip the conflicting commit (F1)
    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: None,
        continue_rebase: false,
        abort: false,
        skip: true,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    // The remaining commits (F2 and F3) should apply without conflict
    assert!(
        !RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state"),
        "Rebase should complete after skip"
    );

    // Verify file1 has master's content (since we skipped feature's change)
    let file1_content = fs::read_to_string(temp_path.path().join("file1.txt")).unwrap();
    assert_eq!(
        file1_content, "master1",
        "file1 should have master's content after skip"
    );

    // Verify new_feature.txt exists (from F2)
    assert!(
        temp_path.path().join("new_feature.txt").exists(),
        "new_feature.txt should exist from commit F2"
    );

    // Verify file3 has feature's content (from F3)
    let file3_content = fs::read_to_string(temp_path.path().join("file3.txt")).unwrap();
    assert_eq!(
        file3_content, "feature3",
        "file3 should have feature's content from F3"
    );
}

#[tokio::test]
#[serial]
async fn test_rebase_state_persistence() {
    use libra::command::rebase::RebaseState;

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // 1. Create initial commit
    fs::write(temp_path.path().join("file.txt"), "base").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".to_string()],
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
        message: Some("Base".to_string()),
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

    // 2. Create feature branch with conflicting change
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".to_string()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("file.txt"), "feature").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".to_string()],
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
        message: Some("Feature".to_string()),
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

    // 3. Create conflicting change on master
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("file.txt"), "main").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".to_string()],
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
        message: Some("Master".to_string()),
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

    // 4. Start rebase
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: Some("main".to_string()),
        continue_rebase: false,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    // 5. Check state persistence
    assert!(
        RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state"),
        "Expected conflict to stop rebase"
    );

    // Verify legacy state files are not created
    let rebase_dir = temp_path.path().join(".libra/rebase-merge");
    assert!(
        !rebase_dir.exists(),
        "legacy rebase-merge directory should not be created"
    );

    // Load and verify state
    let state = RebaseState::load()
        .await
        .expect("Should be able to load state");
    assert_eq!(state.head_name, "feature", "head_name should be 'feature'");
    assert!(
        state.stopped_sha.is_some(),
        "stopped_sha should be set during conflict"
    );

    // Clean up - abort the rebase
    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: None,
        continue_rebase: false,
        abort: true,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    // Verify state is cleaned up
    assert!(
        !RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state"),
        "Rebase state should be cleaned up after abort"
    );
    assert!(
        !rebase_dir.exists(),
        "legacy rebase-merge directory should not exist after abort"
    );
}

#[tokio::test]
#[serial]
async fn test_rebase_fast_forward_branch_behind() {
    use libra::internal::head::Head;

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Initial commit on master
    fs::write(temp_path.path().join("file.txt"), "base").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".to_string()],
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
        message: Some("Base commit".to_string()),
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

    // Create feature branch at the same commit
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".to_string()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    // Advance master by one commit
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("file.txt"), "master-advance").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".to_string()],
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
        message: Some("Advance master".to_string()),
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

    let master_head = Head::current_commit().await.unwrap();

    // Rebase feature onto master (fast-forward)
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: Some("main".to_string()),
        continue_rebase: false,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    match Head::current().await {
        Head::Branch(name) => assert_eq!(name, "feature", "Should be on feature branch"),
        _ => panic!("Should be on a branch after fast-forward"),
    }

    let feature_head = Head::current_commit().await.unwrap();
    assert_eq!(
        feature_head, master_head,
        "Feature should fast-forward to master"
    );

    let content = fs::read_to_string(temp_path.path().join("file.txt")).unwrap();
    assert_eq!(content, "master-advance");
}

#[tokio::test]
#[serial]
async fn test_rebase_fast_forward_blocks_dirty_workdir() {
    use libra::internal::head::Head;

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Base commit on master
    fs::write(temp_path.path().join("file.txt"), "base").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".to_string()],
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
        message: Some("Base commit".to_string()),
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

    // Create feature branch at base
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".to_string()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    // Advance master by one commit
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("file.txt"), "master-advance").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".to_string()],
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
        message: Some("Advance master".to_string()),
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

    // Switch to feature and introduce a dirty tracked file
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    let feature_head = Head::current_commit().await.unwrap();
    fs::write(temp_path.path().join("file.txt"), "local-modification").unwrap();

    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: Some("main".to_string()),
        continue_rebase: false,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    match Head::current().await {
        Head::Branch(name) => assert_eq!(name, "feature", "Should stay on feature branch"),
        _ => panic!("Should be on a branch after failed fast-forward"),
    }

    let feature_head_after = Head::current_commit().await.unwrap();
    assert_eq!(
        feature_head_after, feature_head,
        "Feature should not move with dirty workdir"
    );

    let content = fs::read_to_string(temp_path.path().join("file.txt")).unwrap();
    assert_eq!(content, "local-modification");
}

#[tokio::test]
#[serial]
async fn test_rebase_fast_forward_blocks_untracked_overwrite() {
    use libra::internal::head::Head;

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Base commit on master
    fs::write(temp_path.path().join("base.txt"), "base").unwrap();
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
        message: Some("Base commit".to_string()),
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

    // Create feature branch at base
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".to_string()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    // Advance master with a new file that will conflict with untracked
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("new.txt"), "master-content").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["new.txt".to_string()],
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
        message: Some("Add new.txt".to_string()),
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

    // Switch to feature and create untracked file that would be overwritten
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    let feature_head = Head::current_commit().await.unwrap();
    fs::write(temp_path.path().join("new.txt"), "local-untracked").unwrap();

    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: Some("main".to_string()),
        continue_rebase: false,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    match Head::current().await {
        Head::Branch(name) => assert_eq!(name, "feature", "Should stay on feature branch"),
        _ => panic!("Should be on a branch after failed fast-forward"),
    }

    let feature_head_after = Head::current_commit().await.unwrap();
    assert_eq!(
        feature_head_after, feature_head,
        "Feature should not move when untracked would be overwritten"
    );

    let content = fs::read_to_string(temp_path.path().join("new.txt")).unwrap();
    assert_eq!(content, "local-untracked");
}

#[tokio::test]
#[serial]
async fn test_rebase_blocks_dirty_workdir_non_fast_forward() {
    use libra::{command::rebase::RebaseState, internal::head::Head};

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Base commit on master
    fs::write(temp_path.path().join("file.txt"), "base").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".to_string()],
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
        message: Some("Base commit".to_string()),
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

    // Create feature branch and add a commit
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".to_string()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("file.txt"), "feature").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".to_string()],
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
        message: Some("Feature commit".to_string()),
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

    // Advance master to force a non-fast-forward rebase
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("file.txt"), "main").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".to_string()],
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
        message: Some("Advance master".to_string()),
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

    // Switch back to feature and introduce a dirty tracked file
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    let feature_head = Head::current_commit().await.unwrap();
    fs::write(temp_path.path().join("file.txt"), "dirty").unwrap();

    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: Some("main".to_string()),
        continue_rebase: false,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    match Head::current().await {
        Head::Branch(name) => assert_eq!(name, "feature", "Should stay on feature branch"),
        _ => panic!("Should be on a branch after failed rebase"),
    }

    let feature_head_after = Head::current_commit().await.unwrap();
    assert_eq!(
        feature_head_after, feature_head,
        "Feature should not move with dirty workdir"
    );

    let content = fs::read_to_string(temp_path.path().join("file.txt")).unwrap();
    assert_eq!(content, "dirty");

    assert!(
        !RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state")
    );
}

#[tokio::test]
#[serial]
async fn test_rebase_conflict_preserves_non_conflicting_workdir() {
    use libra::command::rebase::RebaseState;

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Base commit on master
    fs::write(temp_path.path().join("conflict.txt"), "base").unwrap();
    fs::write(temp_path.path().join("clean.txt"), "base-clean").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string(), "clean.txt".to_string()],
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
        message: Some("Base".to_string()),
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

    // Feature commit modifies both files (conflict + clean)
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".to_string()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("conflict.txt"), "feature-conflict").unwrap();
    fs::write(temp_path.path().join("clean.txt"), "feature-clean").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string(), "clean.txt".to_string()],
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
        message: Some("Feature changes".to_string()),
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

    // Master conflicting change
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("conflict.txt"), "master-conflict").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string()],
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
        message: Some("Master conflict".to_string()),
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

    // Rebase feature onto master; should stop with conflict
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: Some("main".to_string()),
        continue_rebase: false,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    assert!(
        RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state"),
        "Expected conflict to stop rebase"
    );

    let clean_content = fs::read_to_string(temp_path.path().join("clean.txt")).unwrap();
    assert_eq!(
        clean_content, "feature-clean",
        "Non-conflicting file should be updated in workdir"
    );

    // Clean up
    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: None,
        continue_rebase: false,
        abort: true,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;
}

#[tokio::test]
#[serial]
async fn test_rebase_conflict_does_not_overwrite_untracked_paths() {
    use libra::command::rebase::RebaseState;

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Base commit on master
    fs::write(temp_path.path().join("conflict.txt"), "base").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string()],
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
        message: Some("Base".to_string()),
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

    // Feature commit adds a file and changes conflict.txt
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".to_string()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("conflict.txt"), "feature").unwrap();
    fs::write(temp_path.path().join("new.txt"), "added").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string(), "new.txt".to_string()],
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
        message: Some("Feature adds new.txt".to_string()),
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

    // Remove the file so HEAD no longer tracks it.
    fs::remove_file(temp_path.path().join("new.txt")).unwrap();
    commit::execute(CommitArgs {
        message: Some("Feature removes new.txt".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: true,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    // Master conflicting change
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("conflict.txt"), "main").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string()],
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
        message: Some("Master conflict".to_string()),
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

    // Rebase feature onto master with an untracked file at the added path.
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("new.txt"), "keep me").unwrap();

    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: Some("main".to_string()),
        continue_rebase: false,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    let new_content = fs::read_to_string(temp_path.path().join("new.txt")).unwrap();
    assert_eq!(new_content, "keep me");

    assert!(
        RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state"),
        "Expected rebase to stop for untracked overwrite"
    );

    // Clean up
    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: None,
        continue_rebase: false,
        abort: true,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;
}

#[tokio::test]
#[serial]
async fn test_rebase_continue_requires_resolution() {
    use libra::{command::rebase::RebaseState, internal::head::Head};

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Base commit on master
    fs::write(temp_path.path().join("conflict.txt"), "base").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string()],
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
        message: Some("Base".to_string()),
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

    // Feature commit
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".to_string()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("conflict.txt"), "feature").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string()],
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
        message: Some("Feature".to_string()),
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

    // Master conflict
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    fs::write(temp_path.path().join("conflict.txt"), "main").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["conflict.txt".to_string()],
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
        message: Some("Master".to_string()),
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

    // Start rebase
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: Some("main".to_string()),
        continue_rebase: false,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    assert!(
        RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state"),
        "Expected conflict to stop rebase"
    );

    let head_before = Head::current_commit().await.unwrap();

    // Continue without resolving conflicts
    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: None,
        continue_rebase: true,
        abort: false,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;

    assert!(
        RebaseState::is_in_progress()
            .await
            .expect("failed to query rebase state"),
        "Rebase should remain in progress after unresolved continue"
    );
    let head_after = Head::current_commit().await.unwrap();
    assert_eq!(head_before, head_after, "HEAD should not move");

    // Clean up
    execute(RebaseArgs {
        no_rerere_autoupdate: false,
        keep_empty: false,
        no_keep_empty: false,
        empty: None,
        autostash: false,
        no_autostash: false,
        exec: Vec::new(),
        update_refs: false,
        no_update_refs: false,
        fork_point: false,
        no_fork_point: false,
        onto: None,
        branch: None,
        upstream: None,
        continue_rebase: false,
        abort: true,
        skip: false,
        autosquash: false,
        reapply_cherry_picks: false,
    })
    .await;
}

/// `libra rebase --help` surfaces the EXAMPLES banner so users see the
/// four mode invocations (start / `--continue` / `--skip` / `--abort`)
/// plus the JSON variant before they have to deal with conflict state.
/// Cross-cutting `--help` EXAMPLES rollout per
/// `docs/development/commands/_general.md` item B.
#[test]
fn test_rebase_help_lists_examples_banner() {
    let repo = tempdir().expect("tempdir for rebase --help");
    let output = run_libra_command(&["rebase", "--help"], repo.path());
    assert!(
        output.status.success(),
        "rebase --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "rebase --help should include EXAMPLES banner, stdout: {stdout}"
    );
    for invocation in [
        "libra rebase main",
        "libra rebase --continue",
        "libra rebase --skip",
        "libra rebase --abort",
        "libra rebase --json main",
    ] {
        assert!(
            stdout.contains(invocation),
            "rebase --help should include `{invocation}`, stdout: {stdout}"
        );
    }
}

// ── PR-14: rebase --onto <newbase> [<upstream>] [<branch>] ──────────────────

/// Resolve a revision to its full OID via the CLI.
fn rev_parse_cli(repo: &Path, rev: &str) -> String {
    let output = run_libra_command(&["rev-parse", rev], repo);
    assert_cli_success(&output, &format!("rev-parse {rev}"));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// HEAD-first first-parent commit messages via `log --oneline`.
fn log_messages_cli(repo: &Path) -> Vec<String> {
    let output = run_libra_command(&["log", "--oneline"], repo);
    assert_cli_success(&output, "log --oneline");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| {
            line.split_once(' ')
                .map(|(_, msg)| msg)
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .collect()
}

/// Build `base -> A` on main; `topic` (from A) -> `T1, T2`; main -> `B`. Returns
/// `(repo, A_oid, B_oid)`. Leaves `topic` checked out. The `A..topic` range is
/// `{T1, T2}`; `B` is the newbase candidate.
fn create_cli_rebase_onto_repo() -> (tempfile::TempDir, String, String) {
    let repo = tempdir().expect("failed to create temp repo");
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);

    commit_file_via_cli(p, "base.txt", "base\n", "base");
    commit_file_via_cli(p, "a.txt", "a\n", "A");
    let a = rev_parse_cli(p, "HEAD");

    assert_cli_success(
        &run_libra_command(&["switch", "-c", "topic"], p),
        "switch -c topic",
    );
    commit_file_via_cli(p, "t1.txt", "t1\n", "T1");
    commit_file_via_cli(p, "t2.txt", "t2\n", "T2");

    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    commit_file_via_cli(p, "b.txt", "b\n", "B");
    let b = rev_parse_cli(p, "HEAD");

    assert_cli_success(&run_libra_command(&["switch", "topic"], p), "switch topic");
    (repo, a, b)
}

#[test]
fn test_rebase_onto_basic_graph_replays_range_onto_newbase() {
    let (repo, a, _b) = create_cli_rebase_onto_repo();
    let p = repo.path();

    let output = run_libra_command(&["--json", "rebase", "--onto", "main", &a], p);
    assert_cli_success(&output, "rebase --onto main <A>");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["status"], "completed");
    assert_eq!(json["data"]["replay_count"], 2);

    // topic must land T1/T2 onto B: base -> A -> B -> T1' -> T2' (HEAD-first).
    let msgs = log_messages_cli(p);
    assert!(msgs.len() >= 5, "unexpected history: {msgs:?}");
    assert_eq!(
        &msgs[..5],
        &["T2", "T1", "B", "A", "base"],
        "range A..topic must replay onto B, got: {msgs:?}"
    );
}

#[test]
fn test_rebase_onto_json_distinguishes_onto_from_upstream() {
    let (repo, a, b) = create_cli_rebase_onto_repo();

    let output = run_libra_command(&["--json", "rebase", "--onto", &b, &a], repo.path());
    assert_cli_success(&output, "rebase --onto <B> <A>");
    let json = parse_json_stdout(&output);
    assert_eq!(
        json["data"]["onto"].as_str().unwrap(),
        b,
        "onto must be the newbase OID, not the upstream"
    );
    assert_eq!(
        json["data"]["upstream"].as_str().unwrap(),
        a,
        "upstream must be the upstream argument"
    );
    assert_eq!(json["data"]["replay_count"], 2);
}

#[test]
fn test_rebase_without_onto_lands_on_upstream() {
    let repo = create_cli_rebase_success_repo();
    let p = repo.path();

    let main_oid = rev_parse_cli(p, "main");
    let output = run_libra_command(&["--json", "rebase", "main"], p);
    assert_cli_success(&output, "plain rebase main");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["status"], "completed");
    // Plain rebase: the landing point IS the upstream commit.
    assert_eq!(json["data"]["onto"].as_str().unwrap(), main_oid);
    assert_eq!(json["data"]["upstream"], "main");
}

#[test]
fn test_rebase_onto_empty_range_does_not_move_branch() {
    let repo = tempdir().expect("failed to create temp repo");
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    commit_file_via_cli(p, "base.txt", "base\n", "base");
    commit_file_via_cli(p, "a.txt", "a\n", "A");
    let a = rev_parse_cli(p, "HEAD");

    // topic stays AT A (no extra commits) -> A..topic is empty.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "topic"], p),
        "switch -c topic",
    );
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    commit_file_via_cli(p, "b.txt", "b\n", "B");
    let b = rev_parse_cli(p, "main");
    assert_cli_success(&run_libra_command(&["switch", "topic"], p), "switch topic");

    let topic_before = rev_parse_cli(p, "topic");
    let output = run_libra_command(&["--json", "rebase", "--onto", &b, &a], p);
    assert_cli_success(&output, "rebase --onto B A (empty range)");
    let json = parse_json_stdout(&output);
    assert!(
        matches!(
            json["data"]["status"].as_str(),
            Some("no-commits") | Some("already-up-to-date")
        ),
        "empty range must not replay, status: {}",
        json["data"]["status"]
    );
    assert_eq!(json["data"]["onto"].as_str().unwrap(), b);
    assert_eq!(
        rev_parse_cli(p, "topic"),
        topic_before,
        "an empty --onto range must not move the branch"
    );
}

#[test]
fn test_rebase_onto_conflict_then_continue() {
    let repo = tempdir().expect("failed to create temp repo");
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    commit_file_via_cli(p, "conflict.txt", "base\n", "base");
    commit_file_via_cli(p, "a.txt", "a\n", "A");
    let a = rev_parse_cli(p, "HEAD");

    assert_cli_success(
        &run_libra_command(&["switch", "-c", "topic"], p),
        "switch -c topic",
    );
    commit_file_via_cli(p, "conflict.txt", "topic\n", "T edits conflict");

    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    commit_file_via_cli(p, "conflict.txt", "newbase\n", "B edits conflict");

    assert_cli_success(&run_libra_command(&["switch", "topic"], p), "switch topic");

    // Replaying T (base->topic) onto B (conflict=newbase) conflicts.
    let output = run_libra_command(&["rebase", "--onto", "main", &a], p);
    assert_eq!(
        output.status.code(),
        Some(128),
        "expected a replay conflict"
    );
    let conflicted = fs::read_to_string(p.join("conflict.txt")).unwrap();
    assert!(
        conflicted.contains("<<<<<<<"),
        "expected conflict markers: {conflicted}"
    );

    // Resolve and continue.
    fs::write(p.join("conflict.txt"), "resolved\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "conflict.txt"], p),
        "stage resolution",
    );
    let cont = run_libra_command(&["--json", "rebase", "--continue"], p);
    assert_cli_success(&cont, "rebase --continue after onto conflict");

    // topic landed on B: base -> A -> B -> T' (HEAD-first).
    let msgs = log_messages_cli(p);
    assert_eq!(
        &msgs[..4],
        &["T edits conflict", "B edits conflict", "A", "base"],
        "continued rebase must land on the newbase chain, got: {msgs:?}"
    );
}

#[test]
fn test_rebase_onto_with_branch_positional_switches_first() {
    let (repo, a, _b) = create_cli_rebase_onto_repo();
    let p = repo.path();

    // Move off topic, then name it as the third positional.
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    let output = run_libra_command(&["--json", "rebase", "--onto", "main", &a, "topic"], p);
    assert_cli_success(&output, "rebase --onto main A topic");

    let current = run_libra_command(&["branch", "--show-current"], p);
    assert_eq!(
        String::from_utf8_lossy(&current.stdout).trim(),
        "topic",
        "the named <branch> must be checked out"
    );
    let msgs = log_messages_cli(p);
    assert_eq!(
        &msgs[..5],
        &["T2", "T1", "B", "A", "base"],
        "history: {msgs:?}"
    );
}

#[test]
fn test_rebase_onto_nonexistent_branch_fails() {
    let (repo, a, _b) = create_cli_rebase_onto_repo();
    let output = run_libra_command(
        &["rebase", "--onto", "main", &a, "no-such-branch"],
        repo.path(),
    );
    assert_ne!(
        output.status.code(),
        Some(0),
        "a missing <branch> must fail rather than silently rebase the current branch"
    );
}

#[test]
fn test_rebase_onto_unresolvable_newbase_fails() {
    let (repo, a, _b) = create_cli_rebase_onto_repo();
    let output = run_libra_command(&["rebase", "--onto", "does-not-exist", &a], repo.path());
    assert_ne!(
        output.status.code(),
        Some(0),
        "unresolvable --onto must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does-not-exist"),
        "error should name the bad --onto target, got: {stderr}"
    );
}

#[test]
fn test_rebase_help_lists_onto_and_examples() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["rebase", "--help"], repo.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--onto"),
        "rebase --help must list --onto: {stdout}"
    );
    assert!(
        stdout.contains("EXAMPLES:"),
        "rebase --help must render the EXAMPLES banner: {stdout}"
    );
}

#[test]
#[serial]
fn test_rebase_keep_empty_is_accepted_noop_and_preserves_empty_commit() {
    // Libra's rebase keeps empty commits by default, so `--keep-empty` (which
    // explicitly requests that default) is an accepted no-op: it must not error,
    // and the empty commit must survive the rebase.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // dev branch: an empty commit followed by a real one.
    assert_cli_success(&run_libra_command(&["branch", "dev"], p), "branch dev");
    assert_cli_success(&run_libra_command(&["checkout", "dev"], p), "checkout dev");
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--allow-empty",
                "-m",
                "empty-commit",
                "--no-verify",
            ],
            p,
        ),
        "empty commit",
    );
    fs::write(p.join("dev.txt"), "dev\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "dev.txt"], p), "add dev.txt");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "real-dev", "--no-verify"], p),
        "real dev commit",
    );

    // Advance main.
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main",
    );
    fs::write(p.join("main.txt"), "main\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "main.txt"], p), "add main.txt");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-advance", "--no-verify"], p),
        "main advance",
    );

    // Rebase dev onto main with --keep-empty: accepted, succeeds, empty commit kept.
    assert_cli_success(
        &run_libra_command(&["checkout", "dev"], p),
        "checkout dev again",
    );
    let out = run_libra_command(&["rebase", "--keep-empty", "main"], p);
    assert_cli_success(&out, "rebase --keep-empty");
    let log =
        String::from_utf8_lossy(&run_libra_command(&["log", "--pretty=%s"], p).stdout).into_owned();
    assert!(
        log.contains("empty-commit"),
        "--keep-empty must preserve the empty commit through the rebase:\n{log}"
    );
    assert!(
        log.contains("real-dev") && log.contains("main-advance"),
        "rebase should have replayed dev onto the advanced main:\n{log}"
    );
}

#[test]
#[serial]
fn test_rebase_no_keep_empty_drops_start_empty_commits() {
    // `--no-keep-empty` drops commits that are already empty in the source history
    // (no change vs their parent), while a real commit is still replayed. The
    // default (and `--keep-empty`) keeps the empty commit.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // dev: an empty commit followed by a real one.
    assert_cli_success(&run_libra_command(&["branch", "dev"], p), "branch dev");
    assert_cli_success(&run_libra_command(&["checkout", "dev"], p), "checkout dev");
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--allow-empty",
                "-m",
                "empty-commit",
                "--no-verify",
            ],
            p,
        ),
        "empty commit",
    );
    std::fs::write(p.join("dev.txt"), "dev\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "dev.txt"], p), "add dev.txt");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "real-dev", "--no-verify"], p),
        "real dev commit",
    );

    // Advance main so the rebase actually replays.
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main",
    );
    std::fs::write(p.join("main.txt"), "main\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "main.txt"], p), "add main.txt");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-advance", "--no-verify"], p),
        "main advance",
    );

    // rebase --no-keep-empty: the empty commit is dropped, the real one replayed.
    assert_cli_success(
        &run_libra_command(&["checkout", "dev"], p),
        "checkout dev again",
    );
    assert_cli_success(
        &run_libra_command(&["rebase", "--no-keep-empty", "main"], p),
        "rebase --no-keep-empty",
    );
    let log =
        String::from_utf8_lossy(&run_libra_command(&["log", "--pretty=%s"], p).stdout).into_owned();
    assert!(
        !log.contains("empty-commit"),
        "--no-keep-empty must drop the start-empty commit:\n{log}"
    );
    assert!(
        log.contains("real-dev") && log.contains("main-advance"),
        "the real commit is still replayed onto the advanced main:\n{log}"
    );
}

#[test]
#[serial]
fn test_rebase_no_keep_empty_all_empty_range_moves_branch_to_base() {
    // When `--no-keep-empty` drops EVERY commit in the range (all start-empty),
    // the branch must still be rebased onto the advanced upstream — not left at
    // its old tip with the empty commits silently retained.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // dev: only empty commits.
    assert_cli_success(&run_libra_command(&["branch", "dev"], p), "branch dev");
    assert_cli_success(&run_libra_command(&["checkout", "dev"], p), "checkout dev");
    assert_cli_success(
        &run_libra_command(
            &["commit", "--allow-empty", "-m", "empty-1", "--no-verify"],
            p,
        ),
        "empty-1",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "--allow-empty", "-m", "empty-2", "--no-verify"],
            p,
        ),
        "empty-2",
    );

    // Advance main.
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main",
    );
    std::fs::write(p.join("main.txt"), "main\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "main.txt"], p), "add main.txt");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-advance", "--no-verify"], p),
        "main advance",
    );
    let main_tip = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "main"], p).stdout)
        .trim()
        .to_string();

    // rebase --no-keep-empty: all commits dropped, dev moves to main's tip.
    assert_cli_success(
        &run_libra_command(&["checkout", "dev"], p),
        "checkout dev again",
    );
    assert_cli_success(
        &run_libra_command(&["rebase", "--no-keep-empty", "main"], p),
        "rebase --no-keep-empty (all empty)",
    );
    let dev_tip = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    assert_eq!(
        dev_tip, main_tip,
        "dev with only empty commits must rebase onto main's tip, not stay put"
    );
    let log =
        String::from_utf8_lossy(&run_libra_command(&["log", "--pretty=%s"], p).stdout).into_owned();
    assert!(
        !log.contains("empty-1") && !log.contains("empty-2"),
        "all start-empty commits must be dropped:\n{log}"
    );
}

/// Build a repo where `topic` has a commit that BECOMES empty when rebased onto
/// `main` (both add the identical change) plus a genuinely new commit. Leaves
/// HEAD on `topic`, with `main` as the rebase upstream.
fn build_become_empty_rebase_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "topic"], p),
        "branch topic",
    );
    std::fs::write(p.join("shared.txt"), "X\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add topic X");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "topic adds X", "--no-verify"], p),
        "commit topic X",
    );
    std::fs::write(p.join("shared.txt"), "X\nY\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add topic Y");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "topic adds Y", "--no-verify"], p),
        "commit topic Y",
    );
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    std::fs::write(p.join("shared.txt"), "X\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add main X");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main adds X", "--no-verify"], p),
        "commit main X",
    );
    assert_cli_success(&run_libra_command(&["switch", "topic"], p), "switch topic");
    repo
}

/// `--empty=drop` skips a commit that becomes empty after replay (its change is
/// already on the new base), reporting the git-style `dropping … upstream`
/// notice, while still replaying the genuinely-new commit.
#[test]
#[serial]
fn test_rebase_empty_drop_skips_become_empty_commit() {
    let repo = build_become_empty_rebase_repo();
    let p = repo.path();
    let out = run_libra_command(&["rebase", "--empty=drop", "main"], p);
    assert_cli_success(&out, "rebase --empty=drop");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("dropping")
            && stdout.contains("topic adds X")
            && stdout.contains("already upstream"),
        "--empty=drop reports the dropped become-empty commit:\n{stdout}"
    );
    let log =
        String::from_utf8_lossy(&run_libra_command(&["log", "--pretty=%s"], p).stdout).into_owned();
    assert!(
        log.contains("topic adds Y"),
        "the new commit is replayed:\n{log}"
    );
    assert!(
        !log.contains("topic adds X"),
        "the become-empty commit was dropped:\n{log}"
    );
    assert!(log.contains("main adds X"), "main's commit remains:\n{log}");
}

/// Without `--empty` (Libra's default) the become-empty commit is KEPT — an
/// intentional divergence from Git, which drops it.
#[test]
#[serial]
fn test_rebase_empty_default_keeps_become_empty_commit() {
    let repo = build_become_empty_rebase_repo();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["rebase", "main"], p),
        "rebase (default keep)",
    );
    let log =
        String::from_utf8_lossy(&run_libra_command(&["log", "--pretty=%s"], p).stdout).into_owned();
    assert!(
        log.contains("topic adds X") && log.contains("topic adds Y"),
        "default rebase keeps the become-empty commit:\n{log}"
    );
}

/// `--empty=stop`/`--empty=ask` (valid Git modes Libra does not support) and any
/// unknown value are usage errors (exit 129) naming `--empty`.
#[test]
#[serial]
fn test_rebase_empty_invalid_mode_rejected() {
    let repo = build_become_empty_rebase_repo();
    let p = repo.path();
    for mode in ["stop", "ask", "bogus"] {
        let out = run_libra_command(&["rebase", &format!("--empty={mode}"), "main"], p);
        assert_eq!(
            out.status.code(),
            Some(129),
            "--empty={mode} is a usage error: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("--empty"),
            "--empty={mode} error names --empty"
        );
    }
}

/// `--empty=drop` survives a conflict + `--continue`: the mode round-trips
/// through `RebaseState`, so a LATER commit that becomes empty is dropped when
/// the resume reaches it (not replayed as an empty commit).
#[test]
#[serial]
fn test_rebase_empty_drop_survives_conflict_resume() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // topic: f1 conflicts on conflict.txt; f2 adds shared.txt=S (will become empty).
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "topic"], p),
        "branch topic",
    );
    std::fs::write(p.join("conflict.txt"), "topic-line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "conflict.txt"], p), "add f1");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "topic f1", "--no-verify"], p),
        "commit f1",
    );
    std::fs::write(p.join("shared.txt"), "S\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add f2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "topic f2 adds S", "--no-verify"], p),
        "commit f2",
    );
    // main: conflicting edit to conflict.txt AND already add the identical shared.txt.
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    std::fs::write(p.join("conflict.txt"), "main-line\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "conflict.txt"], p),
        "add main edit",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main edits conflict", "--no-verify"], p),
        "commit main edit",
    );
    std::fs::write(p.join("shared.txt"), "S\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add main S");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main adds S", "--no-verify"], p),
        "commit main S",
    );
    assert_cli_success(&run_libra_command(&["switch", "topic"], p), "switch topic");

    // Start rebase --empty=drop: f1 conflicts and stops with resumable state.
    let start = run_libra_command(&["rebase", "--empty=drop", "main"], p);
    assert_eq!(
        start.status.code(),
        Some(128),
        "f1 conflict stops the rebase"
    );
    assert!(
        String::from_utf8_lossy(&start.stderr).contains("--continue"),
        "the conflict stop points at --continue (resumable state): {}",
        String::from_utf8_lossy(&start.stderr)
    );

    // Resolve f1 and continue; the resume reaches f2 (become-empty) and, because
    // --empty=drop round-tripped through the state, drops it.
    std::fs::write(p.join("conflict.txt"), "resolved\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "conflict.txt"], p),
        "stage resolution",
    );
    let cont = run_libra_command(&["rebase", "--continue"], p);
    assert_cli_success(&cont, "--continue drops the redundant f2 and finishes");
    let cont_out = String::from_utf8_lossy(&cont.stdout);
    assert!(
        cont_out.contains("dropping")
            && cont_out.contains("topic f2 adds S")
            && cont_out.contains("already upstream"),
        "the resumed become-empty f2 is reported as dropped:\n{cont_out}"
    );
    let log =
        String::from_utf8_lossy(&run_libra_command(&["log", "--pretty=%s"], p).stdout).into_owned();
    assert!(
        log.contains("topic f1"),
        "f1 (resolved) is replayed:\n{log}"
    );
    assert!(
        !log.contains("topic f2 adds S"),
        "the become-empty f2 was dropped on resume:\n{log}"
    );
    // State cleared (sequence complete): another --continue errors with no rebase.
    let after = run_libra_command(&["rebase", "--continue"], p);
    assert_ne!(
        after.status.code(),
        Some(0),
        "no rebase in progress after completion"
    );
    assert!(
        String::from_utf8_lossy(&after.stderr).contains("no rebase in progress"),
        "state cleared after the sequence completes: {}",
        String::from_utf8_lossy(&after.stderr)
    );
}
