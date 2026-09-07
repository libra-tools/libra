//! Tests SwitchError variant coverage: exit codes, stable error codes, and hints.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use libra::{
    command::switch::SwitchError,
    utils::error::{CliError, StableErrorCode},
};

use super::*;

fn assert_cli_error_contract(
    output: &std::process::Output,
    expected_exit: i32,
    expected_code: StableErrorCode,
    expected_message: &str,
    expected_hints: &[&str],
) {
    assert_eq!(output.status.code(), Some(expected_exit));
    let (_human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.exit_code, expected_exit);
    assert_eq!(report.error_code, expected_code.as_str());
    assert!(
        report.message.contains(expected_message),
        "expected message containing {expected_message:?}, got: {}",
        report.message
    );
    for hint in expected_hints {
        assert!(
            report
                .hints
                .iter()
                .any(|candidate| candidate.contains(hint)),
            "expected hint containing {hint:?}, got: {:?}",
            report.hints
        );
    }
}

fn assert_mapped_contract(
    error: SwitchError,
    expected_code: StableErrorCode,
    expected_exit: i32,
    expected_message: &str,
    expected_hints: &[&str],
) {
    let err = CliError::from(error);
    assert_eq!(err.stable_code(), expected_code);
    assert_eq!(err.exit_code(), expected_exit);
    assert!(
        err.message().contains(expected_message),
        "expected message containing {expected_message:?}, got: {}",
        err.message()
    );
    for hint in expected_hints {
        assert!(
            err.hints()
                .iter()
                .any(|candidate| candidate.as_str().contains(hint)),
            "expected hint containing {hint:?}, got: {:?}",
            err.hints()
                .iter()
                .map(|candidate| candidate.as_str())
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn branch_not_found_returns_cli_error_code_and_create_hint() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["switch", "nonexistent-branch"], repo.path());
    assert_cli_error_contract(
        &output,
        129,
        StableErrorCode::CliInvalidTarget,
        "branch 'nonexistent-branch' not found",
        &["libra switch -c nonexistent-branch"],
    );
}

#[test]
fn branch_not_found_levenshtein_suggestion() {
    let repo = create_committed_repo_via_cli();
    let _ = run_libra_command(&["switch", "-c", "feature"], repo.path());
    let _ = run_libra_command(&["switch", "main"], repo.path());

    let output = run_libra_command(&["switch", "featur"], repo.path());
    assert_eq!(output.status.code(), Some(129));
    let (_human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(
        report.error_code,
        StableErrorCode::CliInvalidTarget.as_str()
    );
    assert!(
        report
            .hints
            .iter()
            .any(|hint| hint.contains("did you mean 'feature'?")),
        "expected Levenshtein suggestion, got: {:?}",
        report.hints
    );
}

#[test]
fn missing_track_target_returns_cli_error_contract() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["switch", "--track"], repo.path());
    assert_cli_error_contract(
        &output,
        129,
        StableErrorCode::CliInvalidArguments,
        "remote branch name is required",
        &["origin/main"],
    );
}

#[test]
fn missing_detach_target_returns_cli_error_contract() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["switch", "--detach"], repo.path());
    assert_cli_error_contract(
        &output,
        129,
        StableErrorCode::CliInvalidArguments,
        "branch name is required when using --detach",
        &["provide a commit, tag, or branch to detach at"],
    );
}

#[test]
fn invalid_remote_branch_returns_cli_invalid_target() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["switch", "--track", "refs/remotes/origin"], repo.path());
    assert_cli_error_contract(
        &output,
        129,
        StableErrorCode::CliInvalidTarget,
        "invalid remote branch 'refs/remotes/origin'",
        &["expected format: 'remote/branch'."],
    );
}

#[test]
fn remote_branch_not_found_returns_fetch_hint() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["switch", "--track", "origin/missing"], repo.path());
    assert_cli_error_contract(
        &output,
        129,
        StableErrorCode::CliInvalidTarget,
        "remote branch 'origin/missing' not found",
        &["libra fetch origin"],
    );
}

#[tokio::test]
#[serial(cwd)]
async fn got_remote_branch_suggests_track() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    let head = Head::current_commit().await.unwrap();
    // Store with name="feature", remote=Some("origin") so search_branch("origin/feature")
    // splits on '/' and finds (remote="origin", name="feature").
    Branch::update_branch("feature", &head.to_string(), Some("origin"))
        .await
        .unwrap();

    let output = run_libra_command(&["switch", "origin/feature"], repo.path());
    assert_cli_error_contract(
        &output,
        129,
        StableErrorCode::CliInvalidTarget,
        "a branch is expected, got remote branch 'origin/feature'",
        &["libra switch --track origin/feature"],
    );
}

#[test]
fn ambiguous_guess_remote_maps_to_conflict_with_disambiguation_hints() {
    assert_mapped_contract(
        SwitchError::AmbiguousGuessRemote {
            branch: "feature".to_string(),
            remotes: vec!["origin".to_string(), "upstream".to_string()],
        },
        StableErrorCode::ConflictOperationBlocked,
        128,
        "'feature' matched multiple remote-tracking branches",
        &[
            "it exists on remotes: origin, upstream.",
            "libra switch --track <remote>/feature",
            "checkout.defaultRemote",
        ],
    );
}

#[test]
fn dirty_unstaged_returns_exit_128_with_repo_state_error() {
    let repo = create_committed_repo_via_cli();
    let _ = run_libra_command(&["switch", "-c", "other"], repo.path());
    let _ = run_libra_command(&["switch", "main"], repo.path());

    std::fs::write(repo.path().join("tracked.txt"), "modified content\n").unwrap();

    let output = run_libra_command(&["switch", "other"], repo.path());
    assert_cli_error_contract(
        &output,
        128,
        StableErrorCode::RepoStateInvalid,
        "unstaged changes, can't switch branch",
        &["commit or stash your changes before switching"],
    );
}

#[test]
fn dirty_uncommitted_returns_exit_128_with_repo_state_error() {
    let repo = create_committed_repo_via_cli();
    let _ = run_libra_command(&["switch", "-c", "other"], repo.path());
    let _ = run_libra_command(&["switch", "main"], repo.path());

    std::fs::write(repo.path().join("new.txt"), "new content\n").unwrap();
    let add = run_libra_command(&["add", "new.txt"], repo.path());
    assert_cli_success(&add, "add new.txt");

    let output = run_libra_command(&["switch", "other"], repo.path());
    assert_cli_error_contract(
        &output,
        128,
        StableErrorCode::RepoStateInvalid,
        "uncommitted changes, can't switch branch",
        &["commit or stash your changes before switching"],
    );
}

#[test]
fn untracked_file_conflicting_with_target_branch_is_rejected() {
    let repo = create_committed_repo_via_cli();

    let create = run_libra_command(&["switch", "-c", "other"], repo.path());
    assert_cli_success(&create, "switch -c other");

    std::fs::write(repo.path().join("conflict.txt"), "tracked on other\n").unwrap();
    let add = run_libra_command(&["add", "conflict.txt"], repo.path());
    assert_cli_success(&add, "add conflict.txt on other");
    let commit = run_libra_command(
        &["commit", "-m", "other adds conflict", "--no-verify"],
        repo.path(),
    );
    assert_cli_success(&commit, "commit conflict.txt on other");

    let back = run_libra_command(&["switch", "main"], repo.path());
    assert_cli_success(&back, "switch main");

    std::fs::write(repo.path().join("conflict.txt"), "local untracked\n").unwrap();

    let output = run_libra_command(&["switch", "other"], repo.path());
    assert_cli_error_contract(
        &output,
        128,
        StableErrorCode::ConflictOperationBlocked,
        "untracked working tree file would be overwritten by switch: conflict.txt",
        &["move or remove it before switching"],
    );

    let content = std::fs::read_to_string(repo.path().join("conflict.txt")).unwrap();
    assert_eq!(content, "local untracked\n");
}

#[test]
fn dirty_repo_branch_not_found_preserves_invalid_target_priority() {
    let repo = create_committed_repo_via_cli();
    std::fs::write(repo.path().join("tracked.txt"), "modified content\n").unwrap();

    let output = run_libra_command(&["switch", "missing-branch"], repo.path());
    assert_cli_error_contract(
        &output,
        129,
        StableErrorCode::CliInvalidTarget,
        "branch 'missing-branch' not found",
        &["libra switch -c missing-branch"],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unstaged changes, can't switch branch"),
        "target validation should win over dirty-worktree errors, got: {stderr}"
    );
}

#[test]
fn dirty_repo_create_existing_branch_preserves_conflict_priority() {
    let repo = create_committed_repo_via_cli();
    let create = run_libra_command(&["branch", "foo"], repo.path());
    assert_cli_success(&create, "branch foo");
    std::fs::write(repo.path().join("tracked.txt"), "modified content\n").unwrap();

    let output = run_libra_command(&["switch", "-c", "foo"], repo.path());
    assert_cli_error_contract(
        &output,
        128,
        StableErrorCode::ConflictOperationBlocked,
        "a branch named 'foo' already exists",
        &[],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unstaged changes, can't switch branch"),
        "create-branch conflicts should win over dirty-worktree errors, got: {stderr}"
    );
}

#[test]
fn internal_branch_blocked_returns_cli_invalid_target() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["switch", "intent"], repo.path());
    assert_cli_error_contract(
        &output,
        129,
        StableErrorCode::CliInvalidTarget,
        "'intent' is a reserved branch name",
        &[],
    );
}

#[tokio::test]
#[serial(cwd)]
async fn branch_already_exists_with_track_returns_conflict() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    let head = Head::current_commit().await.unwrap();
    Branch::update_branch(
        "refs/remotes/origin/main",
        &head.to_string(),
        Some("origin"),
    )
    .await
    .unwrap();

    let output = run_libra_command(&["switch", "--track", "origin/main"], repo.path());
    assert_cli_error_contract(
        &output,
        128,
        StableErrorCode::ConflictOperationBlocked,
        "a branch named 'main' already exists",
        &["libra switch main"],
    );
}

#[test]
fn commit_resolve_returns_cli_invalid_target() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["switch", "--detach", "no-such-revision"], repo.path());
    assert_cli_error_contract(
        &output,
        129,
        StableErrorCode::CliInvalidTarget,
        "failed to resolve commit",
        &["check the revision name and try again"],
    );
}

#[test]
fn status_check_preserves_io_read_failed() {
    let repo = create_committed_repo_via_cli();
    let branch = run_libra_command(&["branch", "other"], repo.path());
    assert_cli_success(&branch, "branch other");
    std::fs::write(
        repo.path().join(".libra").join("index"),
        b"not a valid index",
    )
    .unwrap();

    let output = run_libra_command(&["switch", "other"], repo.path());
    assert_cli_error_contract(
        &output,
        128,
        StableErrorCode::IoReadFailed,
        "failed to determine working tree status",
        &[],
    );
}

#[test]
fn delegated_cli_passthrough_keeps_original_contract() {
    let repo = create_committed_repo_via_cli();
    // Create a branch "foo", then try to create it again via switch -c.
    // "main" would be caught by is_locked_branch before reaching create_branch_safe,
    // so we need a non-locked branch that already exists.
    let create = run_libra_command(&["branch", "foo"], repo.path());
    assert_cli_success(&create, "branch foo");
    let output = run_libra_command(&["switch", "-c", "foo"], repo.path());
    assert_cli_error_contract(
        &output,
        128,
        StableErrorCode::ConflictOperationBlocked,
        "a branch named 'foo' already exists",
        &[],
    );
}

#[test]
fn mapping_contract_covers_non_cli_reachable_variants() {
    assert_mapped_contract(
        SwitchError::MissingBranchName,
        StableErrorCode::CliInvalidArguments,
        129,
        "branch name is required",
        &["provide a branch name"],
    );
    assert_mapped_contract(
        SwitchError::BranchCreate {
            branch: "feature".into(),
            detail: "disk full".into(),
        },
        StableErrorCode::IoWriteFailed,
        128,
        "failed to create branch 'feature': disk full",
        &[],
    );
    assert_mapped_contract(
        SwitchError::HeadUpdate("write refs failed".into()),
        StableErrorCode::IoWriteFailed,
        128,
        "failed to update HEAD: write refs failed",
        &[],
    );

    let delegated = CliError::fatal("delegated failure")
        .with_stable_code(StableErrorCode::ConflictOperationBlocked)
        .with_hint("preserve this hint");
    let passthrough = CliError::from(SwitchError::DelegatedCli(delegated.clone()));
    assert_eq!(passthrough, delegated);
}
