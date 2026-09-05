//! Tests merge command scenarios including fast-forward handling and conflict reporting.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::path::Path;

use git_internal::internal::object::commit::Commit;
use libra::{
    command::load_object,
    internal::{branch::Branch, head::Head},
    utils::test::ChangeDirGuard,
};
use serial_test::serial;

use super::{
    assert_cli_success, create_committed_repo_via_cli, parse_cli_error_stderr, parse_json_stdout,
    run_libra_command, run_libra_command_with_stdin, run_libra_command_with_stdin_and_env,
};

fn commit_file(repo: &Path, file: &str, content: &str, message: &str) {
    let path = repo.join(file);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create parent directory");
    }
    std::fs::write(path, content).expect("failed to write file");
    assert_cli_success(&run_libra_command(&["add", file], repo), "add file");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", message, "--no-verify"], repo),
        "commit file",
    );
}

#[test]
fn test_merge_cli_missing_branch_returns_error_1() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["merge", "no-such"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(stderr.contains("error: no-such - not something we can merge"));
}

#[test]
fn test_merge_json_fast_forward_outputs_summary() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );

    std::fs::write(temp_path.join("file.txt"), "Feature content").expect("failed to write file");
    assert_cli_success(&run_libra_command(&["add", "."], temp_path), "add file");
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "Add feature content", "--no-verify"],
            temp_path,
        ),
        "commit",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );

    let output = run_libra_command(&["--json", "merge", "feature"], temp_path);
    assert_cli_success(&output, "json merge feature");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "merge");
    assert_eq!(json["data"]["strategy"], "fast-forward");
    assert_eq!(json["data"]["up_to_date"], false);
    assert_eq!(json["data"]["files_changed"], 1);
    assert!(json["data"]["old_commit"].as_str().is_some());
    assert!(json["data"]["commit"].as_str().is_some());
    assert!(output.stderr.is_empty());
}

#[test]
fn test_merge_json_already_up_to_date_outputs_summary() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create branch",
    );

    let output = run_libra_command(&["--json", "merge", "feature"], temp_path);
    assert_cli_success(&output, "json merge up to date");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "merge");
    assert_eq!(json["data"]["strategy"], "already-up-to-date");
    assert_eq!(json["data"]["up_to_date"], true);
    assert_eq!(json["data"]["files_changed"], 0);
    assert!(json["data"]["old_commit"].as_str().is_some());
    assert!(json["data"]["commit"].is_null());
    assert!(output.stderr.is_empty());
}

#[test]
fn test_merge_machine_outputs_single_json_line() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create branch",
    );

    let output = run_libra_command(&["--machine", "merge", "feature"], temp_path);
    assert_cli_success(&output, "machine merge feature");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().count(),
        1,
        "expected one JSON line, got: {stdout}"
    );
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("expected JSON");
    assert_eq!(json["command"], "merge");
    assert_eq!(json["data"]["strategy"], "already-up-to-date");
    assert!(output.stderr.is_empty());
}

#[test]
fn test_merge_machine_fast_forward_outputs_single_json_line() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );

    std::fs::write(temp_path.join("file.txt"), "Feature content").expect("failed to write file");
    assert_cli_success(&run_libra_command(&["add", "."], temp_path), "add file");
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "Add feature content", "--no-verify"],
            temp_path,
        ),
        "commit",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );

    let output = run_libra_command(&["--machine", "merge", "feature"], temp_path);
    assert_cli_success(&output, "machine merge feature");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().count(),
        1,
        "expected one JSON line, got: {stdout}"
    );
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("expected JSON");
    assert_eq!(json["command"], "merge");
    assert_eq!(json["data"]["strategy"], "fast-forward");
    assert_eq!(json["data"]["up_to_date"], false);
    assert_eq!(json["data"]["files_changed"], 1);
    assert!(output.stderr.is_empty());
}

#[tokio::test]
/// Test fast-forward merge of local branches
async fn test_merge_fast_forward() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );

    // Commit changes on the feature branch
    std::fs::write(temp_path.join("file.txt"), "Feature content").expect("Failed to write file");
    assert_cli_success(&run_libra_command(&["add", "."], temp_path), "add file");
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "Add feature content", "--no-verify"],
            temp_path,
        ),
        "commit",
    );

    // Switch back to the main branch and perform fast-forward merge
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );

    let merge_output = run_libra_command(&["merge", "feature"], temp_path);
    assert!(
        merge_output.status.success(),
        "Fast-forward merge failed: {}",
        String::from_utf8_lossy(&merge_output.stderr)
    );
}

#[tokio::test]
#[serial(cwd)]
/// Test merging a remote branch
async fn test_merge_remote_branch() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );

    std::fs::write(temp_path.join("remote.txt"), "Remote content").expect("Failed to write file");
    assert_cli_success(&run_libra_command(&["add", "."], temp_path), "add file");
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "Add remote content", "--no-verify"],
            temp_path,
        ),
        "commit",
    );

    let _guard = ChangeDirGuard::new(temp_path);
    let feature_commit = Head::current_commit()
        .await
        .expect("feature branch should have a tip");
    Branch::update_branch("feature", &feature_commit.to_string(), Some("origin"))
        .await
        .unwrap();

    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );

    let merge_output = run_libra_command(&["merge", "origin/feature"], temp_path);
    assert!(
        merge_output.status.success(),
        "Merge remote branch failed: {}",
        String::from_utf8_lossy(&merge_output.stderr)
    );
}

#[tokio::test]
#[serial(cwd)]
/// Test JSON output when merging a remote branch reference.
async fn test_merge_json_remote_branch_outputs_summary() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );

    std::fs::write(temp_path.join("remote.txt"), "Remote content").expect("Failed to write file");
    assert_cli_success(&run_libra_command(&["add", "."], temp_path), "add file");
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "Add remote content", "--no-verify"],
            temp_path,
        ),
        "commit",
    );

    let _guard = ChangeDirGuard::new(temp_path);
    let feature_commit = Head::current_commit()
        .await
        .expect("feature branch should have a tip");
    Branch::update_branch("feature", &feature_commit.to_string(), Some("origin"))
        .await
        .unwrap();

    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );

    let output = run_libra_command(
        &["--json", "merge", "refs/remotes/origin/feature"],
        temp_path,
    );
    assert_cli_success(&output, "json merge remote branch");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "merge");
    assert_eq!(json["data"]["strategy"], "fast-forward");
    assert_eq!(json["data"]["up_to_date"], false);
    assert_eq!(json["data"]["files_changed"], 1);
    assert!(json["data"]["commit"].as_str().is_some());
    assert!(output.stderr.is_empty());
}

#[tokio::test]
#[serial(cwd)]
/// Test merging diverged branches with non-overlapping changes.
async fn test_merge_diverged_branch_creates_two_parent_commit() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    let output = run_libra_command(&["branch", "branch1"], temp_path);
    assert!(output.status.success(), "Failed to create branch1");

    let output = run_libra_command(&["checkout", "branch1"], temp_path);
    assert!(output.status.success(), "Failed to checkout branch1");

    commit_file(
        temp_path,
        "branch1.txt",
        "Branch1 content",
        "Add branch1 content",
    );

    let output = run_libra_command(&["checkout", "main"], temp_path);
    assert!(output.status.success(), "Failed to checkout main");

    let output = run_libra_command(&["branch", "branch2"], temp_path);
    assert!(output.status.success(), "Failed to create branch2");

    let output = run_libra_command(&["checkout", "branch2"], temp_path);
    assert!(output.status.success(), "Failed to checkout branch2");

    commit_file(
        temp_path,
        "branch2.txt",
        "Branch2 content",
        "Add branch2 content",
    );

    let output = run_libra_command(&["checkout", "branch1"], temp_path);
    assert!(output.status.success(), "Failed to checkout branch1");

    let merge_output = run_libra_command(&["merge", "branch2"], temp_path);
    assert_cli_success(&merge_output, "three-way merge");
    let stdout = String::from_utf8_lossy(&merge_output.stdout);
    assert!(
        stdout.contains("Merge made by the 'three-way' strategy."),
        "merge should report three-way strategy, stdout: {stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(temp_path.join("branch1.txt")).expect("read branch1"),
        "Branch1 content"
    );
    assert_eq!(
        std::fs::read_to_string(temp_path.join("branch2.txt")).expect("read branch2"),
        "Branch2 content"
    );

    let _guard = ChangeDirGuard::new(temp_path);
    let head = Head::current_commit()
        .await
        .expect("merge should create HEAD");
    let commit: Commit = load_object(&head).expect("load merge commit");
    assert_eq!(
        commit.parent_commit_ids.len(),
        2,
        "diverged merge should create a two-parent commit"
    );
    assert!(
        commit.message.starts_with('\n'),
        "merge commit body must retain Git's blank-line separator before the message"
    );
}

#[test]
fn test_merge_custom_message_via_dash_m() {
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();

    assert!(
        run_libra_command(&["checkout", "-b", "feat"], p)
            .status
            .success(),
        "create+checkout feat"
    );
    commit_file(p, "feat.txt", "feat content", "feat commit");
    assert!(
        run_libra_command(&["checkout", "main"], p).status.success(),
        "checkout main"
    );
    commit_file(p, "main.txt", "main content", "main commit");

    let merge = run_libra_command(&["merge", "-m", "MY CUSTOM MERGE MSG", "feat"], p);
    assert_cli_success(&merge, "merge -m custom feat");

    // The merge commit (HEAD) should carry the custom subject.
    let log = run_libra_command(&["log", "-n", "1", "--pretty=%s"], p);
    assert_cli_success(&log, "log -n 1 --pretty=%s");
    let subject = String::from_utf8_lossy(&log.stdout);
    assert!(
        subject.contains("MY CUSTOM MERGE MSG"),
        "merge commit subject should be the -m message, got: {subject}"
    );
}

#[test]
fn test_merge_squash_stages_without_committing() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    assert!(
        run_libra_command(&["checkout", "-b", "feat"], p)
            .status
            .success(),
        "checkout -b feat"
    );
    commit_file(p, "feat.txt", "feat content", "feat commit");
    assert!(
        run_libra_command(&["checkout", "main"], p).status.success(),
        "checkout main"
    );
    commit_file(p, "main.txt", "main content", "main commit");

    let before = run_libra_command(&["rev-parse", "HEAD"], p);
    let before_head = String::from_utf8_lossy(&before.stdout).trim().to_string();

    let merge = run_libra_command(&["merge", "--squash", "feat"], p);
    assert_cli_success(&merge, "merge --squash feat");
    let merge_out = String::from_utf8_lossy(&merge.stdout);
    assert!(
        merge_out.contains("Squash commit"),
        "expected squash message, got: {merge_out}"
    );

    // --squash must NOT move HEAD, but the merged file must be in the worktree.
    let after = run_libra_command(&["rev-parse", "HEAD"], p);
    assert_eq!(
        String::from_utf8_lossy(&after.stdout).trim(),
        before_head,
        "--squash must not move HEAD"
    );
    assert!(
        p.join("feat.txt").exists(),
        "merged file should be staged into the worktree"
    );

    // The staged result is finalized with a normal commit, which advances HEAD.
    let commit = run_libra_command(&["commit", "-m", "squashed merge", "--no-verify"], p);
    assert_cli_success(&commit, "commit after squash");
    let final_head = run_libra_command(&["rev-parse", "HEAD"], p);
    assert_ne!(
        String::from_utf8_lossy(&final_head.stdout).trim(),
        before_head,
        "HEAD should advance after committing the squashed result"
    );
}

#[test]
fn test_merge_no_commit_then_continue() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    assert!(
        run_libra_command(&["checkout", "-b", "feat"], p)
            .status
            .success(),
        "checkout -b feat"
    );
    commit_file(p, "feat.txt", "feat content", "feat commit");
    assert!(
        run_libra_command(&["checkout", "main"], p).status.success(),
        "checkout main"
    );
    commit_file(p, "main.txt", "main content", "main commit");

    let before = run_libra_command(&["rev-parse", "HEAD"], p);
    let before_head = String::from_utf8_lossy(&before.stdout).trim().to_string();

    // --no-commit stages the merge but does not move HEAD.
    let merge = run_libra_command(&["merge", "--no-commit", "feat"], p);
    assert_cli_success(&merge, "merge --no-commit feat");
    assert!(
        String::from_utf8_lossy(&merge.stdout).contains("stopped before committing"),
        "expected the no-commit message, got: {}",
        String::from_utf8_lossy(&merge.stdout)
    );
    let mid = run_libra_command(&["rev-parse", "HEAD"], p);
    assert_eq!(
        String::from_utf8_lossy(&mid.stdout).trim(),
        before_head,
        "--no-commit must not move HEAD"
    );
    assert!(
        p.join("feat.txt").exists(),
        "merged file should be staged into the worktree"
    );

    // merge --continue finalizes the two-parent commit and advances HEAD.
    let cont = run_libra_command(&["merge", "--continue"], p);
    assert_cli_success(&cont, "merge --continue");
    let after = run_libra_command(&["rev-parse", "HEAD"], p);
    assert_ne!(
        String::from_utf8_lossy(&after.stdout).trim(),
        before_head,
        "HEAD should advance after merge --continue"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_merge_same_file_non_overlapping_edits_merges_without_conflict() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    commit_file(
        temp_path,
        "tracked.txt",
        "line 1\nline 2\nline 3\nline 4\nline 5\n",
        "Prepare shared merge fixture",
    );

    let output = run_libra_command(&["branch", "feature"], temp_path);
    assert_cli_success(&output, "create feature");

    let output = run_libra_command(&["checkout", "feature"], temp_path);
    assert_cli_success(&output, "checkout feature");

    commit_file(
        temp_path,
        "tracked.txt",
        "line 1\nline 2\nline 3\nline 4\nline 5 from feature\n",
        "Edit last line on feature",
    );

    let output = run_libra_command(&["checkout", "main"], temp_path);
    assert_cli_success(&output, "checkout main");

    commit_file(
        temp_path,
        "tracked.txt",
        "line 1 from main\nline 2\nline 3\nline 4\nline 5\n",
        "Edit first line on main",
    );

    let merge_output = run_libra_command(&["merge", "feature"], temp_path);
    assert_cli_success(&merge_output, "non-overlapping same-file merge");

    let merged = std::fs::read_to_string(temp_path.join("tracked.txt")).expect("read merged file");
    assert_eq!(
        merged, "line 1 from main\nline 2\nline 3\nline 4\nline 5 from feature\n",
        "non-overlapping same-file edits should merge without conflict markers"
    );
    assert!(
        !merged.contains("<<<<<<<") && !merged.contains("=======") && !merged.contains(">>>>>>>"),
        "clean same-file merge must not leave conflict markers: {merged}"
    );
    assert!(
        !temp_path.join(".libra").join("merge-state.json").exists(),
        "clean same-file merge must not leave merge state"
    );

    let _guard = ChangeDirGuard::new(temp_path);
    let head = Head::current_commit()
        .await
        .expect("merge should create HEAD");
    let commit: Commit = load_object(&head).expect("load merge commit");
    assert_eq!(
        commit.parent_commit_ids.len(),
        2,
        "clean same-file merge should create a two-parent commit"
    );
}

#[test]
fn test_merge_diverged_nested_directory_file_survives_three_way() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(
        temp_path,
        "nested/feature.txt",
        "feature nested\n",
        "feature nested",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "main.txt", "main\n", "main change");

    let output = run_libra_command(&["merge", "feature"], temp_path);
    assert_cli_success(&output, "nested three-way merge");
    assert_eq!(
        std::fs::read_to_string(temp_path.join("nested").join("feature.txt"))
            .expect("read nested feature file"),
        "feature nested\n"
    );
}

#[test]
/// Test JSON envelope for a clean three-way merge.
fn test_merge_json_diverged_branch_outputs_three_way_summary() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    let output = run_libra_command(&["branch", "branch1"], temp_path);
    assert!(output.status.success(), "Failed to create branch1");

    let output = run_libra_command(&["checkout", "branch1"], temp_path);
    assert!(output.status.success(), "Failed to checkout branch1");

    commit_file(
        temp_path,
        "branch1.txt",
        "Branch1 content",
        "Add branch1 content",
    );

    let output = run_libra_command(&["checkout", "main"], temp_path);
    assert!(output.status.success(), "Failed to checkout main");

    let output = run_libra_command(&["branch", "branch2"], temp_path);
    assert!(output.status.success(), "Failed to create branch2");

    let output = run_libra_command(&["checkout", "branch2"], temp_path);
    assert!(output.status.success(), "Failed to checkout branch2");

    commit_file(
        temp_path,
        "branch2.txt",
        "Branch2 content",
        "Add branch2 content",
    );

    let output = run_libra_command(&["checkout", "branch1"], temp_path);
    assert!(output.status.success(), "Failed to checkout branch1");

    let merge_output = run_libra_command(&["--json", "merge", "branch2"], temp_path);
    assert_cli_success(&merge_output, "json three-way merge");
    assert!(merge_output.stderr.is_empty());
    let json = parse_json_stdout(&merge_output);
    assert_eq!(json["command"], "merge");
    assert_eq!(json["data"]["strategy"], "three-way");
    assert_eq!(json["data"]["up_to_date"], false);
    assert_eq!(
        json["data"]["parents"].as_array().expect("parents").len(),
        2
    );
    assert!(
        json["data"]["commit"].as_str().is_some(),
        "json should report the merge commit: {json}"
    );
}

#[test]
fn test_merge_conflict_writes_markers_status_hints_and_abort_restores() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(
        temp_path,
        "tracked.txt",
        "feature change\n",
        "feature change",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "tracked.txt", "main change\n", "main change");

    let output = run_libra_command(&["merge", "feature"], temp_path);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(stderr.contains("merge has conflicts in tracked.txt"));
    assert!(
        report
            .hints
            .iter()
            .any(|hint| hint.contains("libra merge --continue")),
        "conflict error should hint continue: {:?}",
        report.hints
    );

    let conflicted = std::fs::read_to_string(temp_path.join("tracked.txt")).expect("read conflict");
    assert!(conflicted.contains("<<<<<<< HEAD"), "{conflicted}");
    assert!(conflicted.contains("======="), "{conflicted}");
    assert!(conflicted.contains(">>>>>>>"), "{conflicted}");

    let status = run_libra_command(&["status"], temp_path);
    assert_cli_success(&status, "status during merge");
    let status_stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_stdout.contains("You are in the middle of a merge with 'feature'."),
        "status should mention merge state, stdout: {status_stdout}"
    );
    assert!(status_stdout.contains("libra merge --continue"));
    assert!(status_stdout.contains("libra merge --abort"));

    let abort = run_libra_command(&["merge", "--abort"], temp_path);
    assert_cli_success(&abort, "merge abort");
    assert_eq!(
        std::fs::read_to_string(temp_path.join("tracked.txt")).expect("read restored file"),
        "main change\n"
    );
    assert!(
        !temp_path.join(".libra").join("merge-state.json").exists(),
        "abort should remove merge state"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_merge_continue_after_resolving_conflict_creates_two_parent_commit() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(
        temp_path,
        "tracked.txt",
        "feature change\n",
        "feature change",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "tracked.txt", "main change\n", "main change");

    let output = run_libra_command(&["merge", "feature"], temp_path);
    assert_eq!(output.status.code(), Some(128));

    std::fs::write(temp_path.join("tracked.txt"), "resolved change\n").expect("write resolution");
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], temp_path),
        "stage resolution",
    );
    let status = run_libra_command(&["status"], temp_path);
    assert_cli_success(&status, "status after staged resolution");
    let status_stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_stdout.contains("all conflicts fixed"),
        "status should acknowledge staged conflict resolution, stdout: {status_stdout}"
    );
    let continued = run_libra_command(&["merge", "--continue"], temp_path);
    assert_cli_success(&continued, "merge continue");
    let stdout = String::from_utf8_lossy(&continued.stdout);
    assert!(stdout.contains("Merge completed."), "stdout: {stdout}");

    let _guard = ChangeDirGuard::new(temp_path);
    let head = Head::current_commit()
        .await
        .expect("merge continue should create HEAD");
    let commit: Commit = load_object(&head).expect("load continued merge commit");
    assert_eq!(commit.parent_commit_ids.len(), 2);
    assert!(
        commit.message.starts_with('\n'),
        "merge --continue commit body must retain Git's blank-line separator before the message"
    );
    assert_eq!(
        std::fs::read_to_string(temp_path.join("tracked.txt")).expect("read resolved file"),
        "resolved change\n"
    );
    assert!(!temp_path.join(".libra").join("merge-state.json").exists());
}

#[test]
fn test_merge_continue_refuses_unstaged_resolution_edits() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(
        temp_path,
        "tracked.txt",
        "feature change\n",
        "feature change",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "tracked.txt", "main change\n", "main change");

    let output = run_libra_command(&["merge", "feature"], temp_path);
    assert_eq!(output.status.code(), Some(128));

    std::fs::write(temp_path.join("tracked.txt"), "staged resolution\n").expect("write resolution");
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], temp_path),
        "stage resolution",
    );
    std::fs::write(temp_path.join("tracked.txt"), "unstaged follow-up\n")
        .expect("write unstaged follow-up");

    let continued = run_libra_command(&["merge", "--continue"], temp_path);
    let (_stderr, report) = parse_cli_error_stderr(&continued.stderr);
    assert_eq!(continued.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(report.message.contains("uncommitted changes"));
    assert_eq!(
        std::fs::read_to_string(temp_path.join("tracked.txt")).expect("read follow-up"),
        "unstaged follow-up\n"
    );
}

#[test]
fn test_merge_dirty_worktree_refuses_before_state() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(temp_path, "feature.txt", "feature\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "main.txt", "main\n", "main change");
    std::fs::write(temp_path.join("tracked.txt"), "dirty\n").expect("write dirty file");

    let output = run_libra_command(&["merge", "feature"], temp_path);
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(report.message.contains("uncommitted changes"));
    assert!(
        !temp_path.join(".libra").join("merge-state.json").exists(),
        "dirty refusal should not create merge state"
    );
}

#[test]
fn test_merge_untracked_overwrite_refuses_before_head_update() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(
        temp_path,
        "clobber.txt",
        "from feature\n",
        "feature clobber",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    std::fs::write(temp_path.join("clobber.txt"), "untracked local\n")
        .expect("write untracked clobber");

    let output = run_libra_command(&["merge", "feature"], temp_path);
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(
        report
            .message
            .contains("untracked working tree file would be overwritten"),
        "message: {}",
        report.message
    );
    assert_eq!(
        std::fs::read_to_string(temp_path.join("clobber.txt")).expect("read untracked file"),
        "untracked local\n"
    );
    assert!(!temp_path.join(".libra").join("merge-state.json").exists());
}

/// `libra merge --help` surfaces the EXAMPLES banner so users see the
/// supported fast-forward / remote-ref / JSON forms before hitting the
/// `MergeNonFastForward` runtime error. Cross-cutting `--help` EXAMPLES
/// rollout per `docs/development/commands/_general.md` item B.
#[test]
fn test_merge_help_lists_examples_banner() {
    let repo = tempfile::tempdir().expect("tempdir for merge --help");
    let output = run_libra_command(&["merge", "--help"], repo.path());
    assert!(
        output.status.success(),
        "merge --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "merge --help should include EXAMPLES banner, stdout: {stdout}"
    );
    assert!(
        stdout.contains("NOTES:"),
        "merge --help should call out the non-fast-forward limitation, stdout: {stdout}"
    );
    for invocation in [
        "libra merge feature-x",
        "libra merge origin/main",
        "libra merge --json",
    ] {
        assert!(
            stdout.contains(invocation),
            "merge --help should include `{invocation}`, stdout: {stdout}"
        );
    }
}

#[test]
fn test_merge_no_edit_accepts_default_message() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(temp_path, "feature.txt", "feature\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "main.txt", "main\n", "main change");

    // `--no-edit` accepts the auto-generated merge message without an editor
    // (Libra never opens one, so this behaves like a plain three-way merge).
    let output = run_libra_command(&["merge", "feature", "--no-edit"], temp_path);
    assert_cli_success(&output, "merge feature --no-edit");
    let log = run_libra_command(&["log", "--oneline", "-n", "1"], temp_path);
    assert!(
        String::from_utf8_lossy(&log.stdout).contains("Merge feature into main"),
        "merge commit landed with the default message: {:?}",
        String::from_utf8_lossy(&log.stdout)
    );
}

#[test]
fn test_merge_no_stat_short_n_and_long_are_accepted() {
    // `-n`/`--no-stat` suppress Git's post-merge diffstat. Libra's merge never
    // prints a diffstat, so both are accepted no-ops that produce a normal merge.
    for flag in ["-n", "--no-stat"] {
        let temp_repo = create_committed_repo_via_cli();
        let temp_path = temp_repo.path();
        assert_cli_success(
            &run_libra_command(&["branch", "feature"], temp_path),
            "create feature",
        );
        assert_cli_success(
            &run_libra_command(&["checkout", "feature"], temp_path),
            "checkout feature",
        );
        commit_file(temp_path, "feature.txt", "feature\n", "feature change");
        assert_cli_success(
            &run_libra_command(&["checkout", "main"], temp_path),
            "checkout main",
        );
        commit_file(temp_path, "main.txt", "main\n", "main change");

        let output = run_libra_command(&["merge", "feature", flag], temp_path);
        assert_cli_success(&output, &format!("merge feature {flag}"));
        // No diffstat is printed (Libra never shows one); the merge still happens.
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains(" | ")
                && !stdout.contains("file changed")
                && !stdout.contains("files changed"),
            "merge {flag} prints no diffstat: {stdout}"
        );
        let log = run_libra_command(&["log", "--oneline", "-n", "1"], temp_path);
        assert!(
            String::from_utf8_lossy(&log.stdout)
                .to_lowercase()
                .contains("merge"),
            "merge {flag} created a merge commit"
        );
    }
}

#[test]
fn test_merge_no_progress_is_accepted_noop() {
    // `--no-progress` suppresses a progress meter. Libra's merge never renders
    // one, so it is an accepted no-op that produces a normal merge.
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(temp_path, "feature.txt", "feature\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "main.txt", "main\n", "main change");

    let output = run_libra_command(&["merge", "feature", "--no-progress"], temp_path);
    assert_cli_success(&output, "merge feature --no-progress");
    let log = run_libra_command(&["log", "--oneline", "-n", "1"], temp_path);
    assert!(
        String::from_utf8_lossy(&log.stdout)
            .to_lowercase()
            .contains("merge"),
        "merge --no-progress created a merge commit"
    );
}

#[test]
fn test_merge_no_verify_signatures_is_accepted_noop() {
    // `--no-verify-signatures` skips GPG signature verification of the merged
    // commits. Libra's merge never verifies signatures, so it is an accepted
    // no-op that produces a normal merge.
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(temp_path, "feature.txt", "feature\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "main.txt", "main\n", "main change");

    let output = run_libra_command(&["merge", "feature", "--no-verify-signatures"], temp_path);
    assert_cli_success(&output, "merge feature --no-verify-signatures");
    let log = run_libra_command(&["log", "--oneline", "-n", "1"], temp_path);
    assert!(
        String::from_utf8_lossy(&log.stdout)
            .to_lowercase()
            .contains("merge"),
        "merge --no-verify-signatures created a merge commit"
    );
}

#[test]
fn test_merge_no_rerere_autoupdate_is_accepted_noop() {
    // `--no-rerere-autoupdate` skips updating the rerere index. Libra has no
    // rerere, so it is an accepted no-op that produces a normal merge.
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(temp_path, "feature.txt", "feature\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "main.txt", "main\n", "main change");

    let output = run_libra_command(&["merge", "feature", "--no-rerere-autoupdate"], temp_path);
    assert_cli_success(&output, "merge feature --no-rerere-autoupdate");
    let log = run_libra_command(&["log", "--oneline", "-n", "1"], temp_path);
    assert!(
        String::from_utf8_lossy(&log.stdout)
            .to_lowercase()
            .contains("merge"),
        "merge --no-rerere-autoupdate created a merge commit"
    );
}

#[test]
fn test_merge_no_gpg_sign_is_accepted_noop() {
    // `--no-gpg-sign` skips signing the merge commit. Libra's merge never signs,
    // so it is an accepted no-op that produces a normal merge.
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(temp_path, "feature.txt", "feature\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "main.txt", "main\n", "main change");

    let output = run_libra_command(&["merge", "feature", "--no-gpg-sign"], temp_path);
    assert_cli_success(&output, "merge feature --no-gpg-sign");
    let log = run_libra_command(&["log", "--oneline", "-n", "1"], temp_path);
    assert!(
        String::from_utf8_lossy(&log.stdout)
            .to_lowercase()
            .contains("merge"),
        "merge --no-gpg-sign created a merge commit"
    );
}

#[test]
fn test_merge_stat_prints_diffstat_for_three_way() {
    // `--stat` prints a diffstat of what the merge brought in. Three-way setup:
    // feature.txt on `feature`, main.txt on `main`, so merging `feature` adds
    // feature.txt relative to the pre-merge main tip.
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "branch feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "checkout feature",
    );
    commit_file(p, "feature.txt", "feature line\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main",
    );
    commit_file(p, "main.txt", "main line\n", "main change");

    let out = run_libra_command(&["merge", "--stat", "feature"], p);
    assert_cli_success(&out, "merge --stat feature");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("feature.txt"),
        "diffstat must name the merged-in file: {stdout}"
    );
    assert!(
        stdout.contains(" | "),
        "diffstat must have a per-file bar line: {stdout}"
    );
    assert!(
        stdout.contains("file changed") || stdout.contains("files changed"),
        "diffstat must have a summary line: {stdout}"
    );
}

#[test]
fn test_merge_stat_prints_diffstat_for_fast_forward() {
    // Fast-forward: `main` is strictly behind `feature`, so merging fast-forwards
    // and `--stat` reports the files feature added.
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "branch feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "checkout feature",
    );
    commit_file(p, "ff.txt", "ff line\n", "ff change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main",
    );

    let out = run_libra_command(&["merge", "--stat", "feature"], p);
    assert_cli_success(&out, "merge --stat feature (ff)");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Fast-forward"),
        "expected a fast-forward: {stdout}"
    );
    assert!(
        stdout.contains("ff.txt") && stdout.contains(" | "),
        "fast-forward --stat must print the diffstat: {stdout}"
    );
}

#[test]
fn test_merge_stat_no_stat_toggle_last_wins() {
    // `--stat`/`--no-stat` is a last-one-wins toggle.
    let make = || -> tempfile::TempDir {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        assert_cli_success(
            &run_libra_command(&["branch", "feature"], p),
            "branch feature",
        );
        assert_cli_success(
            &run_libra_command(&["checkout", "feature"], p),
            "checkout feature",
        );
        commit_file(p, "feature.txt", "feature line\n", "feature change");
        assert_cli_success(
            &run_libra_command(&["checkout", "main"], p),
            "checkout main",
        );
        commit_file(p, "main.txt", "main line\n", "main change");
        repo
    };

    // `--no-stat --stat` → stat wins → diffstat printed.
    let repo = make();
    let out = run_libra_command(&["merge", "--no-stat", "--stat", "feature"], repo.path());
    assert_cli_success(&out, "merge --no-stat --stat");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("file changed")
            || String::from_utf8_lossy(&out.stdout).contains("files changed"),
        "last --stat wins → diffstat printed"
    );

    // `--stat --no-stat` → no-stat wins → no diffstat.
    let repo = make();
    let out = run_libra_command(&["merge", "--stat", "--no-stat", "feature"], repo.path());
    assert_cli_success(&out, "merge --stat --no-stat");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains(" | ") && !stdout.contains("file changed"),
        "last --no-stat wins → no diffstat: {stdout}"
    );
}

#[test]
fn test_merge_stat_suppressed_in_json_machine_and_quiet_modes() {
    // `--stat` must never corrupt structured (`--json`/`--machine`) output or
    // break `--quiet` silence: the diffstat is human-only.
    let setup = || -> tempfile::TempDir {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        assert_cli_success(
            &run_libra_command(&["branch", "feature"], p),
            "branch feature",
        );
        assert_cli_success(
            &run_libra_command(&["checkout", "feature"], p),
            "checkout feature",
        );
        commit_file(p, "feature.txt", "feature line\n", "feature change");
        assert_cli_success(
            &run_libra_command(&["checkout", "main"], p),
            "checkout main",
        );
        commit_file(p, "main.txt", "main line\n", "main change");
        repo
    };
    let no_stat_text =
        |s: &str| !s.contains(" | ") && !s.contains("file changed") && !s.contains("files changed");

    // `--json --stat`: stdout is a single parseable JSON envelope, no diffstat text.
    let repo = setup();
    let out = run_libra_command(&["--json", "merge", "--stat", "feature"], repo.path());
    assert_cli_success(&out, "--json merge --stat");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("--json stdout must be a single JSON record");
    assert_eq!(json["command"], "merge");
    assert!(
        no_stat_text(&stdout),
        "no diffstat text in JSON stdout: {stdout}"
    );

    // `--machine --stat`: NDJSON stays clean (machine implies json + quiet).
    let repo = setup();
    let out = run_libra_command(&["--machine", "merge", "--stat", "feature"], repo.path());
    assert_cli_success(&out, "--machine merge --stat");
    assert!(
        no_stat_text(&String::from_utf8_lossy(&out.stdout)),
        "no diffstat text in machine stdout"
    );

    // `--quiet --stat`: stdout stays empty.
    let repo = setup();
    let out = run_libra_command(&["--quiet", "merge", "--stat", "feature"], repo.path());
    assert_cli_success(&out, "--quiet merge --stat");
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "quiet must suppress the diffstat"
    );
}

#[test]
fn test_merge_verify_signatures_accepts_signed_rejects_unsigned() {
    // `merge --verify-signatures` validates the merged tip's PGP signature
    // against the local vault key, aborting if it is unsigned (or invalid).
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // dev: a branch whose tip is a SIGNED commit (vault PGP signing on; `libra
    // init` already provisioned the vault key, so enabling the config is enough).
    assert_cli_success(
        &run_libra_command(&["config", "vault.signing", "true"], p),
        "enable vault signing",
    );
    assert_cli_success(&run_libra_command(&["branch", "dev"], p), "branch dev");
    assert_cli_success(&run_libra_command(&["checkout", "dev"], p), "checkout dev");
    std::fs::write(p.join("dev.txt"), "dev\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "dev.txt"], p), "add dev.txt");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "dev-signed", "--no-verify"], p),
        "signed dev commit",
    );

    // dev2: a branch (from the original base) whose tip is UNSIGNED.
    assert_cli_success(
        &run_libra_command(&["config", "vault.signing", "false"], p),
        "disable vault signing",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main",
    );
    assert_cli_success(&run_libra_command(&["branch", "dev2"], p), "branch dev2");
    assert_cli_success(
        &run_libra_command(&["checkout", "dev2"], p),
        "checkout dev2",
    );
    std::fs::write(p.join("dev2.txt"), "dev2\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "dev2.txt"], p), "add dev2.txt");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "dev2-unsigned", "--no-verify"], p),
        "unsigned dev2 commit",
    );

    // Signed tip → merge --verify-signatures succeeds (proves the signed-content
    // reconstruction round-trips against the vault key).
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main again",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--verify-signatures", "dev"], p),
        "merge of a signed tip",
    );

    // Unsigned tip → aborts before merging.
    let bad = run_libra_command(&["merge", "--verify-signatures", "dev2"], p);
    assert!(
        !bad.status.success(),
        "merge of an unsigned tip must abort: {}",
        String::from_utf8_lossy(&bad.stdout)
    );
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("does not have a GPG signature"),
        "unsigned-merge error should name the missing signature: {}",
        String::from_utf8_lossy(&bad.stderr)
    );

    // Without verification, the unsigned tip merges fine.
    assert_cli_success(
        &run_libra_command(&["merge", "--no-verify-signatures", "dev2"], p),
        "unsigned tip merges without verification",
    );

    // A signed commit whose message starts with whitespace (preserved via
    // --cleanup=verbatim) must still verify: the signed-content reconstruction
    // takes the message verbatim, not trimmed.
    assert_cli_success(
        &run_libra_command(&["config", "vault.signing", "true"], p),
        "re-enable vault signing",
    );
    assert_cli_success(&run_libra_command(&["branch", "dev3"], p), "branch dev3");
    assert_cli_success(
        &run_libra_command(&["checkout", "dev3"], p),
        "checkout dev3",
    );
    std::fs::write(p.join("dev3.txt"), "dev3\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "dev3.txt"], p), "add dev3.txt");
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--cleanup=verbatim",
                "-m",
                "  leading-space subject",
                "--no-verify",
            ],
            p,
        ),
        "signed commit with leading-whitespace message",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main for dev3",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--verify-signatures", "dev3"], p),
        "signed leading-whitespace-message tip verifies (message taken verbatim)",
    );

    // A signed message whose body itself contains the signature END-marker text
    // must still verify: the body is located by the signature block's offset, not
    // by searching for the marker (which would mis-select the body copy).
    assert_cli_success(&run_libra_command(&["branch", "dev4"], p), "branch dev4");
    assert_cli_success(
        &run_libra_command(&["checkout", "dev4"], p),
        "checkout dev4",
    );
    std::fs::write(p.join("dev4.txt"), "dev4\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "dev4.txt"], p), "add dev4.txt");
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--cleanup=verbatim",
                "-m",
                "body mentions -----END PGP SIGNATURE----- inline",
                "--no-verify",
            ],
            p,
        ),
        "signed commit whose body contains the END marker text",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main for dev4",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--verify-signatures", "dev4"], p),
        "signed tip whose message contains the END marker still verifies",
    );
}

/// A three-way `merge` conflict on one line of a multi-line file produces
/// LINE-LEVEL markers (matching Git): shared context lines stay OUTSIDE the
/// `<<<<<<< / ======= / >>>>>>>` region. Fails under the old whole-file
/// presentation (which enclosed every line of each side).
#[test]
fn test_merge_conflict_is_line_level() {
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();

    commit_file(p, "shared.txt", "top\nl1\nl2\nl3\nbottom\n", "base shared");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(
        p,
        "shared.txt",
        "top\nl1\nFEATURE\nl3\nbottom\n",
        "feature edit",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    commit_file(p, "shared.txt", "top\nl1\nMAIN\nl3\nbottom\n", "main edit");

    let out = run_libra_command(&["merge", "feature"], p);
    assert_eq!(out.status.code(), Some(128), "merge conflict exits 128");
    let body = std::fs::read_to_string(p.join("shared.txt")).expect("read conflict");

    assert!(
        body.starts_with("top\nl1\n<<<<<<< HEAD\n"),
        "shared prefix precedes the markers: {body:?}"
    );
    assert!(
        body.ends_with("l3\nbottom\n"),
        "shared suffix follows the markers: {body:?}"
    );
    let ours = body
        .split_once("<<<<<<< HEAD\n")
        .and_then(|(_, rest)| rest.split_once("\n======="))
        .map(|(mid, _)| mid)
        .expect("conflict region present");
    assert_eq!(
        ours, "MAIN",
        "ours hunk is just the diverging line: {body:?}"
    );
    assert!(
        body.contains("\nFEATURE\n"),
        "theirs hunk present: {body:?}"
    );
    assert!(
        !ours.contains("top") && !ours.contains("bottom"),
        "shared lines must not be inside the conflict region: {body:?}"
    );
}

/// Build a one-line both-modified conflict repo (`shared.txt`) with `feature`
/// diverging from `main`, without running the merge yet.
fn create_diverged_repo_for_conflict() -> tempfile::TempDir {
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    commit_file(
        p,
        "shared.txt",
        "top\nl1\nORIG\nl3\nbottom\n",
        "base shared",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(
        p,
        "shared.txt",
        "top\nl1\nFEATURE\nl3\nbottom\n",
        "feature edit",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    commit_file(p, "shared.txt", "top\nl1\nMAIN\nl3\nbottom\n", "main edit");
    temp_repo
}

/// `merge.conflictStyle = diff3` adds the `||||||| base` block with the
/// common-ancestor content between ours and the `=======` separator
/// (lore.md §1.3); the default two-marker style stays unchanged when unset.
#[test]
fn test_merge_conflict_diff3_markers() {
    let temp_repo = create_diverged_repo_for_conflict();
    let p = temp_repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "merge.conflictStyle", "diff3"], p),
        "set conflictStyle",
    );

    let out = run_libra_command(&["merge", "feature"], p);
    assert_eq!(out.status.code(), Some(128), "merge conflict exits 128");
    let body = std::fs::read_to_string(p.join("shared.txt")).expect("read conflict");
    assert!(
        body.contains("<<<<<<< HEAD\nMAIN\n||||||| base\nORIG\n=======\nFEATURE\n"),
        "diff3 emits the base block between ours and the separator: {body:?}"
    );
}

/// An unsupported `merge.conflictStyle` (e.g. the unimplemented `zdiff3`) is a
/// hard error when a conflict must be rendered — never a silent fall-back to
/// the default marker format — and nothing is written (no merge state).
#[test]
fn test_merge_conflict_style_invalid_rejected() {
    let temp_repo = create_diverged_repo_for_conflict();
    let p = temp_repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "merge.conflictStyle", "zdiff3"], p),
        "set conflictStyle",
    );

    let out = run_libra_command(&["merge", "feature"], p);
    assert_eq!(out.status.code(), Some(128), "invalid style is fatal");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unsupported merge.conflictStyle 'zdiff3'"),
        "actionable error names the bad value: {stderr}"
    );
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "no merge state is left behind when the style is rejected"
    );
    let body = std::fs::read_to_string(p.join("shared.txt")).expect("read file");
    assert!(
        !body.contains("<<<<<<<"),
        "no conflict markers were written: {body:?}"
    );
}

// ---------------------------------------------------------------------------
// `merge --dry-run` (Libra extension, lore.md §1.3): preview the outcome
// writing NOTHING — no HEAD/index/worktree/merge-state/object-store mutation.
// Exit 0 for ff/up-to-date/clean; exit 1 when the merge would conflict.
// ---------------------------------------------------------------------------

/// HEAD commit hash via `--json log -n1`-free plumbing: read `.libra` HEAD via
/// `rev-parse`-equivalent CLI (`libra rev-parse HEAD`).
fn head_commit(p: &Path) -> String {
    let out = run_libra_command(&["rev-parse", "HEAD"], p);
    assert_cli_success(&out, "rev-parse HEAD");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Count every file under `.libra/objects` (loose objects), recursively.
fn count_loose_objects(p: &Path) -> usize {
    fn walk(dir: &Path, total: &mut usize) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, total);
                } else {
                    *total += 1;
                }
            }
        }
    }
    let mut total = 0;
    walk(&p.join(".libra").join("objects"), &mut total);
    total
}

#[test]
fn test_merge_dry_run_fast_forward_writes_nothing() {
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "co feat");
    commit_file(p, "file.txt", "feature content\n", "feature edit");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");

    let head_before = head_commit(p);
    let operations_before = operation_count(p);
    let out = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_cli_success(&out, "dry-run ff");
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["strategy"], "fast-forward");
    assert_eq!(json["data"]["dry_run"], true);
    assert!(json["data"].get("would_conflict").is_none());
    // Nothing was written: HEAD unchanged, worktree file absent, no state.
    assert_eq!(head_commit(p), head_before, "HEAD must not move");
    assert!(
        !p.join("file.txt").exists(),
        "worktree must not receive the feature file"
    );
    assert!(!p.join(".libra").join("merge-state.json").exists());
    // And no OPERATION row (§C.9): the sequencer boundary persists one before
    // the handler runs, so mapping a dry run to a control action would write to
    // the operation log for a command documented to write nothing.
    assert_eq!(
        operation_count(p),
        operations_before,
        "a dry run must not record an operation"
    );
}

/// How many operations the log holds — the assertion a dry run needs, since
/// the control boundary writes its row before the handler is reached.
fn operation_count(repo: &Path) -> u64 {
    let out = run_libra_command(&["--json", "op", "log", "-n", "100"], repo);
    assert_cli_success(&out, "op log");
    parse_json_stdout(&out)["data"]["total"]
        .as_u64()
        .expect("op log total")
}

#[test]
fn test_merge_dry_run_already_up_to_date() {
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    let out = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_cli_success(&out, "dry-run up-to-date");
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["up_to_date"], true);
    assert_eq!(json["data"]["dry_run"], true);
}

#[test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
fn test_merge_dry_run_clean_three_way_writes_no_objects() {
    // Divergent but non-overlapping edits: a clean three-way preview. The
    // auto-merged blob must be computed in memory only — the object store,
    // HEAD, index, worktree, and merge state all stay untouched.
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    commit_file(p, "shared.txt", "top\nl1\nl2\nl3\nbottom\n", "base shared");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "co feat");
    commit_file(
        p,
        "shared.txt",
        "top\nFEATURE\nl2\nl3\nbottom\n",
        "feature edit",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    commit_file(p, "shared.txt", "top\nl1\nl2\nMAIN\nbottom\n", "main edit");

    let head_before = head_commit(p);
    let objects_before = count_loose_objects(p);
    let out = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_cli_success(&out, "dry-run clean three-way");
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["strategy"], "three-way");
    assert_eq!(json["data"]["dry_run"], true);
    assert!(json["data"]["commit"].is_null(), "no merge commit created");
    assert!(json["data"].get("would_conflict").is_none());

    assert_eq!(head_commit(p), head_before, "HEAD must not move");
    assert_eq!(
        count_loose_objects(p),
        objects_before,
        "a dry-run must not write objects (auto-merged blobs stay in memory)"
    );
    assert!(!p.join(".libra").join("merge-state.json").exists());
    assert_eq!(
        std::fs::read_to_string(p.join("shared.txt")).unwrap(),
        "top\nl1\nl2\nMAIN\nbottom\n",
        "worktree untouched"
    );
}

#[test]
fn test_merge_dry_run_conflict_exits_1_and_writes_nothing() {
    let temp_repo = create_diverged_repo_for_conflict();
    let p = temp_repo.path();
    let head_before = head_commit(p);

    let out = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_eq!(
        out.status.code(),
        Some(1),
        "would-conflict preview exits 1 (an outcome signal, not the 128 of a real conflict)"
    );
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["dry_run"], true);
    assert_eq!(json["data"]["would_conflict"], true);
    assert!(
        json["data"]["conflicted_paths"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some("shared.txt"))),
        "conflicted_paths names the path: {json}"
    );

    assert_eq!(head_commit(p), head_before, "HEAD must not move");
    assert!(!p.join(".libra").join("merge-state.json").exists());
    let body = std::fs::read_to_string(p.join("shared.txt")).unwrap();
    assert!(
        !body.contains("<<<<<<<"),
        "no conflict markers written by a preview: {body:?}"
    );
}

#[test]
fn test_merge_json_schema_freeze_no_dry_run_fields_on_real_merge() {
    // A REAL merge's JSON must not grow the dry_run/would_conflict keys.
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    let out = run_libra_command(&["--json", "merge", "feature"], p);
    assert_cli_success(&out, "real merge");
    let json = parse_json_stdout(&out);
    assert!(json["data"].get("dry_run").is_none());
    assert!(json["data"].get("would_conflict").is_none());
}

#[test]
fn test_merge_dry_run_clap_exclusions() {
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    for argv in [
        &["merge", "--dry-run", "--continue"][..],
        &["merge", "--dry-run", "--abort"][..],
        &["merge", "--dry-run", "--squash", "feature"][..],
        &["merge", "--restart", "feature"][..],
        &["merge", "--restart", "--no-ff"][..],
        &["merge", "--restart", "--dry-run"][..],
    ] {
        let out = run_libra_command(argv, p);
        assert_eq!(
            out.status.code(),
            Some(129),
            "clap must reject {argv:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

// ---------------------------------------------------------------------------
// `merge --restart` (Libra extension, lore.md §1.3): abort the in-progress
// conflicted merge (discarding resolution work, exactly like --abort) and
// re-run the SAME merge against the recorded target commit.
// ---------------------------------------------------------------------------

#[test]
fn test_merge_restart_regenerates_fresh_conflict() {
    let temp_repo = create_diverged_repo_for_conflict();
    let p = temp_repo.path();
    let head_before = head_commit(p);
    let out = run_libra_command(&["merge", "feature"], p);
    assert_eq!(out.status.code(), Some(128), "initial conflict");

    // Simulate partial resolution work that --restart must DISCARD.
    std::fs::write(p.join("shared.txt"), "half-resolved\n").unwrap();

    let out = run_libra_command(&["merge", "--restart"], p);
    assert_eq!(
        out.status.code(),
        Some(128),
        "the re-run reproduces the conflict (normal merge exit): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = std::fs::read_to_string(p.join("shared.txt")).unwrap();
    assert!(
        body.contains("<<<<<<< HEAD") && !body.contains("half-resolved"),
        "fresh markers regenerated, user edits discarded: {body:?}"
    );
    assert!(
        p.join(".libra").join("merge-state.json").exists(),
        "a fresh merge state exists after restart"
    );
    assert_eq!(head_commit(p), head_before, "HEAD is back at orig_head");
    // The restarted merge is resumable exactly like a normal conflicted merge.
    assert_cli_success(&run_libra_command(&["merge", "--abort"], p), "abort");
}

#[test]
fn test_merge_restart_without_merge_in_progress_errors() {
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    let out = run_libra_command(&["merge", "--restart"], p);
    assert_eq!(out.status.code(), Some(128), "no merge in progress");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no merge in progress"),
        "actionable error: {stderr}"
    );
}

#[test]
fn test_merge_restart_refuses_staged_no_commit_merge() {
    // `--no-commit` persists MergeState with NO conflicts; --restart must
    // refuse (it would discard the staged result and could fast-forward),
    // leaving the staged merge fully intact.
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    commit_file(p, "shared.txt", "top\nl1\nl2\nl3\nbottom\n", "base shared");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "co feat");
    commit_file(
        p,
        "shared.txt",
        "top\nFEATURE\nl2\nl3\nbottom\n",
        "feature edit",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    commit_file(p, "shared.txt", "top\nl1\nl2\nMAIN\nbottom\n", "main edit");

    assert_cli_success(
        &run_libra_command(&["merge", "--no-commit", "feature"], p),
        "clean --no-commit merge",
    );
    assert!(p.join(".libra").join("merge-state.json").exists());
    let head_before = head_commit(p);

    let out = run_libra_command(&["merge", "--restart"], p);
    assert_eq!(out.status.code(), Some(128), "restart refused");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no conflicted merge to restart"),
        "actionable refusal: {stderr}"
    );
    // The staged no-commit merge is untouched and still finishable.
    assert_eq!(head_commit(p), head_before, "HEAD untouched");
    assert!(
        p.join(".libra").join("merge-state.json").exists(),
        "staged merge state preserved"
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--continue"], p),
        "staged merge still finishable",
    );
}

// ── merge --autostash (lore.md §1.8) ────────────────────────────────────────

/// Diverged repo WITHOUT conflicts: feature edits its own file.
fn create_diverged_repo_clean() -> tempfile::TempDir {
    let temp_repo = create_committed_repo_via_cli();
    let p = temp_repo.path();
    commit_file(p, "base.txt", "base\n", "base");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(p, "feature.txt", "feature\n", "feature edit");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    commit_file(p, "main.txt", "main\n", "main edit");
    temp_repo
}

fn stash_list_len(p: &Path) -> usize {
    let out = run_libra_command(&["stash", "list"], p);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count()
}

#[test]
fn test_merge_autostash_clean_merge_reapplies() {
    let temp_repo = create_diverged_repo_clean();
    let p = temp_repo.path();
    // Clean tree: strict no-op (no stash, normal merge).
    let out = run_libra_command(&["--json", "merge", "feature", "--autostash"], p);
    assert_cli_success(&out, "clean-tree autostash merge");
    let json = parse_json_stdout(&out);
    assert!(
        json["data"].get("autostash").is_none(),
        "clean tree adds no autostash marker: {json}"
    );
    // Re-merge with a dirty tree in a fresh repo.
    let temp_repo = create_diverged_repo_clean();
    let p = temp_repo.path();
    std::fs::write(p.join("base.txt"), "dirty edit\n").unwrap();
    let out = run_libra_command(&["--json", "merge", "feature", "--autostash"], p);
    assert_cli_success(&out, "dirty autostash merge");
    let json = parse_json_stdout(&out);
    assert_eq!(
        json["data"]["autostash"].as_str(),
        Some("applied"),
        "{json}"
    );
    // The dirty edit is back, and the stash list is empty (never entered).
    assert_eq!(
        std::fs::read_to_string(p.join("base.txt")).unwrap(),
        "dirty edit\n"
    );
    assert_eq!(stash_list_len(p), 0, "autostash never enters stash list");
    // The merge result is present too.
    assert!(p.join("feature.txt").exists());
}

#[test]
fn test_merge_autostash_restores_staged_and_worktree_layers() {
    let temp_repo = create_diverged_repo_clean();
    let p = temp_repo.path();
    std::fs::write(p.join("base.txt"), "staged only\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "base.txt"], p), "stage edit");
    std::fs::write(p.join("base.txt"), "base\n").unwrap();

    let out = run_libra_command(&["merge", "feature", "--autostash"], p);
    assert_cli_success(&out, "layered autostash merge");
    assert_eq!(
        std::fs::read_to_string(p.join("base.txt")).unwrap(),
        "base\n"
    );

    let staged = run_libra_command(&["ls-files", "--stage", "base.txt"], p);
    assert_cli_success(&staged, "inspect restored staged entry");
    let staged = String::from_utf8(staged.stdout).unwrap();
    let staged_oid = staged
        .split_whitespace()
        .nth(1)
        .expect("stage row has object id");
    let blob = run_libra_command(&["cat-file", "-p", staged_oid], p);
    assert_cli_success(&blob, "read restored staged blob");
    assert_eq!(blob.stdout, b"staged only\n");
}

#[test]
fn test_merge_autostash_conflict_holds_then_abort_restores() {
    let temp_repo = create_diverged_repo_for_conflict();
    let p = temp_repo.path();
    std::fs::write(p.join("unrelated.txt"), "precious\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "unrelated.txt"], p), "add");
    let out = run_libra_command(&["merge", "feature", "--autostash"], p);
    assert_eq!(out.status.code(), Some(128), "conflict exits 128");
    // Held: dirty changes absent from the conflicted tree, stash list empty.
    assert!(
        !p.join("unrelated.txt").exists(),
        "held autostash removes the dirty file from the conflict worktree"
    );
    assert_eq!(stash_list_len(p), 0, "held autostash not in stash list");
    assert!(
        p.join(".libra/merge-autostash.json").exists(),
        "sidecar holds the stash"
    );
    assert_cli_success(
        &run_libra_command(&["maintenance", "run", "--task", "gc"], p),
        "gc preserves held merge autostash",
    );
    // --abort restores the pre-merge tree AND re-applies the autostash.
    let abort = run_libra_command(&["--json", "merge", "--abort"], p);
    assert_cli_success(&abort, "abort");
    let json = parse_json_stdout(&abort);
    assert_eq!(
        json["data"]["autostash"].as_str(),
        Some("applied"),
        "{json}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("unrelated.txt")).unwrap(),
        "precious\n"
    );
    assert!(!p.join(".libra/merge-autostash.json").exists());
}

#[test]
fn test_merge_autostash_conflict_resolve_continue_reapplies() {
    let temp_repo = create_diverged_repo_for_conflict();
    let p = temp_repo.path();
    std::fs::write(p.join("unrelated.txt"), "precious\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "unrelated.txt"], p), "add");
    let out = run_libra_command(&["merge", "feature", "--autostash"], p);
    assert_eq!(out.status.code(), Some(128));
    // Resolve and continue; the autostash comes back after the merge commit.
    std::fs::write(p.join("shared.txt"), "top\nl1\nRESOLVED\nl3\nbottom\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add");
    let cont = run_libra_command(&["--json", "merge", "--continue"], p);
    assert_cli_success(&cont, "continue");
    let json = parse_json_stdout(&cont);
    assert_eq!(
        json["data"]["autostash"].as_str(),
        Some("applied"),
        "{json}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("unrelated.txt")).unwrap(),
        "precious\n"
    );
    assert_eq!(stash_list_len(p), 0);
}

#[test]
fn test_merge_autostash_restart_preserves_held_stash() {
    let temp_repo = create_diverged_repo_for_conflict();
    let p = temp_repo.path();
    std::fs::write(p.join("unrelated.txt"), "precious\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "unrelated.txt"], p), "add");
    let out = run_libra_command(&["merge", "feature", "--autostash"], p);
    assert_eq!(out.status.code(), Some(128));
    // Restart re-conflicts; the held stash must survive (not demoted).
    let restart = run_libra_command(&["merge", "--restart"], p);
    assert_eq!(restart.status.code(), Some(128), "re-conflicts");
    assert_eq!(stash_list_len(p), 0, "held stash NOT demoted by restart");
    assert!(
        p.join(".libra/merge-autostash.json").exists(),
        "sidecar survives restart"
    );
    // Abort finally restores everything.
    assert_cli_success(&run_libra_command(&["merge", "--abort"], p), "abort");
    assert_eq!(
        std::fs::read_to_string(p.join("unrelated.txt")).unwrap(),
        "precious\n"
    );
}

#[test]
fn test_merge_autostash_start_failure_restores_immediately() {
    let temp_repo = create_diverged_repo_clean();
    let p = temp_repo.path();
    std::fs::write(p.join("base.txt"), "dirty edit\n").unwrap();
    // --ff-only on diverged branches is refused AFTER the stash was taken:
    // the dirty tree must be restored before the error propagates.
    let out = run_libra_command(&["merge", "feature", "--ff-only", "--autostash"], p);
    assert!(!out.status.success(), "ff-only diverged refused");
    assert_eq!(
        std::fs::read_to_string(p.join("base.txt")).unwrap(),
        "dirty edit\n",
        "start failure restores the dirty tree"
    );
    assert!(!p.join(".libra/merge-autostash.json").exists());
    assert_eq!(stash_list_len(p), 0);
}

#[test]
fn test_merge_autostash_config_and_validation() {
    let temp_repo = create_diverged_repo_clean();
    let p = temp_repo.path();
    // merge.autostash=true enables without the flag.
    assert_cli_success(
        &run_libra_command(&["config", "merge.autostash", "true"], p),
        "set config",
    );
    std::fs::write(p.join("base.txt"), "dirty edit\n").unwrap();
    let out = run_libra_command(&["--json", "merge", "feature"], p);
    assert_cli_success(&out, "config-enabled autostash");
    let json = parse_json_stdout(&out);
    assert_eq!(
        json["data"]["autostash"].as_str(),
        Some("applied"),
        "{json}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("base.txt")).unwrap(),
        "dirty edit\n"
    );
    // Invalid config value is a HARD error (not silently off).
    let temp_repo = create_diverged_repo_clean();
    let p = temp_repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "merge.autostash", "sometimes"], p),
        "set bad config",
    );
    std::fs::write(p.join("base.txt"), "dirty edit\n").unwrap();
    let out = run_libra_command(&["merge", "feature"], p);
    assert!(!out.status.success(), "invalid merge.autostash is fatal");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("merge.autostash"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // --no-autostash overrides the (invalid) config: merge refuses the dirty
    // tree via the normal guard instead (dirty worktree blocks a three-way).
    let out = run_libra_command(&["merge", "feature", "--no-autostash"], p);
    assert!(!out.status.success());
    assert!(
        std::fs::read_to_string(p.join("base.txt")).unwrap() == "dirty edit\n",
        "no-autostash leaves the tree alone"
    );
    // clap exclusions.
    let out = run_libra_command(&["merge", "--continue", "--autostash"], p);
    assert_eq!(out.status.code(), Some(129));
    let out = run_libra_command(&["merge", "feature", "--dry-run", "--autostash"], p);
    assert_eq!(out.status.code(), Some(129));
}

/// Regression: `Commit::from_tree_id` hardcodes `mega <admin@mega.org>` as both
/// author and committer. Every merge-commit path used it, so merge commits
/// silently discarded `user.name` / `user.email`. All three paths must now carry
/// the configured identity.
#[tokio::test]
#[serial(cwd)]
async fn test_merge_commit_carries_configured_identity() {
    for (label, extra_args) in [
        ("three-way", Vec::new()),
        ("no-ff", vec!["--no-ff"]),
        ("ours-strategy", vec!["-s", "ours"]),
    ] {
        let temp_repo = create_committed_repo_via_cli();
        let temp_path = temp_repo.path();

        assert_cli_success(
            &run_libra_command(&["branch", "feature"], temp_path),
            "create feature",
        );
        assert_cli_success(
            &run_libra_command(&["checkout", "feature"], temp_path),
            "checkout feature",
        );
        commit_file(temp_path, "feature.txt", "feature\n", "feature commit");
        assert_cli_success(
            &run_libra_command(&["checkout", "main"], temp_path),
            "checkout main",
        );
        commit_file(temp_path, "main.txt", "main\n", "main commit");

        let mut args = vec!["merge", "feature"];
        args.extend_from_slice(&extra_args);
        assert_cli_success(&run_libra_command(&args, temp_path), label);

        let _guard = ChangeDirGuard::new(temp_path);
        let head = Head::current_commit().await.expect("merge moved HEAD");
        let commit: Commit = load_object(&head).expect("load merge commit");
        assert_eq!(
            commit.parent_commit_ids.len(),
            2,
            "{label} should record two parents"
        );
        assert_eq!(
            (
                commit.author.name.as_str(),
                commit.author.email.as_str(),
                commit.committer.name.as_str(),
                commit.committer.email.as_str(),
            ),
            (
                "Test User",
                "test@example.com",
                "Test User",
                "test@example.com",
            ),
            "{label} merge commit must use the configured identity, not the hardcoded default"
        );
    }
}

/// `--continue` finalizes without an editor, so `-m` is the only way to set the
/// message of a conflicted merge. It must also carry the configured identity.
#[tokio::test]
#[serial(cwd)]
async fn test_merge_continue_accepts_message_override_and_configured_identity() {
    let temp_repo = create_committed_repo_via_cli();
    let temp_path = temp_repo.path();

    assert_cli_success(
        &run_libra_command(&["branch", "feature"], temp_path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], temp_path),
        "checkout feature",
    );
    commit_file(temp_path, "tracked.txt", "feature change\n", "feature");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], temp_path),
        "checkout main",
    );
    commit_file(temp_path, "tracked.txt", "main change\n", "main");

    // Conflict, then resolve.
    assert_eq!(
        run_libra_command(&["merge", "feature"], temp_path)
            .status
            .code(),
        Some(128)
    );
    std::fs::write(temp_path.join("tracked.txt"), "resolved\n").expect("write resolution");
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], temp_path),
        "stage resolution",
    );

    assert_cli_success(
        &run_libra_command(
            &["merge", "--continue", "-m", "custom merge subject"],
            temp_path,
        ),
        "merge continue with -m",
    );

    let _guard = ChangeDirGuard::new(temp_path);
    let head = Head::current_commit().await.expect("continue moved HEAD");
    let commit: Commit = load_object(&head).expect("load continued merge commit");
    assert_eq!(commit.parent_commit_ids.len(), 2);
    // Commit messages are stored with a leading newline (`format_commit_msg`).
    assert!(
        commit
            .message
            .trim_start()
            .starts_with("custom merge subject"),
        "-m must override the message stored at merge start, got: {}",
        commit.message
    );
    assert!(
        !commit.message.contains("Merge feature into main"),
        "the stored default must not survive the override, got: {}",
        commit.message
    );
    assert_eq!(
        (commit.author.name.as_str(), commit.author.email.as_str()),
        ("Test User", "test@example.com"),
        "continued merge commit must use the configured identity"
    );
}

// ── ADR-MG-01 gitlink (submodule) fail-closed ────────────────────────────────
//
// Libra is a monorepo client and never merges submodule content. A three-way
// merge that would have to ARBITRATE a `160000` gitlink is refused before
// anything is written; a gitlink all three sides already agree on is carried
// through untouched. `merge`, `rebase` and `cherry-pick` share one guard, so
// the refusal text is identical apart from the operation name.

/// The submodule pointer the fixtures start from. A gitlink names a commit of
/// ANOTHER repository, so nothing requires it to exist here — which is exactly
/// why merging one cannot be resolved locally.
const GITLINK_BASE: &str = "0123456789abcdef0123456789abcdef01234567";
/// A different pointer, used to make one side of the merge move the submodule.
const GITLINK_MOVED: &str = "89abcdef0123456789abcdef0123456789abcdef";

fn stdout_trimmed(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Build a repository whose base commit tracks a `vendor` gitlink, with a
/// `feature` branch that adds `side.txt` and sets the gitlink to
/// `feature_gitlink`, and a `main` tip that adds `ours.txt`.
///
/// Everything is composed with plumbing (`update-index --cacheinfo` /
/// `write-tree` / `commit-tree` / `update-ref`) so no checkout ever has to
/// materialize the submodule, and `main`'s index is restored to the base tree
/// before the final commit — the fixture therefore starts from a clean status.
fn create_gitlink_repo(feature_gitlink: &str) -> tempfile::TempDir {
    create_gitlink_repo_with(feature_gitlink, false)
}

/// [`create_gitlink_repo`], optionally making both sides edit `tracked.txt` so
/// the merge stops on a real content conflict while the submodule pointer stays
/// untouched.
fn create_gitlink_repo_with(feature_gitlink: &str, conflicting: bool) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{GITLINK_BASE},vendor"),
            ],
            p,
        ),
        "stage the base gitlink",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "add submodule", "--no-verify"], p),
        "commit the base gitlink",
    );
    let base = head_commit(p);

    let side_blob = {
        let out = run_libra_command(&["hash-object", "-w", "--stdin"], p);
        assert!(out.status.success(), "hash-object must succeed");
        stdout_trimmed(&out)
    };
    // `hash-object --stdin` with no stdin body hashes the empty blob, which is
    // all this fixture needs: the point is that `feature` touches a file.
    let mut stage_feature = vec![
        "update-index".to_string(),
        "--cacheinfo".to_string(),
        format!("100644,{side_blob},side.txt"),
        "--cacheinfo".to_string(),
        format!("160000,{feature_gitlink},vendor"),
    ];
    if conflicting {
        let their_tracked = {
            let out =
                run_libra_command_with_stdin(&["hash-object", "-w", "--stdin"], p, "theirs edit\n");
            assert!(out.status.success(), "hash-object must succeed");
            stdout_trimmed(&out)
        };
        stage_feature.push("--cacheinfo".to_string());
        stage_feature.push(format!("100644,{their_tracked},tracked.txt"));
    }
    let stage_feature: Vec<&str> = stage_feature.iter().map(String::as_str).collect();
    assert_cli_success(
        &run_libra_command(&stage_feature, p),
        "stage the feature tree",
    );
    let tree = {
        let out = run_libra_command(&["write-tree"], p);
        assert_cli_success(&out, "write-tree");
        stdout_trimmed(&out)
    };
    let feature = {
        let out = run_libra_command(&["commit-tree", &tree, "-p", &base, "-m", "feature"], p);
        assert_cli_success(&out, "commit-tree");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(&["update-ref", "refs/heads/feature", &feature], p),
        "create refs/heads/feature",
    );

    // Put main's index back to the base tree: `side.txt` was only ever an index
    // entry, and the gitlink goes back to the pointer the base commit records.
    assert_cli_success(
        &run_libra_command(&["update-index", "--remove", "side.txt"], p),
        "unstage side.txt",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{GITLINK_BASE},vendor"),
            ],
            p,
        ),
        "restore the base gitlink",
    );

    std::fs::write(p.join("ours.txt"), "ours\n").expect("write ours.txt");
    assert_cli_success(&run_libra_command(&["add", "ours.txt"], p), "add ours.txt");
    if conflicting {
        std::fs::write(p.join("tracked.txt"), "ours edit\n").expect("write tracked.txt");
        assert_cli_success(
            &run_libra_command(&["add", "tracked.txt"], p),
            "add tracked.txt",
        );
    }
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ours", "--no-verify"], p),
        "commit ours",
    );

    repo
}

/// The `vendor` line of `libra ls-tree <rev>`, or `None` when the tree has no
/// such entry (which is what the silent-drop bug used to produce).
fn gitlink_tree_line(p: &Path, rev: &str) -> Option<String> {
    let out = run_libra_command(&["ls-tree", rev], p);
    assert_cli_success(&out, "ls-tree");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find(|line| line.ends_with("\tvendor"))
        .map(|line| line.to_string())
}

#[test]
fn merge_gitlink_divergent_pointer_is_refused_before_any_write() {
    let repo = create_gitlink_repo(GITLINK_MOVED);
    let p = repo.path();
    let head_before = head_commit(p);

    let output = run_libra_command(&["merge", "feature"], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-UNSUPPORTED-001");
    assert!(
        stderr.contains("'vendor'"),
        "the refusal must name the gitlink path, got: {stderr}"
    );
    assert!(
        stderr.contains("Libra does not support submodules"),
        "the refusal must say why it cannot be resolved, got: {stderr}"
    );
    // Fail-closed means fail BEFORE writing: no merge state, no moved HEAD, and
    // nothing from the other side staged or materialized.
    assert_eq!(head_commit(p), head_before, "HEAD must not move");
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "a refused merge must not record merge state"
    );
    assert!(
        !p.join("side.txt").exists(),
        "a refused merge must not write the other side's files"
    );
}

#[test]
fn merge_gitlink_agreed_pointer_passes_through_untouched() {
    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();

    let output = run_libra_command(&["merge", "feature"], p);
    assert_cli_success(&output, "a submodule no side moved needs no decision");

    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_BASE}\tvendor").as_str()),
        "the merge result must keep the submodule pointer verbatim"
    );
    // Regression guard for the silent-drop this card removed: the merge must
    // also leave the repository clean, i.e. the index still records the gitlink.
    let status = run_libra_command(&["status", "--short"], p);
    assert_cli_success(&status, "status after merge");
    assert!(
        !String::from_utf8_lossy(&status.stdout).contains("vendor"),
        "the carried-through submodule must not show up as a change"
    );
}

#[test]
fn merge_gitlink_rebase_consumer_refuses_divergent_pointer() {
    let repo = create_gitlink_repo(GITLINK_MOVED);
    let p = repo.path();

    let head_before = head_commit(p);
    let output = run_libra_command(&["rebase", "feature"], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-UNSUPPORTED-001");
    // Same shared guard as merge: identical wording apart from the operation.
    assert!(
        stderr.contains(
            "rebase would have to merge the submodule (gitlink) entry 'vendor': Libra does not support submodules"
        ),
        "rebase must refuse through the shared gitlink guard, got: {stderr}"
    );
    // The refusal must land before the start path's writes: the aux sidecar,
    // the HEAD detach, and the rebase state row.
    assert_eq!(head_commit(p), head_before, "HEAD must not move");
    assert!(
        !p.join(".libra").join("rebase-aux.json").exists(),
        "a refused rebase must not write the aux sidecar"
    );
    assert!(
        !p.join("side.txt").exists(),
        "a refused rebase must not materialize the replayed side"
    );
    let status = run_libra_command(&["status", "--short", "--branch"], p);
    assert_cli_success(&status, "status after a refused rebase");
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("## main"),
        "a refused rebase must leave the branch checked out, not a detached HEAD"
    );
}

#[test]
fn merge_gitlink_rebase_consumer_passes_through_agreed_pointer() {
    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();

    let output = run_libra_command(&["rebase", "feature"], p);
    assert_cli_success(&output, "rebase over an unchanged submodule");

    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_BASE}\tvendor").as_str()),
        "the replayed commit must keep the submodule pointer verbatim"
    );
}

#[test]
fn merge_gitlink_cherry_pick_consumer_refuses_divergent_pointer() {
    let repo = create_gitlink_repo(GITLINK_MOVED);
    let p = repo.path();
    let feature = {
        let out = run_libra_command(&["rev-parse", "feature"], p);
        assert_cli_success(&out, "rev-parse feature");
        stdout_trimmed(&out)
    };

    let head_before = head_commit(p);
    let output = run_libra_command(&["cherry-pick", &feature], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-UNSUPPORTED-001");
    assert!(
        stderr.contains(
            "cherry-pick would have to merge the submodule (gitlink) entry 'vendor': Libra does not support submodules"
        ),
        "cherry-pick must refuse through the shared gitlink guard, got: {stderr}"
    );
    // Refused before the first index/worktree/state write.
    assert_eq!(head_commit(p), head_before, "HEAD must not move");
    assert!(
        !p.join("side.txt").exists(),
        "a refused pick must not materialize the picked side"
    );
    let status = run_libra_command(&["status", "--short"], p);
    assert_cli_success(&status, "status after a refused cherry-pick");
    assert!(
        !String::from_utf8_lossy(&status.stdout).contains("side.txt"),
        "a refused pick must not stage the picked side"
    );
}

#[test]
fn merge_gitlink_cherry_pick_consumer_passes_through_agreed_pointer() {
    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();
    let feature = {
        let out = run_libra_command(&["rev-parse", "feature"], p);
        assert_cli_success(&out, "rev-parse feature");
        stdout_trimmed(&out)
    };

    let output = run_libra_command(&["cherry-pick", &feature], p);
    assert_cli_success(&output, "cherry-pick over an unchanged submodule");

    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_BASE}\tvendor").as_str()),
        "the picked commit must keep the submodule pointer verbatim"
    );
}

#[test]
fn merge_gitlink_agreed_pointer_survives_conflict_and_continue() {
    let repo = create_gitlink_repo_with(GITLINK_BASE, true);
    let p = repo.path();

    let conflicted = run_libra_command(&["merge", "feature"], p);
    let (_, report) = parse_cli_error_stderr(&conflicted.stderr);
    assert_eq!(
        report.error_code, "LBR-CONFLICT-002",
        "the fixture must stop on a real content conflict, not on the submodule"
    );

    std::fs::write(p.join("tracked.txt"), "resolved\n").expect("resolve the conflict");
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], p),
        "stage the resolution",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--continue", "--no-verify"], p),
        "finish the conflicted merge",
    );

    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_BASE}\tvendor").as_str()),
        "a submodule carried across a CONFLICTED merge must survive --continue"
    );
}

#[test]
fn merge_gitlink_divergent_pointer_refused_before_autostash_writes() {
    // The three-way engine's own gate runs after `--autostash` has created a
    // stash commit, fsynced its sidecar, and reset the working tree — so the
    // refusal has to happen in the wrapper, ahead of all three (ADR-MG-01 G1).
    let repo = create_gitlink_repo(GITLINK_MOVED);
    let p = repo.path();
    std::fs::write(p.join("tracked.txt"), "dirty\n").expect("dirty the worktree");

    let output = run_libra_command(&["merge", "--autostash", "feature"], p);
    let (_, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-UNSUPPORTED-001");
    assert_eq!(
        std::fs::read_to_string(p.join("tracked.txt")).expect("read tracked.txt"),
        "dirty\n",
        "the refused merge must not have stashed and reset the working tree"
    );
    assert!(
        !p.join(".libra").join("merge-autostash.json").exists(),
        "a refused merge must not leave an autostash sidecar"
    );
    let stash = run_libra_command(&["stash", "list"], p);
    assert_cli_success(&stash, "stash list after a refused merge");
    assert!(
        String::from_utf8_lossy(&stash.stdout).trim().is_empty(),
        "a refused merge must not create a stash entry"
    );
}

#[test]
fn merge_gitlink_agreed_pointer_survives_a_conflicted_rebase_replay() {
    // A replay that BOTH carries a pass-through gitlink and stops on a real
    // content conflict: the conflict path stages and materializes the merged
    // entries, and a submodule pointer has no blob to write.
    let repo = create_gitlink_repo_with(GITLINK_BASE, true);
    let p = repo.path();

    let conflicted = run_libra_command(&["rebase", "feature"], p);
    let (_, report) = parse_cli_error_stderr(&conflicted.stderr);
    assert_eq!(
        report.error_code, "LBR-CONFLICT-001",
        "the fixture must stop on a content conflict, not on the submodule"
    );

    std::fs::write(p.join("tracked.txt"), "resolved\n").expect("resolve the conflict");
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], p),
        "stage the resolution",
    );
    assert_cli_success(
        &run_libra_command(&["rebase", "--continue"], p),
        "finish the conflicted rebase",
    );

    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_BASE}\tvendor").as_str()),
        "a submodule carried across a CONFLICTED replay must survive --continue"
    );
}

#[test]
fn merge_gitlink_hard_reset_restores_a_tree_carrying_a_pointer() {
    // `reset --hard` rebuilds the index from the tree and restores the working
    // tree from it; a gitlink names a SUBMODULE's commit, which is not an
    // object of this repository, so neither step may ask for it as a blob.
    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();
    std::fs::write(p.join("ours.txt"), "dirty\n").expect("dirty a tracked file");
    // A user who checked the submodule out by hand: the gitlink path is a real
    // DIRECTORY, so the removal loop must not try to unlink it and the
    // untracked-overwrite check must not count its contents.
    std::fs::create_dir_all(p.join("vendor")).expect("materialize the submodule directory");
    std::fs::write(p.join("vendor").join("inner.txt"), "submodule\n").expect("submodule content");

    let output = run_libra_command(&["reset", "--hard", "HEAD"], p);
    assert_cli_success(&output, "hard reset in a repository carrying a gitlink");
    assert!(
        p.join("vendor").join("inner.txt").exists(),
        "a checked-out submodule directory is not Libra's to delete"
    );

    assert_eq!(
        std::fs::read_to_string(p.join("ours.txt")).expect("read ours.txt"),
        "ours\n",
        "the hard reset must still restore ordinary files"
    );
    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_BASE}\tvendor").as_str()),
        "the submodule pointer stays in the tree"
    );
    let files = run_libra_command(&["ls-files", "-s"], p);
    assert_cli_success(&files, "ls-files after a hard reset");
    assert!(
        String::from_utf8_lossy(&files.stdout)
            .lines()
            .any(|line| line.starts_with("160000") && line.ends_with("vendor")),
        "the rebuilt index keeps the gitlink entry"
    );
}

/// `main` carries `vendor` at [`GITLINK_BASE`]; `feature` is a DIRECT CHILD of
/// it that moves the pointer to [`GITLINK_MOVED`]. HEAD stays on `main`, so the
/// branch is an ancestor of `feature` — the shape `rebase` fast-forwards and
/// `cherry-pick --ff` fast-forwards, neither of which arbitrates anything.
fn create_gitlink_fast_forward_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{GITLINK_BASE},vendor"),
            ],
            p,
        ),
        "stage the base gitlink",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "add submodule", "--no-verify"], p),
        "commit the base gitlink",
    );
    let base = head_commit(p);

    let side_blob = {
        let out = run_libra_command(&["hash-object", "-w", "--stdin"], p);
        assert!(out.status.success(), "hash-object must succeed");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("100644,{side_blob},side.txt"),
                "--cacheinfo",
                &format!("160000,{GITLINK_MOVED},vendor"),
            ],
            p,
        ),
        "stage the child tree",
    );
    let tree = {
        let out = run_libra_command(&["write-tree"], p);
        assert_cli_success(&out, "write-tree");
        stdout_trimmed(&out)
    };
    let child = {
        let out = run_libra_command(
            &["commit-tree", &tree, "-p", &base, "-m", "move submodule"],
            p,
        );
        assert_cli_success(&out, "commit-tree");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(&["update-ref", "refs/heads/feature", &child], p),
        "create refs/heads/feature",
    );
    assert_cli_success(
        &run_libra_command(&["update-index", "--remove", "side.txt"], p),
        "unstage side.txt",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{GITLINK_BASE},vendor"),
            ],
            p,
        ),
        "restore the base gitlink",
    );

    repo
}

#[test]
fn merge_gitlink_fast_forward_rebase_adopts_a_moved_pointer() {
    // A rebase whose merge base IS the branch tip fast-forwards onto the
    // upstream tree wholesale: nothing is arbitrated, so a MOVED submodule
    // pointer must be adopted rather than refused.
    let repo = create_gitlink_fast_forward_repo();
    let p = repo.path();

    let output = run_libra_command(&["rebase", "feature"], p);
    assert_cli_success(&output, "fast-forward rebase over a moved submodule");

    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_MOVED}\tvendor").as_str()),
        "the fast-forward adopts the upstream pointer"
    );
}

#[test]
fn merge_gitlink_fast_forward_pick_adopts_a_moved_pointer_only_with_ff() {
    // `cherry-pick --ff` on a direct child of HEAD advances HEAD without
    // replaying, so it decides nothing and a moved pointer is adopted. The same
    // pick WITHOUT `--ff` performs a three-way apply and must be refused — the
    // preflight has to tell the two apart.
    let repo = create_gitlink_fast_forward_repo();
    let p = repo.path();
    let child = {
        let out = run_libra_command(&["rev-parse", "feature"], p);
        assert_cli_success(&out, "rev-parse feature");
        stdout_trimmed(&out)
    };

    let replayed = run_libra_command(&["cherry-pick", &child], p);
    let (_, report) = parse_cli_error_stderr(&replayed.stderr);
    assert_eq!(
        report.error_code, "LBR-UNSUPPORTED-001",
        "a replaying pick still has to arbitrate the moved pointer"
    );

    let output = run_libra_command(&["cherry-pick", "--ff", &child], p);
    assert_cli_success(&output, "fast-forward pick over a moved submodule");
    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_MOVED}\tvendor").as_str()),
        "the fast-forward pick adopts the moved pointer"
    );
}

/// Run `git` in `cwd`, asserting success. Used only by the `pull --rebase`
/// fixture below: `pull` needs a real remote, and git is the one tool that can
/// author a gitlink-bearing history on the far side.
fn git_in(args: &[&str], cwd: &Path) -> String {
    let output = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("failed to execute git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is utf8")
        .trim()
        .to_string()
}

#[test]
fn merge_gitlink_pull_rebase_refuses_before_its_autostash() {
    // `pull --rebase` is a SECOND entry into the replay and pushes its own
    // autostash before handing off, so the refusal has to come from `pull`
    // itself — the gate inside the rebase start path would already be too late.
    let temp_root = tempfile::tempdir().expect("temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let work_dir = temp_root.path().join("workdir");
    git_in(
        &["init", "--bare", remote_dir.to_str().unwrap()],
        temp_root.path(),
    );
    git_in(&["init", work_dir.to_str().unwrap()], temp_root.path());
    git_in(&["config", "user.name", "Libra Tester"], &work_dir);
    git_in(&["config", "user.email", "tester@example.com"], &work_dir);

    std::fs::write(work_dir.join("README.md"), "hello\n").expect("write README");
    git_in(&["add", "README.md"], &work_dir);
    git_in(
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{GITLINK_BASE},vendor"),
        ],
        &work_dir,
    );
    git_in(&["commit", "-m", "initial commit"], &work_dir);
    let branch = git_in(&["rev-parse", "--abbrev-ref", "HEAD"], &work_dir);
    git_in(
        &["remote", "add", "origin", remote_dir.to_str().unwrap()],
        &work_dir,
    );
    git_in(
        &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
        &work_dir,
    );

    let local = tempfile::tempdir().expect("local repo");
    let p = local.path();
    assert_cli_success(&run_libra_command(&["init"], p), "libra init");
    assert_cli_success(
        &run_libra_command(&["config", "user.name", "Libra Tester"], p),
        "set user.name",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "tester@example.com"], p),
        "set user.email",
    );
    assert_cli_success(
        &run_libra_command(
            &["remote", "add", "origin", remote_dir.to_str().unwrap()],
            p,
        ),
        "remote add",
    );
    assert_cli_success(
        &run_libra_command(&["config", "branch.main.remote", "origin"], p),
        "set branch.main.remote",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "config",
                "branch.main.merge",
                &format!("refs/heads/{branch}"),
            ],
            p,
        ),
        "set branch.main.merge",
    );
    assert_cli_success(&run_libra_command(&["pull"], p), "initial pull");
    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_BASE}\tvendor").as_str()),
        "the fetched history carries the submodule pointer"
    );

    // Diverge: a local commit, and a remote commit that MOVES the pointer.
    std::fs::write(p.join("local.txt"), "local\n").expect("write local.txt");
    assert_cli_success(
        &run_libra_command(&["add", "local.txt"], p),
        "add local.txt",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "local work", "--no-verify"], p),
        "local commit",
    );
    let head_before = head_commit(p);

    git_in(
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{GITLINK_MOVED},vendor"),
        ],
        &work_dir,
    );
    std::fs::write(work_dir.join("remote.txt"), "remote\n").expect("write remote.txt");
    git_in(&["add", "remote.txt"], &work_dir);
    git_in(&["commit", "-m", "move submodule"], &work_dir);
    git_in(
        &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
        &work_dir,
    );

    std::fs::write(p.join("local.txt"), "dirty\n").expect("dirty the worktree");
    let output = run_libra_command(&["pull", "--rebase", "--autostash"], p);
    let (_, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-UNSUPPORTED-001");
    assert_eq!(head_commit(p), head_before, "HEAD must not move");
    assert_eq!(
        std::fs::read_to_string(p.join("local.txt")).expect("read local.txt"),
        "dirty\n",
        "the refused pull must not have stashed and reset the working tree"
    );
    let stash = run_libra_command(&["stash", "list"], p);
    assert_cli_success(&stash, "stash list after a refused pull --rebase");
    assert!(
        String::from_utf8_lossy(&stash.stdout).trim().is_empty(),
        "a refused pull must not create a stash entry"
    );
}

/// `main` has no submodule; `feature` is a direct child that ADDS `vendor` at
/// [`GITLINK_BASE`]. HEAD stays on `main`, so the merge fast-forwards.
fn create_gitlink_adding_fast_forward_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let base = head_commit(p);

    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{GITLINK_BASE},vendor"),
            ],
            p,
        ),
        "stage the gitlink",
    );
    let tree = {
        let out = run_libra_command(&["write-tree"], p);
        assert_cli_success(&out, "write-tree");
        stdout_trimmed(&out)
    };
    let child = {
        let out = run_libra_command(
            &["commit-tree", &tree, "-p", &base, "-m", "add submodule"],
            p,
        );
        assert_cli_success(&out, "commit-tree");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(&["update-ref", "refs/heads/feature", &child], p),
        "create refs/heads/feature",
    );
    assert_cli_success(
        &run_libra_command(&["update-index", "--remove", "vendor"], p),
        "unstage the gitlink",
    );

    repo
}

#[test]
fn merge_gitlink_refuses_to_replace_an_untracked_file_at_the_pointer_path() {
    // Materializing a `160000` entry creates a DIRECTORY placeholder, which
    // would delete a plain file sitting exactly there. That path is matched
    // exactly (files UNDER a submodule directory belong to the submodule and
    // are not overwritten), and the refusal lands before HEAD moves.
    let repo = create_gitlink_adding_fast_forward_repo();
    let p = repo.path();
    std::fs::write(p.join("vendor"), "not a submodule\n").expect("write untracked file");
    let head_before = head_commit(p);

    let output = run_libra_command(&["merge", "feature"], p);
    let (_, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert_eq!(head_commit(p), head_before, "HEAD must not move");
    assert_eq!(
        std::fs::read_to_string(p.join("vendor")).expect("read the untracked file"),
        "not a submodule\n",
        "the untracked file must survive the refusal"
    );
}

#[test]
fn merge_gitlink_leaves_a_checked_out_submodule_directory_untouched() {
    // The same fast-forward, but the path is a real submodule checkout: its
    // files are untracked and live UNDER the pointer, so they neither block the
    // merge nor get deleted by it.
    let repo = create_gitlink_adding_fast_forward_repo();
    let p = repo.path();
    std::fs::create_dir_all(p.join("vendor")).expect("materialize the submodule");
    std::fs::write(p.join("vendor").join("inner.txt"), "submodule\n").expect("submodule content");

    let output = run_libra_command(&["merge", "feature"], p);
    assert_cli_success(&output, "fast-forward adopting a checked-out submodule");

    assert_eq!(
        std::fs::read_to_string(p.join("vendor").join("inner.txt")).expect("read submodule file"),
        "submodule\n",
        "the submodule checkout is not Libra's to touch"
    );
    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_BASE}\tvendor").as_str()),
        "the pointer is adopted"
    );
}

/// [`create_gitlink_repo`] plus a `dropped` branch: a direct child of HEAD whose
/// tree no longer declares `vendor`.
fn create_gitlink_dropping_repo() -> tempfile::TempDir {
    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();
    let base = head_commit(p);
    assert_cli_success(
        &run_libra_command(&["update-index", "--remove", "vendor"], p),
        "stage the removal",
    );
    let tree = {
        let out = run_libra_command(&["write-tree"], p);
        assert_cli_success(&out, "write-tree");
        stdout_trimmed(&out)
    };
    let child = {
        let out = run_libra_command(
            &["commit-tree", &tree, "-p", &base, "-m", "drop submodule"],
            p,
        );
        assert_cli_success(&out, "commit-tree");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(&["update-ref", "refs/heads/dropped", &child], p),
        "create refs/heads/dropped",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{GITLINK_BASE},vendor"),
            ],
            p,
        ),
        "restore the gitlink",
    );
    repo
}

#[test]
fn merge_gitlink_fast_forward_dropping_a_pointer_refuses_before_moving_head() {
    // `restore` refuses to replace a NON-EMPTY materialized submodule directory
    // (its own long-standing contract). The fast-forward restores AFTER moving
    // the ref, so that refusal has to be raised beforehand — otherwise the
    // branch ends up ahead of the index and the working tree.
    let repo = create_gitlink_dropping_repo();
    let p = repo.path();
    std::fs::create_dir_all(p.join("vendor")).expect("materialize the submodule");
    std::fs::write(p.join("vendor").join("inner.txt"), "submodule\n").expect("submodule content");
    let head_before = head_commit(p);

    let output = run_libra_command(&["merge", "dropped"], p);
    let (stderr, _) = parse_cli_error_stderr(&output.stderr);

    assert!(
        stderr.contains("refusing to replace non-empty worktree directory 'vendor'"),
        "the refusal must name the submodule directory, got: {stderr}"
    );
    assert_eq!(
        head_commit(p),
        head_before,
        "the refusal must land BEFORE the ref moves"
    );
    assert!(
        p.join("vendor").join("inner.txt").exists(),
        "the submodule checkout survives"
    );
    assert_eq!(
        gitlink_tree_line(p, "HEAD").as_deref(),
        Some(format!("160000 commit {GITLINK_BASE}\tvendor").as_str()),
        "HEAD still declares the pointer"
    );
}

#[test]
fn merge_gitlink_fast_forward_dropping_a_pointer_clears_an_empty_placeholder() {
    // With only Libra's own empty placeholder there, the same fast-forward
    // completes and HEAD, the index and the working tree all agree.
    let repo = create_gitlink_dropping_repo();
    let p = repo.path();
    std::fs::create_dir_all(p.join("vendor")).expect("materialize the placeholder");

    let output = run_libra_command(&["merge", "dropped"], p);
    assert_cli_success(&output, "fast-forward dropping an unmaterialized submodule");

    assert_eq!(
        gitlink_tree_line(p, "HEAD"),
        None,
        "the tree drops the pointer"
    );
    let files = run_libra_command(&["ls-files", "-s"], p);
    assert_cli_success(&files, "ls-files after the drop");
    assert!(
        !String::from_utf8_lossy(&files.stdout).contains("vendor"),
        "the index drops the pointer too"
    );
}

#[test]
fn merge_gitlink_restore_refuses_to_delete_an_untracked_file_at_the_pointer_path() {
    // `restore --source` materializes a `160000` entry as a directory
    // placeholder, which would remove a plain file sitting exactly there. An
    // UNTRACKED file is the user's, so the whole restore is refused before any
    // write; files BENEATH a checked-out submodule are untouched either way.
    let repo = create_gitlink_adding_fast_forward_repo();
    let p = repo.path();
    std::fs::write(p.join("vendor"), "not a submodule\n").expect("write untracked file");

    let output = run_libra_command(&["restore", "--source", "feature", "--worktree", "."], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(
        stderr.contains("refusing to replace worktree path 'vendor'"),
        "the refusal must name the path, got: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("vendor")).expect("read the untracked file"),
        "not a submodule\n",
        "the untracked file must survive"
    );
}

#[test]
fn merge_gitlink_cleanliness_gate_ignores_a_submodule_but_not_a_plain_file() {
    // The gate (`status::changes_to_be_staged`, read by
    // `switch::ensure_clean_status`) ignores the two shapes Libra expects for a
    // `160000` entry — absent (never materialized) and a directory (checked out
    // by the user) — otherwise no repository containing a submodule could merge
    // at all. A plain file at that path is neither, and must NOT be waved
    // through silently.
    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["merge", "feature"], p),
        "an unmaterialized submodule must not read as a dirty worktree",
    );

    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();
    std::fs::create_dir_all(p.join("vendor")).expect("materialize the submodule");
    std::fs::write(p.join("vendor").join("inner.txt"), "submodule\n").expect("submodule content");
    assert_cli_success(
        &run_libra_command(&["merge", "feature"], p),
        "a checked-out submodule directory must not read as a dirty worktree",
    );

    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();
    std::fs::write(p.join("vendor"), "not a submodule\n").expect("write a file at the path");
    let refused = run_libra_command(&["merge", "feature"], p);
    let (stderr, report) = parse_cli_error_stderr(&refused.stderr);
    assert_eq!(
        report.error_code, "LBR-CONFLICT-002",
        "a plain file where a submodule belongs is a dirty worktree, not a no-op"
    );
    assert!(
        stderr.contains("uncommitted changes"),
        "the gate must name the dirty worktree, got: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("vendor")).expect("read the file"),
        "not a submodule\n"
    );
}

#[test]
fn merge_gitlink_multi_pick_refusal_survives_a_later_empty_commit() {
    // The whole-sequence preflight stops modelling at a pick the sequencer
    // would reject for its own reason (here: an empty commit without
    // `--allow-empty`). It must still decide on what it modelled so far —
    // otherwise the divergent pointer in the FIRST pick escapes the gate and is
    // applied before the per-pick guard refuses it.
    let repo = create_gitlink_repo(GITLINK_MOVED);
    let p = repo.path();
    let diverging = {
        let out = run_libra_command(&["rev-parse", "feature"], p);
        assert_cli_success(&out, "rev-parse feature");
        stdout_trimmed(&out)
    };
    // An empty commit on top of `feature`: same tree, so the pick would stop
    // with `EmptyCommit` rather than a gitlink verdict.
    let feature_tree = {
        let out = run_libra_command(&["rev-parse", "feature^{tree}"], p);
        assert_cli_success(&out, "rev-parse feature tree");
        stdout_trimmed(&out)
    };
    let empty = {
        let out = run_libra_command(
            &[
                "commit-tree",
                &feature_tree,
                "-p",
                &diverging,
                "-m",
                "empty",
            ],
            p,
        );
        assert_cli_success(&out, "commit-tree");
        stdout_trimmed(&out)
    };
    let head_before = head_commit(p);

    let output = run_libra_command(&["cherry-pick", &diverging, &empty], p);
    let (_, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(
        report.error_code, "LBR-UNSUPPORTED-001",
        "the first pick's divergent pointer must still be refused"
    );
    assert_eq!(head_commit(p), head_before, "no pick may be applied");
}

#[test]
fn merge_gitlink_restore_refuses_a_file_left_where_the_index_records_a_pointer() {
    // "Tracked" is not enough to make a path replaceable: the index entry here
    // IS the gitlink, so the file at that path was never written by Libra and
    // is not recoverable from the object store. Only ordinary tracked content
    // may be replaced by the directory placeholder.
    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();
    std::fs::write(p.join("vendor"), "left behind\n").expect("write a file at the path");

    let output = run_libra_command(&["restore", "--source", "HEAD", "--worktree", "."], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(
        stderr.contains("refusing to replace worktree path 'vendor'"),
        "the refusal must name the path, got: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("vendor")).expect("read the file"),
        "left behind\n"
    );
}

#[test]
fn merge_gitlink_hard_reset_keeps_a_file_left_where_the_index_records_a_pointer() {
    // `reset --hard` to a tree WITHOUT the pointer must not delete that file
    // either: Libra never materialized the submodule, so nothing at that path
    // is its to remove. (`cherry-pick --ff` delegates here.)
    let repo = create_gitlink_dropping_repo();
    let p = repo.path();
    std::fs::write(p.join("vendor"), "left behind\n").expect("write a file at the path");
    let dropped = {
        let out = run_libra_command(&["rev-parse", "dropped"], p);
        assert_cli_success(&out, "rev-parse dropped");
        stdout_trimmed(&out)
    };

    let output = run_libra_command(&["reset", "--hard", &dropped], p);
    assert_cli_success(&output, "hard reset dropping the pointer");

    assert_eq!(
        std::fs::read_to_string(p.join("vendor")).expect("read the file"),
        "left behind\n",
        "a file at a former submodule path is not Libra's to delete"
    );
}

#[test]
fn merge_gitlink_restore_refuses_to_drop_a_pointer_over_user_content() {
    // The other direction across the same path: the source no longer declares
    // the submodule, so a plain restore would REMOVE whatever is there. That
    // content is the user's — Libra never wrote it — so the restore refuses
    // before touching anything.
    let repo = create_gitlink_dropping_repo();
    let p = repo.path();
    std::fs::write(p.join("vendor"), "left behind\n").expect("write a file at the path");

    let output = run_libra_command(&["restore", "--source", "dropped", "--worktree", "."], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(
        stderr.contains("refusing to replace worktree path 'vendor'"),
        "the refusal must name the path, got: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("vendor")).expect("read the file"),
        "left behind\n"
    );
}

#[test]
fn merge_gitlink_overlay_restore_still_refuses_a_source_present_replacement() {
    // `--overlay` only suppresses DELETION of paths the source omits. A source
    // that REPLACES the submodule with an ordinary blob still writes, so the
    // guard must stay armed for it.
    let repo = create_gitlink_repo(GITLINK_BASE);
    let p = repo.path();
    let base = head_commit(p);
    let blob = {
        let out = run_libra_command_with_stdin(&["hash-object", "-w", "--stdin"], p, "replaced\n");
        assert!(out.status.success(), "hash-object must succeed");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("100644,{blob},vendor"),
            ],
            p,
        ),
        "stage a blob at the submodule path",
    );
    let tree = {
        let out = run_libra_command(&["write-tree"], p);
        assert_cli_success(&out, "write-tree");
        stdout_trimmed(&out)
    };
    let replaced = {
        let out = run_libra_command(
            &["commit-tree", &tree, "-p", &base, "-m", "submodule to file"],
            p,
        );
        assert_cli_success(&out, "commit-tree");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(&["update-ref", "refs/heads/replaced", &replaced], p),
        "create refs/heads/replaced",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{GITLINK_BASE},vendor"),
            ],
            p,
        ),
        "restore the gitlink",
    );
    std::fs::write(p.join("vendor"), "left behind\n").expect("write a file at the path");

    let output = run_libra_command(
        &[
            "restore",
            "--overlay",
            "--source",
            "replaced",
            "--worktree",
            ".",
        ],
        p,
    );
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(
        stderr.contains("refusing to replace worktree path 'vendor'"),
        "the refusal must name the path, got: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("vendor")).expect("read the file"),
        "left behind\n"
    );
}

#[test]
fn merge_gitlink_fast_forward_rebase_refuses_before_moving_the_ref() {
    // `rebase`'s fast-forward branch materializes the worktree AFTER updating
    // the ref and the index, so a transition the materialization would refuse
    // (here: a non-empty submodule directory the upstream tree drops) has to be
    // caught first — otherwise the branch runs ahead of the working tree.
    let repo = create_gitlink_dropping_repo();
    let p = repo.path();
    std::fs::create_dir_all(p.join("vendor")).expect("materialize the submodule");
    std::fs::write(p.join("vendor").join("inner.txt"), "submodule\n").expect("submodule content");
    let head_before = head_commit(p);

    let output = run_libra_command(&["rebase", "dropped"], p);
    let (stderr, _) = parse_cli_error_stderr(&output.stderr);

    assert!(
        stderr.contains("refusing to replace non-empty worktree directory 'vendor'"),
        "the refusal must name the submodule directory, got: {stderr}"
    );
    assert_eq!(
        head_commit(p),
        head_before,
        "the refusal must land BEFORE the ref moves"
    );
    assert!(
        p.join("vendor").join("inner.txt").exists(),
        "the submodule checkout survives"
    );
}

// ---------------------------------------------------------------------------
// MG-02: criss-cross histories (several merge bases) and the recursive virtual
// ancestor they are folded into.
// ---------------------------------------------------------------------------

/// A criss-cross history, the shape Git's `t6024-recursive-merge.sh`
/// (git@`3cb9185f6`) is built around: two branches that merged each other, so
/// the two tips below have TWO merge bases and neither dominates the other.
///
/// ```text
///            ┌─ a(f=1) ─┐    x = merge(a, b) ── ours (adds t)
///   main(o) ─┤          ├────
///            └─ b(g=1) ─┘    y = merge(b, a) ── theirs (f=2, g=2)
/// ```
///
/// `merge_bases(ours, theirs) == {a, b}`, and the fixture is chosen so that
/// EITHER single base gives the wrong answer: relative to `a` the `g` edits on
/// both sides look divergent, relative to `b` the `f` edits do. Only the
/// recursive ancestor (`f=1, g=1`) explains both sides' history.
fn create_crisscross_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("f.txt"), "0\n").expect("write f");
    std::fs::write(p.join("g.txt"), "0\n").expect("write g");
    assert_cli_success(
        &run_libra_command(&["add", "f.txt", "g.txt"], p),
        "add roots",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "commit root",
    );

    for (branch, file, content) in [("a", "f.txt", "1\n"), ("b", "g.txt", "1\n")] {
        assert_cli_success(
            &run_libra_command(&["checkout", "main"], p),
            "checkout main",
        );
        assert_cli_success(&run_libra_command(&["branch", branch], p), "create branch");
        assert_cli_success(
            &run_libra_command(&["checkout", branch], p),
            "checkout branch",
        );
        commit_file(p, file, content, "side edit");
    }

    // The two merges have to be made from THROWAWAY branches: merging `b` into
    // `a` itself would leave `a` an ancestor of the result, and the second
    // merge would fast-forward instead of criss-crossing.
    for (from, tip, other) in [("a", "x", "b"), ("b", "y", "a")] {
        assert_cli_success(&run_libra_command(&["checkout", from], p), "checkout side");
        assert_cli_success(&run_libra_command(&["branch", tip], p), "create tip branch");
        assert_cli_success(&run_libra_command(&["checkout", tip], p), "checkout tip");
        assert_cli_success(
            &run_libra_command(&["merge", other], p),
            "criss-cross merge",
        );
    }

    assert_cli_success(&run_libra_command(&["checkout", "x"], p), "checkout x");
    commit_file(p, "t.txt", "ours\n", "ours-only file");

    assert_cli_success(&run_libra_command(&["checkout", "y"], p), "checkout y");
    std::fs::write(p.join("f.txt"), "2\n").expect("write f");
    std::fs::write(p.join("g.txt"), "2\n").expect("write g");
    assert_cli_success(
        &run_libra_command(&["add", "f.txt", "g.txt"], p),
        "add edits",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "theirs edits", "--no-verify"], p),
        "commit theirs",
    );

    assert_cli_success(&run_libra_command(&["checkout", "x"], p), "checkout x");
    repo
}

fn read_merge_state(p: &Path) -> serde_json::Value {
    let raw = std::fs::read(p.join(".libra").join("merge-state.json"))
        .expect("merge-state.json written by a conflicted merge");
    serde_json::from_slice(&raw).expect("merge-state.json is json")
}

/// G6: with two merge bases, folding them into a virtual ancestor merges
/// cleanly where picking either single base reports a conflict.
#[test]
fn merge_crisscross_folds_both_bases_and_merges_cleanly() {
    let repo = create_crisscross_repo();
    let p = repo.path();

    let output = run_libra_command(&["--json", "merge", "y"], p);
    assert_cli_success(&output, "criss-cross merge");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["strategy"], "three-way");
    assert!(
        json["data"]["conflicted_paths"].is_null(),
        "a clean merge omits the key entirely (frozen schema): {json}"
    );
    assert_eq!(
        json["data"]["files_changed"], 2,
        "only the two files `theirs` re-edited change"
    );

    assert_eq!(
        std::fs::read_to_string(p.join("f.txt")).expect("f"),
        "2\n",
        "the virtual ancestor already carries f=1, so theirs' edit applies cleanly"
    );
    assert_eq!(std::fs::read_to_string(p.join("g.txt")).expect("g"), "2\n");
    assert_eq!(
        std::fs::read_to_string(p.join("t.txt")).expect("t"),
        "ours\n",
        "our side's own file survives"
    );

    let raw = run_libra_command(&["cat-file", "-p", "HEAD"], p);
    assert_cli_success(&raw, "cat-file the merge commit");
    assert_eq!(
        String::from_utf8_lossy(&raw.stdout)
            .lines()
            .filter(|line| line.starts_with("parent "))
            .count(),
        2,
        "a criss-cross merge still records the two REAL parents, not the virtual ancestor"
    );
}

/// G6 (conflict half) + G7: when the sides really do diverge relative to the
/// virtual ancestor the merge conflicts as usual — and the state it writes
/// records no `base`, because a virtual ancestor is a one-shot object that must
/// not become a GC root (ADR-MG-04).
#[test]
fn merge_crisscross_conflict_records_no_virtual_base_in_the_state() {
    let repo = create_crisscross_repo();
    let p = repo.path();
    // Diverge from the virtual ancestor (f=1) on OUR side too, so f is a real
    // both-modified conflict; g stays clean.
    commit_file(p, "f.txt", "ours-2\n", "ours re-edits f");

    let output = run_libra_command(&["merge", "y"], p);
    assert_eq!(
        output.status.code(),
        Some(128),
        "the merge conflicts (LBR-CONFLICT-002)"
    );
    let conflicted = std::fs::read_to_string(p.join("f.txt")).expect("f");
    assert!(
        conflicted.contains("<<<<<<< HEAD") && conflicted.contains("ours-2"),
        "the OUTER conflict keeps the default seven-character markers: {conflicted}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("g.txt")).expect("g"),
        "2\n",
        "the path the virtual ancestor explains still merges cleanly"
    );

    let state = read_merge_state(p);
    assert!(
        state.get("base").is_none_or(serde_json::Value::is_null),
        "the virtual ancestor is never recorded as the merge base: {state}"
    );
    assert!(
        state["conflicted_paths"]
            .as_array()
            .expect("conflicted_paths")
            .iter()
            .any(|path| path == "f.txt"),
        "the conflicted path is recorded: {state}"
    );
}

/// The single-base path is untouched by MG-02: an ordinary diverged merge still
/// records its real base and merges exactly as before.
#[test]
fn merge_crisscross_single_base_merge_is_unchanged() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "shared.txt", "base\n", "shared base");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(p, "feature.txt", "feature\n", "feature file");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    commit_file(p, "main.txt", "main\n", "main file");
    let base = head_commit(p);

    let output = run_libra_command(&["--json", "merge", "feature"], p);
    assert_cli_success(&output, "single-base merge");
    assert_eq!(parse_json_stdout(&output)["data"]["strategy"], "three-way");
    assert_ne!(head_commit(p), base, "a merge commit was created");
    assert!(p.join("feature.txt").exists() && p.join("main.txt").exists());
}

/// `--allow-unrelated-histories` keeps its virtual EMPTY base: zero merge bases
/// is still zero, not something the fold is asked to build.
#[test]
fn merge_crisscross_unrelated_histories_keep_the_empty_base() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["checkout", "--orphan", "imported"], p),
        "orphan branch",
    );
    std::fs::write(p.join("imported.txt"), "imported\n").expect("write imported");
    assert_cli_success(&run_libra_command(&["add", "imported.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "imported root", "--no-verify"], p),
        "commit orphan",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");

    let refused = run_libra_command(&["merge", "imported"], p);
    assert_eq!(refused.status.code(), Some(128), "unrelated by default");

    let output = run_libra_command(
        &["--json", "merge", "--allow-unrelated-histories", "imported"],
        p,
    );
    assert_cli_success(&output, "unrelated merge with the empty base");
    assert_eq!(parse_json_stdout(&output)["data"]["strategy"], "three-way");
    assert!(p.join("imported.txt").exists() && p.join("tracked.txt").exists());
}

/// `--restart` recomputes the virtual ancestor from the REAL bases and lands on
/// the same conflict — the fold is deterministic (bases folded in hex order).
#[test]
fn merge_crisscross_restart_recomputes_the_virtual_ancestor() {
    let repo = create_crisscross_repo();
    let p = repo.path();
    commit_file(p, "f.txt", "ours-2\n", "ours re-edits f");
    let ours = head_commit(p);

    assert_eq!(
        run_libra_command(&["merge", "y"], p).status.code(),
        Some(128),
        "the merge conflicts"
    );
    let first = std::fs::read_to_string(p.join("f.txt")).expect("f");
    // Overwrite the conflict resolution: --restart must discard it.
    std::fs::write(p.join("f.txt"), "hand-resolved\n").expect("resolve");

    let restarted = run_libra_command(&["merge", "--restart"], p);
    assert_eq!(
        restarted.status.code(),
        Some(128),
        "the restart re-conflicts"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("f.txt")).expect("f"),
        first,
        "the recomputed ancestor reproduces the same conflict byte for byte"
    );
    assert_eq!(head_commit(p), ours, "HEAD is still the pre-merge commit");
}

/// Every loose object currently on disk, as `<dir><file>` object ids.
fn loose_object_ids(p: &Path) -> std::collections::BTreeSet<String> {
    let mut ids = std::collections::BTreeSet::new();
    let objects = p.join(".libra").join("objects");
    let Ok(dirs) = std::fs::read_dir(&objects) else {
        return ids;
    };
    for dir in dirs.flatten() {
        let prefix = dir.file_name().to_string_lossy().to_string();
        if prefix.len() != 2 {
            continue;
        }
        if let Ok(files) = std::fs::read_dir(dir.path()) {
            for file in files.flatten() {
                ids.insert(format!("{prefix}{}", file.file_name().to_string_lossy()));
            }
        }
    }
    ids
}

/// Age every loose object past the prune grace window and drive the GC
/// quarantine's two phases explicitly, the way `maintenance_test` does, instead
/// of waiting an hour.
fn prune_unreachable_objects(p: &Path) -> String {
    let objects = p.join(".libra").join("objects");
    let aged = std::process::Command::new("find")
        .arg(&objects)
        .args([
            "-type",
            "f",
            "-exec",
            "touch",
            "-t",
            "200001010000",
            "{}",
            ";",
        ])
        .status()
        .expect("spawn find");
    assert!(aged.success(), "backdate the loose objects");

    let first = run_libra_command(&["maintenance", "run", "--task", "gc"], p);
    assert_cli_success(&first, "gc quarantines the unreachable objects");
    let ledger_path = p.join(".libra").join("gc-prune-candidates.json");
    let ledger: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&ledger_path).expect("ledger")).expect("ledger json");
    let aged_ledger: serde_json::Map<String, serde_json::Value> = ledger
        .as_object()
        .expect("ledger object")
        .keys()
        .map(|oid| (oid.clone(), serde_json::json!(0)))
        .collect();
    assert!(
        !aged_ledger.is_empty(),
        "something unreachable must have been quarantined"
    );
    std::fs::write(
        &ledger_path,
        serde_json::to_vec(&serde_json::Value::Object(aged_ledger)).expect("serialize"),
    )
    .expect("age the ledger");

    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], p);
    assert_cli_success(&gc, "gc prunes without a dangling-reference error");
    String::from_utf8_lossy(&gc.stdout).to_string()
}

/// G8 + G9: the virtual ancestor is NOT a GC root. `maintenance gc` reclaims
/// the exact objects the fold created, mid-merge, without any
/// dangling-reference complaint — and `--restart` brings those same object ids
/// back, which is ADR-MG-04's whole recovery contract (the fold is
/// deterministic, so recomputation is bit-identical).
#[test]
fn merge_crisscross_gc_reclaims_the_virtual_ancestor_and_restart_recovers() {
    let repo = create_crisscross_repo();
    let p = repo.path();
    commit_file(p, "f.txt", "ours-2\n", "ours re-edits f");

    let before_merge = loose_object_ids(p);
    assert_eq!(
        run_libra_command(&["merge", "y"], p).status.code(),
        Some(128),
        "the merge conflicts and leaves state behind"
    );
    let conflicted = std::fs::read_to_string(p.join("f.txt")).expect("f");
    let created: std::collections::BTreeSet<String> = loose_object_ids(p)
        .difference(&before_merge)
        .cloned()
        .collect();
    assert!(
        !created.is_empty(),
        "the merge writes the virtual ancestor's objects"
    );

    let target_before = read_merge_state(p)["target"].clone();
    let gc_out = prune_unreachable_objects(p);
    let after_gc = loose_object_ids(p);
    let pruned: Vec<String> = created.difference(&after_gc).cloned().collect();
    // The synthetic COMMIT is always new; its tree usually is not, because
    // folding two bases reproduces a tree an earlier merge already wrote and
    // object storage is content-addressed. What matters is that whatever the
    // fold DID add is unrooted and reclaimable.
    assert!(
        !pruned.is_empty(),
        "the virtual ancestor is unrooted, so gc takes it (created={created:?}, \
         gc said: {gc_out})"
    );
    assert_eq!(
        read_merge_state(p)["target"],
        target_before,
        "the merge state survives the prune intact — it never named the virtual ancestor, \
         so the sidecar root check has nothing to fail closed on"
    );

    // Recovery does not depend on the reclaimed objects: --restart rebuilds the
    // ancestor from the real merge bases, byte for byte.
    let restarted = run_libra_command(&["merge", "--restart"], p);
    assert_eq!(
        restarted.status.code(),
        Some(128),
        "the restart re-runs the merge and re-conflicts"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("f.txt")).expect("f"),
        conflicted,
        "the ancestor recomputed after the prune is the same one"
    );
    let after_restart = loose_object_ids(p);
    let missing: Vec<&String> = pruned
        .iter()
        .filter(|oid| !after_restart.contains(*oid))
        .collect();
    assert!(
        missing.is_empty(),
        "every reclaimed object is recomputed by --restart: {missing:?}"
    );
}

/// G10: a recursive merge adds no field to `merge-state.json`, so a state file
/// in the pre-existing schema still drives `--abort` to completion.
#[test]
fn merge_crisscross_merge_state_keeps_the_older_schema_readable() {
    let repo = create_crisscross_repo();
    let p = repo.path();
    commit_file(p, "f.txt", "ours-2\n", "ours re-edits f");
    let ours = head_commit(p);

    assert_eq!(
        run_libra_command(&["merge", "y"], p).status.code(),
        Some(128),
        "the merge conflicts"
    );
    let state = read_merge_state(p);
    let known = [
        "head_name",
        "orig_head",
        "target",
        "target_ref",
        "base",
        "strategy",
        "allow_unrelated_histories",
        "skip_hooks",
        "conflicted_paths",
        "message",
        // Injected at the JSON layer by `MergeState::save` (W2 worktree
        // ownership), not part of the merge's own schema.
        "owner_scope",
    ];
    for key in state.as_object().expect("state object").keys() {
        assert!(
            known.contains(&key.as_str()),
            "a recursive merge must not grow the state schema; found '{key}'"
        );
    }

    // Rewrite it in the pre-P1-07b shape (no strategy / unrelated / hook flags,
    // no base) and confirm it is still a state this binary can finish.
    let old_schema = serde_json::json!({
        "owner_scope": state["owner_scope"],
        "head_name": state["head_name"],
        "orig_head": state["orig_head"],
        "target": state["target"],
        "target_ref": state["target_ref"],
        "conflicted_paths": state["conflicted_paths"],
        "message": state["message"],
    });
    std::fs::write(
        p.join(".libra").join("merge-state.json"),
        serde_json::to_vec(&old_schema).expect("serialize"),
    )
    .expect("write old-schema state");

    let aborted = run_libra_command(&["merge", "--abort"], p);
    assert_cli_success(&aborted, "abort reads the older state schema");
    assert_eq!(head_commit(p), ours);
    assert_eq!(
        std::fs::read_to_string(p.join("f.txt")).expect("f"),
        "ours-2\n"
    );
}

/// A criss-cross `--dry-run` previews the folded result and still writes
/// nothing: the fold keeps its blobs in memory and materializes no virtual
/// tree or commit when the merge is only being previewed.
#[test]
fn merge_crisscross_dry_run_previews_without_writing_objects() {
    let repo = create_crisscross_repo();
    let p = repo.path();
    let head_before = head_commit(p);
    let objects_before = count_loose_objects(p);

    let output = run_libra_command(&["--json", "merge", "--dry-run", "y"], p);
    assert_cli_success(&output, "criss-cross dry run");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["dry_run"], true);
    assert!(
        json["data"]["would_conflict"].is_null(),
        "a clean preview omits `would_conflict` (frozen schema): {json}"
    );
    assert_eq!(json["data"]["files_changed"], 2);

    assert_eq!(count_loose_objects(p), objects_before, "no objects written");
    assert_eq!(head_commit(p), head_before);
    assert!(!p.join(".libra").join("merge-state.json").exists());
    assert_eq!(std::fs::read_to_string(p.join("f.txt")).expect("f"), "1\n");
}

/// A history with THREE merge bases, which forces the fold to run more than one
/// step: `merge_bases_of_folded` has to answer for the ancestor already folded
/// from `a` and `b` before `c` can be folded in.
///
/// ```text
///            ┌─ a(f=1) ─┐
///   main(o) ─┼─ b(g=1) ─┼── x = ((a ⊕ b) ⊕ c) ── ours
///            └─ c(h=1) ─┘   y = ((b ⊕ c) ⊕ a) ── theirs
/// ```
///
/// `merge_bases(ours, theirs) == {a, b, c}`, and EVERY single base is wrong:
/// relative to `a` the `g`/`h` edits look divergent, relative to `b` the `f`/`h`
/// ones do, and relative to `c` the `f`/`g` ones do. Only the folded ancestor
/// (`f=1, g=1, h=1`) explains both sides.
fn create_three_base_crisscross_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    for file in ["f.txt", "g.txt", "h.txt"] {
        std::fs::write(p.join(file), "0\n").expect("write root file");
    }
    assert_cli_success(
        &run_libra_command(&["add", "f.txt", "g.txt", "h.txt"], p),
        "add roots",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "commit root",
    );

    for (branch, file) in [("a", "f.txt"), ("b", "g.txt"), ("c", "h.txt")] {
        assert_cli_success(
            &run_libra_command(&["checkout", "main"], p),
            "checkout main",
        );
        assert_cli_success(&run_libra_command(&["branch", branch], p), "create branch");
        assert_cli_success(
            &run_libra_command(&["checkout", branch], p),
            "checkout branch",
        );
        commit_file(p, file, "1\n", "side edit");
    }

    for (from, tip, others) in [("a", "x", ["b", "c"]), ("b", "y", ["c", "a"])] {
        assert_cli_success(&run_libra_command(&["checkout", from], p), "checkout side");
        assert_cli_success(&run_libra_command(&["branch", tip], p), "create tip branch");
        assert_cli_success(&run_libra_command(&["checkout", tip], p), "checkout tip");
        for other in others {
            assert_cli_success(
                &run_libra_command(&["merge", other], p),
                "criss-cross merge",
            );
        }
    }

    assert_cli_success(&run_libra_command(&["checkout", "y"], p), "checkout y");
    for file in ["f.txt", "g.txt", "h.txt"] {
        std::fs::write(p.join(file), "2\n").expect("write theirs");
    }
    assert_cli_success(
        &run_libra_command(&["add", "f.txt", "g.txt", "h.txt"], p),
        "add theirs",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "theirs edits", "--no-verify"], p),
        "commit theirs",
    );

    assert_cli_success(&run_libra_command(&["checkout", "x"], p), "checkout x");
    repo
}

/// G1 + G2 + G6 at three bases: the two fold steps produce ONE ancestor that
/// resolves the paths no single base can, the one genuinely divergent path
/// still conflicts, and `--restart` recomputes the whole fold byte-identically
/// (the fold order is fixed, so a recompute cannot land anywhere else).
#[test]
fn merge_crisscross_three_merge_bases_fold_into_one_ancestor() {
    let repo = create_three_base_crisscross_repo();
    let p = repo.path();
    // Diverge from the folded ancestor (f=1) on our side too, so `f` is a real
    // both-modified conflict while `g` and `h` stay clean.
    commit_file(p, "f.txt", "ours-2\n", "ours re-edits f");
    let ours = head_commit(p);

    assert_eq!(
        run_libra_command(&["merge", "y"], p).status.code(),
        Some(128),
        "only the genuinely divergent path conflicts"
    );
    let conflicted = std::fs::read_to_string(p.join("f.txt")).expect("f");
    assert!(
        conflicted.contains("<<<<<<< HEAD") && conflicted.contains("ours-2"),
        "f.txt carries the outer conflict: {conflicted}"
    );
    for file in ["g.txt", "h.txt"] {
        assert_eq!(
            std::fs::read_to_string(p.join(file)).expect("clean path"),
            "2\n",
            "{file} merges cleanly ONLY through the ancestor folded from all three bases"
        );
    }
    let state = read_merge_state(p);
    assert!(
        state.get("base").is_none_or(serde_json::Value::is_null),
        "the folded ancestor is not recorded as the merge base: {state}"
    );

    std::fs::write(p.join("f.txt"), "hand-resolved\n").expect("resolve");
    let restarted = run_libra_command(&["merge", "--restart"], p);
    assert_eq!(
        restarted.status.code(),
        Some(128),
        "the restart re-conflicts"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("f.txt")).expect("f"),
        conflicted,
        "two fold steps recompute to the same ancestor, so the conflict is identical"
    );
    assert_eq!(head_commit(p), ours, "HEAD is still the pre-merge commit");
}

/// G5 end to end: a conflict recorded INSIDE the virtual ancestor keeps markers
/// two characters wider than the merge that reads it back. With
/// `merge.conflictStyle=diff3` the ancestor's content is printed in the
/// `|||||||` block of the outer conflict, so both widths are visible in one
/// file — and the nested ones carry Git's temporary-branch labels.
#[test]
fn merge_crisscross_nested_conflict_markers_are_wider_than_the_outer_ones() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "merge.conflictStyle", "diff3"], p),
        "configure diff3",
    );
    std::fs::write(p.join("p.txt"), "0\n").expect("write p");
    assert_cli_success(&run_libra_command(&["add", "p.txt"], p), "add p");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "commit root",
    );

    // Both sides change the SAME line, so folding the two bases conflicts.
    for (branch, content) in [("a", "a\n"), ("b", "b\n")] {
        assert_cli_success(
            &run_libra_command(&["checkout", "main"], p),
            "checkout main",
        );
        assert_cli_success(&run_libra_command(&["branch", branch], p), "create branch");
        assert_cli_success(
            &run_libra_command(&["checkout", branch], p),
            "checkout branch",
        );
        commit_file(p, "p.txt", content, "side edit");
    }

    // The criss-cross merges themselves conflict; resolve each by hand so the
    // recorded trees do NOT contain what the fold will compute.
    for (from, tip, other, resolution) in [("a", "x", "b", "x\n"), ("b", "y", "a", "y\n")] {
        assert_cli_success(&run_libra_command(&["checkout", from], p), "checkout side");
        assert_cli_success(&run_libra_command(&["branch", tip], p), "create tip branch");
        assert_cli_success(&run_libra_command(&["checkout", tip], p), "checkout tip");
        assert_eq!(
            run_libra_command(&["merge", other], p).status.code(),
            Some(128),
            "the criss-cross merge conflicts"
        );
        std::fs::write(p.join("p.txt"), resolution).expect("resolve");
        assert_cli_success(&run_libra_command(&["add", "p.txt"], p), "stage resolution");
        assert_cli_success(
            &run_libra_command(&["merge", "--continue", "--no-verify"], p),
            "finish the criss-cross merge",
        );
    }

    assert_cli_success(&run_libra_command(&["checkout", "x"], p), "checkout x");
    assert_eq!(
        run_libra_command(&["merge", "y"], p).status.code(),
        Some(128),
        "ours and theirs both differ from the folded ancestor"
    );

    let text = std::fs::read_to_string(p.join("p.txt")).expect("p");
    assert!(
        text.contains("<<<<<<<<< Temporary merge branch 1")
            && text.contains(">>>>>>>>> Temporary merge branch 2"),
        "the ancestor's own conflict is nine characters wide (7 + 2 x depth 1) and labelled \
         the way Git labels a virtual-ancestor merge: {text}"
    );
    assert!(
        text.contains("<<<<<<<<<< HEAD"),
        "the outer merge's markers are widened past the nested ones, so the two levels can \
         never be confused: {text}"
    );
}

/// `--dry-run` must not touch a leftover autostash sidecar either: recovering
/// one promotes it into the stash list and deletes the file, and both are
/// writes. A preview leaves it exactly where it found it, for the next REAL
/// merge to recover.
#[test]
fn merge_crisscross_dry_run_leaves_a_stale_autostash_sidecar_untouched() {
    let repo = create_crisscross_repo();
    let p = repo.path();
    // A syntactically valid sidecar naming an object that does not exist: a
    // real merge would refuse to proceed past it, a preview must not even read
    // it as something to act on.
    let sidecar = p.join(".libra").join("merge-autostash.json");
    let stale = r#"{"stash_commit":"0123456789abcdef0123456789abcdef01234567"}"#;
    std::fs::write(&sidecar, stale).expect("plant a stale sidecar");
    let stashes_before = stash_list_len(p);
    let objects_before = count_loose_objects(p);

    let output = run_libra_command(&["--json", "merge", "--dry-run", "y"], p);
    assert_cli_success(&output, "dry run with a stale sidecar present");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["dry_run"], true);
    assert!(
        json["data"]["autostash"].is_null(),
        "a preview reports no autostash outcome: {json}"
    );

    assert_eq!(
        std::fs::read_to_string(&sidecar).expect("sidecar still present"),
        stale,
        "the stale sidecar is neither recovered nor rewritten by a preview"
    );
    assert_eq!(
        stash_list_len(p),
        stashes_before,
        "nothing promoted to the stash list"
    );
    assert_eq!(count_loose_objects(p), objects_before, "no objects written");
}

/// The width ceiling on the COMMAND path: with more merge bases than Libra
/// folds, `merge` is refused with `LBR-UNSUPPORTED-001` before it loads a
/// single base commit or tree, and — like every refusal — writes nothing.
///
/// The fixture builds 33 mutually independent common ancestors (`a01`..`a33`
/// off `main`) and two tips that each reach all of them by different routes.
#[test]
fn merge_crisscross_more_bases_than_the_width_ceiling_is_refused_before_loading() {
    const WIDTH: usize = 33;
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let names: Vec<String> = (1..=WIDTH).map(|i| format!("a{i:02}")).collect();
    for name in &names {
        assert_cli_success(
            &run_libra_command(&["checkout", "main"], p),
            "checkout main",
        );
        assert_cli_success(
            &run_libra_command(&["branch", name], p),
            "create base branch",
        );
        assert_cli_success(&run_libra_command(&["checkout", name], p), "checkout base");
        commit_file(p, &format!("{name}.txt"), "1\n", "independent ancestor");
    }
    // x folds a01..a33 in order; y folds a33..a01 in reverse. Both reach every
    // ancestor, neither reaches the other, and no ancestor dominates another.
    for (tip, order) in [
        ("x", names.clone()),
        ("y", names.iter().rev().cloned().collect::<Vec<_>>()),
    ] {
        assert_cli_success(
            &run_libra_command(&["checkout", &order[0]], p),
            "checkout first",
        );
        assert_cli_success(&run_libra_command(&["branch", tip], p), "create tip");
        assert_cli_success(&run_libra_command(&["checkout", tip], p), "checkout tip");
        for other in &order[1..] {
            assert_cli_success(&run_libra_command(&["merge", other], p), "fold ancestor in");
        }
    }
    assert_cli_success(&run_libra_command(&["checkout", "x"], p), "checkout x");
    commit_file(p, "ours.txt", "ours\n", "diverge ours");
    assert_cli_success(&run_libra_command(&["checkout", "y"], p), "checkout y");
    commit_file(p, "theirs.txt", "theirs\n", "diverge theirs");
    assert_cli_success(&run_libra_command(&["checkout", "x"], p), "checkout x");

    let head_before = head_commit(p);
    let objects_before = count_loose_objects(p);
    let output = run_libra_command(&["merge", "y"], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128), "refused: {stderr}");
    assert_eq!(report.error_code, "LBR-UNSUPPORTED-001");
    assert!(
        stderr.contains("folded from 33 merge bases, more than the 32 Libra folds"),
        "the refusal names the width: {stderr}"
    );
    assert_eq!(head_commit(p), head_before, "HEAD untouched");
    assert_eq!(count_loose_objects(p), objects_before, "no objects written");
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "no merge state written"
    );
    assert!(!p.join("theirs.txt").exists(), "worktree untouched");

    // `--ff-only` never folds either: a diverged history is refused as
    // non-fast-forward (LBR-CONFLICT-002), exactly as with one merge base —
    // the width ceiling must not pre-empt that verdict.
    let ff_only = run_libra_command(&["merge", "--ff-only", "y"], p);
    let (ff_stderr, ff_report) = parse_cli_error_stderr(&ff_only.stderr);
    assert_eq!(ff_only.status.code(), Some(128), "refused: {ff_stderr}");
    assert_eq!(
        ff_report.error_code, "LBR-CONFLICT-002",
        "--ff-only reports non-fast-forward, not the width ceiling: {ff_stderr}"
    );
    assert!(
        !ff_stderr.contains("merge bases"),
        "the width ceiling stays out of a --ff-only verdict: {ff_stderr}"
    );

    // `-s ours` never folds, so it is never refused for width.
    let ours = run_libra_command(&["--json", "merge", "-s", "ours", "y"], p);
    assert_cli_success(&ours, "-s ours ignores the width ceiling");
    assert_eq!(parse_json_stdout(&ours)["data"]["strategy"], "ours");
}

/// MG-03 G4 end to end: the flattening path is still selectable under the test
/// sentinel and produces the same merge — same tree, same files_changed — as
/// the default incremental walk.
#[test]
fn merge_tree_walk_flat_switch_matches_the_incremental_default() {
    let run = |flat: bool| -> (String, serde_json::Value) {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        std::fs::create_dir_all(p.join("deep/er/dir")).expect("dirs");
        std::fs::write(p.join("deep/er/dir/leaf.txt"), "0\n").expect("leaf");
        std::fs::write(p.join("top.txt"), "0\n").expect("top");
        assert_cli_success(&run_libra_command(&["add", "."], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
            "root",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
        commit_file(p, "top.txt", "ours\n", "ours edit");
        assert_cli_success(
            &run_libra_command(&["checkout", "feature"], p),
            "co feature",
        );
        commit_file(p, "deep/er/dir/leaf.txt", "theirs\n", "theirs deep edit");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
        let mut env: Vec<(&str, &str)> = vec![("LIBRA_TEST", "1")];
        if flat {
            env.push(("LIBRA_TEST_MERGE_TREE_WALK", "flat"));
        }
        let output =
            run_libra_command_with_stdin_and_env(&["--json", "merge", "feature"], p, "", &env);
        assert_cli_success(&output, "merge under the selected tree walk");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json summary");
        let tree = run_libra_command(&["rev-parse", "HEAD^{tree}"], p);
        assert_cli_success(&tree, "tree id");
        (
            String::from_utf8_lossy(&tree.stdout).trim().to_string(),
            json["data"].clone(),
        )
    };
    let (incremental_tree, incremental) = run(false);
    let (flat_tree, flat) = run(true);
    assert_eq!(
        incremental_tree, flat_tree,
        "both paths write the same merged tree"
    );
    assert_eq!(incremental["files_changed"], flat["files_changed"]);
    assert_eq!(
        incremental["files_changed"], 1,
        "only the deep leaf changed relative to ours"
    );
    assert_eq!(incremental["strategy"], "three-way");
}

/// MG-03 G1/G2/G5 at the PRODUCTION entry: the default `libra merge` takes the
/// incremental walk and reads only the trees along the changed paths — proven
/// through the `LIBRA_TEST_MERGE_TREE_STATS` seam rather than an in-memory
/// graph. A deep subtree all three sides share is never opened; the subtree
/// theirs changed is opened once per side per differing level.
#[test]
fn merge_tree_walk_default_is_incremental_and_reads_only_changed_paths() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    for dir in ["shared/a/b/c", "moved/x/y"] {
        std::fs::create_dir_all(p.join(dir)).expect("dirs");
    }
    std::fs::write(p.join("shared/a/b/c/leaf.txt"), "0\n").expect("shared leaf");
    std::fs::write(p.join("moved/x/y/leaf.txt"), "0\n").expect("moved leaf");
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    assert_cli_success(&run_libra_command(&["add", "."], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    commit_file(p, "top.txt", "ours\n", "ours edit");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(p, "moved/x/y/leaf.txt", "theirs\n", "theirs deep edit");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    // An untracked file under the SHARED subtree: the untracked-collision check
    // must not expand `shared/` for it (ours already has that subtree; nothing
    // under it is written), so the read bound below still holds.
    std::fs::write(p.join("shared/a/b/untracked.txt"), "stray\n").expect("untracked");

    let stats_dir = tempfile::tempdir().expect("stats dir");
    let stats = stats_dir.path().join("stats.json");
    let stats_path = stats.to_string_lossy().to_string();
    let output = run_libra_command_with_stdin_and_env(
        &["--json", "merge", "feature"],
        p,
        "",
        &[
            ("LIBRA_TEST", "1"),
            ("LIBRA_TEST_MERGE_TREE_STATS", &stats_path),
        ],
    );
    assert_cli_success(&output, "default merge");
    let recorded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&stats).expect("stats written")).expect("stats json");
    assert_eq!(
        recorded["walk"], "incremental",
        "the default merge takes the pruning walk"
    );
    let reads = recorded["tree_reads"].as_u64().expect("tree_reads") as usize;
    // Per pass: 3 distinct roots + `moved/x/y` (three levels, two distinct
    // sides: base == ours, theirs) = 9; `shared/…` is identical everywhere: 0.
    // Two passes read from the store — the preflight gate's and the engine's,
    // each with its own cache — so the whole merge is bounded by 2 × 9.
    assert!(
        reads <= 2 * (3 + 6),
        "tree reads are bounded by the changed path, not the tree: {reads} ({recorded})"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("moved/x/y/leaf.txt")).expect("merged leaf"),
        "theirs\n"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("top.txt")).expect("top"),
        "ours\n"
    );

    // The same repository shape under the flat switch records the flat walk.
    let repo2 = create_committed_repo_via_cli();
    let q = repo2.path();
    std::fs::write(q.join("a.txt"), "0\n").expect("a");
    assert_cli_success(&run_libra_command(&["add", "a.txt"], q), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], q),
        "root",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], q), "branch");
    commit_file(q, "b.txt", "ours\n", "ours");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], q),
        "co feature",
    );
    commit_file(q, "a.txt", "theirs\n", "theirs");
    assert_cli_success(&run_libra_command(&["checkout", "main"], q), "co main");
    let output = run_libra_command_with_stdin_and_env(
        &["merge", "feature"],
        q,
        "",
        &[
            ("LIBRA_TEST", "1"),
            ("LIBRA_TEST_MERGE_TREE_WALK", "flat"),
            ("LIBRA_TEST_MERGE_TREE_STATS", &stats_path),
        ],
    );
    assert_cli_success(&output, "flat merge");
    let recorded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&stats).expect("stats written")).expect("stats json");
    assert_eq!(recorded["walk"], "flat");
}

/// MG-03: an untracked FILE whose path is an ancestor of a subtree the merge
/// would adopt verbatim collides exactly as the flattening path's per-leaf
/// check says it does — refused before HEAD, index or worktree change.
#[test]
fn merge_tree_walk_refuses_untracked_ancestor_of_an_adopted_subtree() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    assert_cli_success(&run_libra_command(&["add", "top.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    commit_file(p, "top.txt", "ours\n", "ours edit");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(
        p,
        "newdir/sub/leaf.txt",
        "theirs\n",
        "theirs adds a subtree",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    // An untracked FILE at `newdir` blocks the adopted `newdir/…` subtree.
    std::fs::write(p.join("newdir"), "untracked file, not a directory\n").expect("untracked");
    let head_before = head_commit(p);
    let objects_before = count_loose_objects(p);

    let output = run_libra_command(&["merge", "feature"], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128), "refused: {stderr}");
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(
        stderr.contains("newdir"),
        "the colliding untracked path is named: {stderr}"
    );
    assert_eq!(head_commit(p), head_before, "HEAD untouched");
    assert_eq!(
        std::fs::read_to_string(p.join("newdir")).expect("untracked survives"),
        "untracked file, not a directory\n"
    );
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "no merge state"
    );
    assert!(
        count_loose_objects(p) <= objects_before + 1,
        "no tree or commit written (at most the auto-merge's blob-free walk): before \
         {objects_before}, after {}",
        count_loose_objects(p)
    );
}

/// MG-03: a `pre-merge-commit` hook that drops an untracked file UNDER a
/// subtree the merge adopts verbatim is caught by the post-hook recheck — the
/// collision set is recomputed after every hook, never reused — so the merge
/// is refused before HEAD moves, exactly as the flattening path refuses it.
#[cfg(unix)]
#[test]
fn merge_tree_walk_rechecks_hook_created_files_under_adopted_subtrees() {
    use std::os::unix::fs::PermissionsExt;
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    assert_cli_success(&run_libra_command(&["add", "top.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    commit_file(p, "top.txt", "ours\n", "ours edit");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(
        p,
        "newdir/sub/leaf.txt",
        "theirs\n",
        "theirs adds a subtree",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    // The hook creates a file at a path the adopted subtree will write.
    let hooks = p.join(".libra").join("hooks");
    std::fs::create_dir_all(&hooks).expect("hooks dir");
    let hook = hooks.join("pre-merge-commit");
    std::fs::write(
        &hook,
        "#!/bin/sh\nmkdir -p \"$LIBRA_WORK_TREE/newdir/sub\"\nprintf 'hook\\n' > \"$LIBRA_WORK_TREE/newdir/sub/leaf.txt\"\n",
    )
    .expect("write hook");
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let head_before = head_commit(p);

    let output = run_libra_command(&["merge", "feature"], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128), "refused: {stderr}");
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(
        stderr.contains("newdir/sub/leaf.txt"),
        "the hook-created colliding path is named: {stderr}"
    );
    assert_eq!(head_commit(p), head_before, "HEAD untouched");
    assert_eq!(
        std::fs::read_to_string(p.join("newdir/sub/leaf.txt")).expect("hook file survives"),
        "hook\n",
        "the untracked file the hook wrote is not overwritten"
    );
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "no merge state"
    );
}

/// MG-03: a subtree theirs ADDED is enumerated in full by the read-only gate
/// before anything is written (an added directory has no counterpart on the
/// other sides, so nothing about it can be skipped). With one of its nested
/// tree objects missing from the store the merge fails the way the flattening
/// path failed — while reading the trees — and HEAD, the index and the working
/// tree are untouched. (Trees the walk leaves unopened are HEAD's own; see the
/// unopened-tree invariant on `incremental_merge_trees`.)
#[test]
fn merge_tree_walk_refuses_an_added_subtree_with_a_missing_nested_tree() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    assert_cli_success(&run_libra_command(&["add", "top.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    commit_file(p, "top.txt", "ours\n", "ours edit");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(
        p,
        "newdir/sub/leaf.txt",
        "theirs\n",
        "theirs adds a nested subtree",
    );
    // Only the leaf's tree differs from the base at the top level; the adopted
    // `newdir/` subtree's nested `sub/` tree is what goes missing.
    let nested = run_libra_command(&["rev-parse", "feature:newdir/sub"], p);
    assert_cli_success(&nested, "resolve the nested tree");
    let nested_id = String::from_utf8_lossy(&nested.stdout).trim().to_string();
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    let object = p
        .join(".libra")
        .join("objects")
        .join(&nested_id[..2])
        .join(&nested_id[2..]);
    assert!(
        object.exists(),
        "the nested tree is a loose object: {}",
        object.display()
    );
    std::fs::remove_file(&object).expect("simulate a missing nested tree");
    let head_before = head_commit(p);

    let output = run_libra_command(&["merge", "feature"], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128), "refused: {stderr}");
    assert_eq!(
        report.error_code, "LBR-REPO-002",
        "a missing tree is repository corruption"
    );
    assert!(
        stderr.contains(&nested_id),
        "the unreadable tree is named: {stderr}"
    );
    assert_eq!(head_commit(p), head_before, "HEAD never moved");
    assert!(
        !p.join("newdir").exists(),
        "nothing of the adopted subtree was written"
    );
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "no merge state"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("top.txt")).expect("top"),
        "ours\n"
    );
}

/// MG-03: a `refs/replace` substitution on a ROOT tree is honoured identically
/// by both walks — the flattening path loads roots through the
/// replacement-aware loader and nested trees raw, and the incremental path
/// mirrors exactly that (roots via `replace::resolve`, nested via the raw
/// loader) — so default and flat merges agree, and both see the replacement.
#[test]
fn merge_tree_walk_root_tree_replacement_is_honoured_identically_by_both_walks() {
    let run = |flat: bool| -> (String, serde_json::Value, String) {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        std::fs::write(p.join("a.txt"), "0\n").expect("a");
        std::fs::write(p.join("b.txt"), "0\n").expect("b");
        assert_cli_success(&run_libra_command(&["add", "a.txt", "b.txt"], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
            "root",
        );
        assert_cli_success(
            &run_libra_command(&["branch", "feature"], p),
            "branch feature",
        );
        assert_cli_success(&run_libra_command(&["branch", "alt"], p), "branch alt");
        commit_file(p, "b.txt", "ours\n", "ours edit");
        assert_cli_success(
            &run_libra_command(&["checkout", "feature"], p),
            "co feature",
        );
        commit_file(p, "a.txt", "theirs\n", "theirs edit");
        let feature_tree = run_libra_command(&["rev-parse", "feature^{tree}"], p);
        assert_cli_success(&feature_tree, "feature tree");
        // The replacement: a root tree where a.txt says something else.
        assert_cli_success(&run_libra_command(&["checkout", "alt"], p), "co alt");
        commit_file(p, "a.txt", "replaced\n", "alt edit");
        let alt_tree = run_libra_command(&["rev-parse", "alt^{tree}"], p);
        assert_cli_success(&alt_tree, "alt tree");
        assert_cli_success(
            &run_libra_command(
                &[
                    "replace",
                    String::from_utf8_lossy(&feature_tree.stdout).trim(),
                    String::from_utf8_lossy(&alt_tree.stdout).trim(),
                ],
                p,
            ),
            "replace feature's root tree",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
        let mut env: Vec<(&str, &str)> = vec![("LIBRA_TEST", "1")];
        if flat {
            env.push(("LIBRA_TEST_MERGE_TREE_WALK", "flat"));
        }
        let output =
            run_libra_command_with_stdin_and_env(&["--json", "merge", "feature"], p, "", &env);
        assert_cli_success(&output, "merge with a replaced root tree");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json summary");
        let tree = run_libra_command(&["rev-parse", "HEAD^{tree}"], p);
        assert_cli_success(&tree, "merged tree");
        (
            String::from_utf8_lossy(&tree.stdout).trim().to_string(),
            json["data"].clone(),
            std::fs::read_to_string(p.join("a.txt")).expect("a"),
        )
    };
    let (incremental_tree, incremental, incremental_a) = run(false);
    let (flat_tree, flat, flat_a) = run(true);
    assert_eq!(
        incremental_a, "replaced\n",
        "the replacement root tree is what gets merged"
    );
    assert_eq!(
        flat_a, incremental_a,
        "both walks see the same replaced root"
    );
    assert_eq!(
        incremental_tree, flat_tree,
        "both walks write the same merged tree"
    );
    assert_eq!(incremental["files_changed"], flat["files_changed"]);
}

/// MG-03: a nested tree that all three sides SHARE and the walk therefore never
/// opens is still read by the checkout — which now runs before the commit and
/// HEAD are written — so a missing shared tree fails with HEAD, index and
/// working tree untouched, exactly as the flattening path's up-front read did.
#[test]
fn merge_tree_walk_missing_shared_tree_fails_before_head_moves() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::create_dir_all(p.join("shared/a/b")).expect("dirs");
    std::fs::write(p.join("shared/a/b/leaf.txt"), "0\n").expect("shared leaf");
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    assert_cli_success(&run_libra_command(&["add", "."], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    commit_file(p, "top.txt", "ours\n", "ours edit");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(p, "other.txt", "theirs\n", "theirs edit");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    let shared = run_libra_command(&["rev-parse", "HEAD:shared/a"], p);
    assert_cli_success(&shared, "resolve the shared nested tree");
    let shared_id = String::from_utf8_lossy(&shared.stdout).trim().to_string();
    let object = p
        .join(".libra")
        .join("objects")
        .join(&shared_id[..2])
        .join(&shared_id[2..]);
    assert!(object.exists(), "loose object: {}", object.display());
    std::fs::remove_file(&object).expect("simulate a missing shared tree");
    let head_before = head_commit(p);
    let index_before = std::fs::read(p.join(".libra/index")).expect("index bytes");

    let output = run_libra_command(&["merge", "feature"], p);
    // Both walks reach the missing tree through the same reader (`Tree::load`
    // inside the index rebuild here; inside flattening on the flat walk), which
    // aborts rather than returning a structured report — a pre-existing,
    // path-independent shape. What this test pins is WHEN: before HEAD, the
    // index or the working tree changed.
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        !output.status.success(),
        "the merge cannot complete: {stderr}"
    );
    assert!(
        stderr.contains(&shared_id),
        "the unreadable tree is named: {stderr}"
    );
    assert_eq!(head_commit(p), head_before, "HEAD never moved");
    assert_eq!(
        std::fs::read(p.join(".libra/index")).expect("index bytes"),
        index_before,
        "the index is untouched"
    );
    assert!(
        !p.join("other.txt").exists(),
        "nothing of the merge result was written"
    );
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "no merge state"
    );
}

/// MG-03: with a replaced root tree that CONFLICTS, the conflict state names
/// the replaced content on stage 3 — identically on both walks — so
/// `restore --theirs` and `--continue` operate on what was actually merged.
#[test]
fn merge_tree_walk_conflict_stages_follow_the_replaced_root_on_both_walks() {
    let run = |flat: bool| -> String {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        std::fs::write(p.join("a.txt"), "0\n").expect("a");
        assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
            "root",
        );
        assert_cli_success(
            &run_libra_command(&["branch", "feature"], p),
            "branch feature",
        );
        assert_cli_success(&run_libra_command(&["branch", "alt"], p), "branch alt");
        commit_file(p, "a.txt", "ours\n", "ours edit");
        assert_cli_success(
            &run_libra_command(&["checkout", "feature"], p),
            "co feature",
        );
        commit_file(p, "a.txt", "theirs\n", "theirs edit");
        let feature_tree = run_libra_command(&["rev-parse", "feature^{tree}"], p);
        assert_cli_success(&feature_tree, "feature tree");
        assert_cli_success(&run_libra_command(&["checkout", "alt"], p), "co alt");
        commit_file(p, "a.txt", "replaced\n", "alt edit");
        let alt_tree = run_libra_command(&["rev-parse", "alt^{tree}"], p);
        assert_cli_success(&alt_tree, "alt tree");
        assert_cli_success(
            &run_libra_command(
                &[
                    "replace",
                    String::from_utf8_lossy(&feature_tree.stdout).trim(),
                    String::from_utf8_lossy(&alt_tree.stdout).trim(),
                ],
                p,
            ),
            "replace feature's root tree",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
        let mut env: Vec<(&str, &str)> = vec![("LIBRA_TEST", "1")];
        if flat {
            env.push(("LIBRA_TEST_MERGE_TREE_WALK", "flat"));
        }
        let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", &env);
        assert_eq!(
            output.status.code(),
            Some(128),
            "the replaced root conflicts with ours"
        );
        assert_cli_success(
            &run_libra_command(&["restore", "--theirs", "a.txt"], p),
            "restore theirs from stage 3",
        );
        std::fs::read_to_string(p.join("a.txt")).expect("a")
    };
    let incremental = run(false);
    let flat = run(true);
    assert_eq!(
        incremental, "replaced\n",
        "stage 3 is the REPLACED root's content"
    );
    assert_eq!(
        flat, incremental,
        "both walks record the same stage-3 entry"
    );
}

/// Build the nested-gitlink fixture shared by the two MG-03 G9/G10 CLI tests:
/// `main` (ours) carries `deps/vendor/lib` = [`GITLINK_BASE`] plus `deps/keep.txt`,
/// then edits only `top.txt`, so the whole `deps/` subtree still equals the base
/// on ours — the shape the incremental walk prunes past. `feature` (theirs) is
/// the base tree plus `side.txt`, with the gitlink set to `feature_gitlink`.
fn create_nested_gitlink_repo(feature_gitlink: &str) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    std::fs::create_dir_all(p.join("deps")).expect("deps");
    std::fs::write(p.join("deps/keep.txt"), "keep\n").expect("keep");
    assert_cli_success(
        &run_libra_command(&["add", "top.txt", "deps/keep.txt"], p),
        "add files",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{GITLINK_BASE},deps/vendor/lib"),
            ],
            p,
        ),
        "stage the nested base gitlink",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "base with nested submodule", "--no-verify"],
            p,
        ),
        "commit the base",
    );
    let base = head_commit(p);

    let side_blob = {
        let out = run_libra_command(&["hash-object", "-w", "--stdin"], p);
        assert!(out.status.success(), "hash-object must succeed");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("100644,{side_blob},side.txt"),
                "--cacheinfo",
                &format!("160000,{feature_gitlink},deps/vendor/lib"),
            ],
            p,
        ),
        "stage the feature tree",
    );
    let tree = {
        let out = run_libra_command(&["write-tree"], p);
        assert_cli_success(&out, "write-tree");
        stdout_trimmed(&out)
    };
    let feature = {
        let out = run_libra_command(&["commit-tree", &tree, "-p", &base, "-m", "feature"], p);
        assert_cli_success(&out, "commit-tree");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(&["update-ref", "refs/heads/feature", &feature], p),
        "create refs/heads/feature",
    );
    // Put main's index back to the base tree, then make ours' own change at
    // the ROOT only, leaving `deps/` byte-identical to the base.
    assert_cli_success(
        &run_libra_command(&["update-index", "--remove", "side.txt"], p),
        "unstage side.txt",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{GITLINK_BASE},deps/vendor/lib"),
            ],
            p,
        ),
        "restore the base gitlink in main's index",
    );
    commit_file(p, "top.txt", "ours\n", "ours edits top only");
    repo
}

/// MG-03 G9 at the CLI: a gitlink buried two directories deep inside a subtree
/// that equals the base on OUR side — the walk would adopt `deps/` from theirs
/// without opening it — is still arbitration when theirs moved the pointer, and
/// is refused before anything is written, naming the nested path.
#[test]
fn merge_gitlink_nested_changed_pointer_inside_a_pruned_subtree_is_refused() {
    let repo = create_nested_gitlink_repo(GITLINK_MOVED);
    let p = repo.path();
    let head_before = head_commit(p);
    let index_before = std::fs::read(p.join(".libra/index")).expect("index bytes");
    let objects_before = count_loose_objects(p);

    let output = run_libra_command(&["merge", "feature"], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128), "refused: {stderr}");
    assert_eq!(report.error_code, "LBR-UNSUPPORTED-001");
    assert!(
        stderr.contains("deps/vendor/lib"),
        "the nested gitlink path is named: {stderr}"
    );
    assert_eq!(head_commit(p), head_before, "HEAD untouched");
    assert_eq!(
        std::fs::read(p.join(".libra/index")).expect("index bytes"),
        index_before,
        "index untouched"
    );
    assert_eq!(count_loose_objects(p), objects_before, "no objects written");
    assert!(!p.join("side.txt").exists(), "worktree untouched");
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "no merge state"
    );
}

/// MG-03 G10 at the CLI: the same nested gitlink, identical on all three sides,
/// passes through inside the pruned `deps/` subtree — the merge succeeds, the
/// result still carries the pointer, and the production read counter shows the
/// subtree was never opened (only the three distinct root trees, per pass).
#[test]
fn merge_gitlink_nested_identical_pointer_inside_a_pruned_subtree_passes_through() {
    let repo = create_nested_gitlink_repo(GITLINK_BASE);
    let p = repo.path();
    let stats_dir = tempfile::tempdir().expect("stats dir");
    let stats = stats_dir.path().join("stats.json");
    let stats_path = stats.to_string_lossy().to_string();

    let output = run_libra_command_with_stdin_and_env(
        &["--json", "merge", "feature"],
        p,
        "",
        &[
            ("LIBRA_TEST", "1"),
            ("LIBRA_TEST_MERGE_TREE_STATS", &stats_path),
        ],
    );
    assert_cli_success(&output, "merge passes the nested gitlink through");
    // `ls-tree` prints the Git mode; (`cat-file -p` renders a gitlink item's
    // mode through the enum's display, which is not the octal form.)
    let vendor = run_libra_command(&["ls-tree", "HEAD", "deps/vendor/"], p);
    assert_cli_success(&vendor, "list the merged deps/vendor tree");
    let listing = String::from_utf8_lossy(&vendor.stdout).to_string();
    assert!(
        listing.contains("160000") && listing.contains(GITLINK_BASE) && listing.contains("lib"),
        "the pointer survives inside the pruned subtree verbatim: {listing}"
    );
    assert!(p.join("side.txt").exists(), "theirs' file merged");
    assert_eq!(
        std::fs::read_to_string(p.join("top.txt")).expect("top"),
        "ours\n"
    );

    let recorded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&stats).expect("stats written")).expect("stats json");
    assert_eq!(recorded["walk"], "incremental");
    let reads = recorded["tree_reads"].as_u64().expect("tree_reads") as usize;
    // Three distinct roots per pass (base / ours / theirs all differ at the
    // root), two passes; `deps/` is identical everywhere and never opened.
    assert!(
        reads <= 2 * 3,
        "the subtree holding the gitlink is pruned, not opened: {reads} ({recorded})"
    );
}

/// MG-03: `--dry-run` reaches the same verdict as the real merge when a tree
/// the walk never opens is missing — the preview probes the carried trees
/// (read-only), so it fails exactly where the real merge's checkout would,
/// instead of reporting a clean preview for a merge that cannot complete.
#[test]
fn merge_tree_walk_dry_run_matches_the_real_verdict_on_a_missing_shared_tree() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::create_dir_all(p.join("shared/a/b")).expect("dirs");
    std::fs::write(p.join("shared/a/b/leaf.txt"), "0\n").expect("shared leaf");
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    assert_cli_success(&run_libra_command(&["add", "."], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    commit_file(p, "top.txt", "ours\n", "ours edit");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(p, "other.txt", "theirs\n", "theirs edit");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    let shared = run_libra_command(&["rev-parse", "HEAD:shared/a"], p);
    assert_cli_success(&shared, "resolve the shared nested tree");
    let shared_id = String::from_utf8_lossy(&shared.stdout).trim().to_string();
    let object = p
        .join(".libra")
        .join("objects")
        .join(&shared_id[..2])
        .join(&shared_id[2..]);
    std::fs::remove_file(&object).expect("simulate a missing shared tree");
    let head_before = head_commit(p);

    let preview = run_libra_command(&["merge", "--dry-run", "feature"], p);
    let stderr = String::from_utf8_lossy(&preview.stderr).to_string();
    assert!(
        !preview.status.success(),
        "the preview must not promise a merge that cannot complete: {stderr}"
    );
    assert!(
        stderr.contains(&shared_id),
        "the unreadable tree is named: {stderr}"
    );
    assert_eq!(head_commit(p), head_before, "a preview never moves HEAD");
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "no merge state"
    );
}

/// Shared shape for the MG-03 G6–G8 CLI carriers. The base carries a plain
/// `tool.sh`, a `target.txt` and a symlink `link -> target.txt`; `ours` edits
/// `top.txt`; `theirs` is built by PLUMBING (`update-index --cacheinfo` +
/// `write-tree` + `commit-tree`) so that a mode-only change can be expressed —
/// Libra's porcelain `add` does not detect a bare `chmod`. Runs the merge under
/// the requested walk and returns `(repo, ls-tree -r HEAD, ls-files -s)`.
#[cfg(unix)]
fn merge_mode_scenario(
    flat: bool,
    theirs_entries: &[(&str, &str)],
) -> (tempfile::TempDir, String, String) {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    std::fs::write(p.join("tool.sh"), "#!/bin/sh\necho hi\n").expect("tool");
    std::fs::write(p.join("target.txt"), "t\n").expect("target");
    std::os::unix::fs::symlink("target.txt", p.join("link")).expect("symlink");
    assert_cli_success(
        &run_libra_command(&["add", "top.txt", "tool.sh", "target.txt", "link"], p),
        "add",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    let base = head_commit(p);
    // theirs = base tree with the requested entries overridden.
    let mut stage: Vec<String> = vec!["update-index".to_string()];
    for (mode_and_path, content) in theirs_entries {
        let (mode, path) = mode_and_path.split_once(' ').expect("'<mode> <path>'");
        let blob = run_libra_command_with_stdin(&["hash-object", "-w", "--stdin"], p, content);
        assert!(blob.status.success(), "hash-object");
        stage.push("--cacheinfo".to_string());
        stage.push(format!("{mode},{},{path}", stdout_trimmed(&blob)));
    }
    let stage: Vec<&str> = stage.iter().map(String::as_str).collect();
    assert_cli_success(&run_libra_command(&stage, p), "stage theirs' entries");
    let tree = {
        let out = run_libra_command(&["write-tree"], p);
        assert_cli_success(&out, "write-tree");
        stdout_trimmed(&out)
    };
    let feature = {
        let out = run_libra_command(&["commit-tree", &tree, "-p", &base, "-m", "theirs"], p);
        assert_cli_success(&out, "commit-tree");
        stdout_trimmed(&out)
    };
    assert_cli_success(
        &run_libra_command(&["update-ref", "refs/heads/feature", &feature], p),
        "refs/heads/feature",
    );
    // Put main's index back to the base tree, then make ours' change.
    assert_cli_success(
        &run_libra_command(&["reset", "--hard", "HEAD"], p),
        "reset main's index to the base",
    );
    commit_file(p, "top.txt", "ours\n", "ours edit");
    let mut env: Vec<(&str, &str)> = vec![("LIBRA_TEST", "1")];
    if flat {
        env.push(("LIBRA_TEST_MERGE_TREE_WALK", "flat"));
    }
    let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", &env);
    assert_cli_success(&output, "merge");
    let tree_listing = run_libra_command(&["ls-tree", "-r", "HEAD"], p);
    assert_cli_success(&tree_listing, "ls-tree");
    let index_listing = run_libra_command(&["ls-files", "-s"], p);
    assert_cli_success(&index_listing, "ls-files -s");
    (
        repo,
        String::from_utf8_lossy(&tree_listing.stdout).to_string(),
        String::from_utf8_lossy(&index_listing.stdout).to_string(),
    )
}

/// What the merge checkout left in the working tree for a path: the file's
/// mode bits, or the symlink target — compared between the two walks.
#[cfg(unix)]
fn worktree_shape(p: &Path, path: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let full = p.join(path);
    match std::fs::read_link(&full) {
        Ok(target) => format!("link->{}", target.to_string_lossy()),
        Err(_) => format!(
            "mode={:o}",
            std::fs::metadata(&full).expect("meta").permissions().mode() & 0o777
        ),
    }
}

/// MG-03 G6 at the CLI: a mode-only change on theirs (`tool.sh` becomes
/// executable, content untouched) merges to a `100755` tree entry and index
/// entry on both walks, and the two walks leave the working tree in the same
/// state. (The merge checkout's working-tree materialization of mode bits and
/// symlink retargets is a pre-existing, walk-independent residual — see the
/// dev doc; `libra checkout` applies them, `merge`'s writer does not yet.)
#[cfg(unix)]
#[test]
fn merge_tree_walk_preserves_a_mode_only_change_on_both_walks() {
    let mut shapes = Vec::new();
    for flat in [false, true] {
        let (repo, tree, index) =
            merge_mode_scenario(flat, &[("100755 tool.sh", "#!/bin/sh\necho hi\n")]);
        assert!(
            tree.lines()
                .any(|l| l.starts_with("100755") && l.ends_with("\ttool.sh")),
            "flat={flat}: the executable bit is in the merged tree: {tree}"
        );
        assert!(
            index
                .lines()
                .any(|l| l.starts_with("100755") && l.ends_with("tool.sh")),
            "flat={flat}: the executable bit is in the merged index: {index}"
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("top.txt")).expect("top"),
            "ours\n"
        );
        shapes.push(worktree_shape(repo.path(), "tool.sh"));
    }
    assert_eq!(
        shapes[0], shapes[1],
        "both walks leave the same working tree"
    );
}

/// MG-03 G7 at the CLI: theirs re-points `link`; the merged tree and index keep
/// a `120000` entry with the new target's blob on both walks, and both walks
/// leave the same working tree.
#[cfg(unix)]
#[test]
fn merge_tree_walk_preserves_a_symlink_change_on_both_walks() {
    let mut shapes = Vec::new();
    for flat in [false, true] {
        let (repo, tree, index) = merge_mode_scenario(flat, &[("120000 link", "top.txt")]);
        let new_target_blob = {
            let out =
                run_libra_command_with_stdin(&["hash-object", "--stdin"], repo.path(), "top.txt");
            assert!(out.status.success(), "hash-object");
            stdout_trimmed(&out)
        };
        assert!(
            tree.lines().any(|l| l.starts_with("120000")
                && l.contains(&new_target_blob)
                && l.ends_with("\tlink")),
            "flat={flat}: the re-pointed symlink is in the merged tree: {tree}"
        );
        assert!(
            index
                .lines()
                .any(|l| l.starts_with("120000") && l.ends_with("link")),
            "flat={flat}: the symlink entry is in the merged index: {index}"
        );
        shapes.push(worktree_shape(repo.path(), "link"));
    }
    assert_eq!(
        shapes[0], shapes[1],
        "both walks leave the same working tree"
    );
}

/// MG-03 G8 at the CLI: an executable file ADDED by theirs arrives as `100755`
/// in the merged tree and index on both walks, with the same working tree.
#[cfg(unix)]
#[test]
fn merge_tree_walk_preserves_an_added_executable_on_both_walks() {
    let mut shapes = Vec::new();
    for flat in [false, true] {
        let (repo, tree, index) =
            merge_mode_scenario(flat, &[("100755 run.sh", "#!/bin/sh\nexit 0\n")]);
        assert!(
            tree.lines()
                .any(|l| l.starts_with("100755") && l.ends_with("\trun.sh")),
            "flat={flat}: the added executable keeps its mode in the tree: {tree}"
        );
        assert!(
            index
                .lines()
                .any(|l| l.starts_with("100755") && l.ends_with("run.sh")),
            "flat={flat}: …and in the index: {index}"
        );
        assert!(
            repo.path().join("run.sh").exists(),
            "flat={flat}: the file is checked out"
        );
        shapes.push(worktree_shape(repo.path(), "run.sh"));
    }
    assert_eq!(
        shapes[0], shapes[1],
        "both walks leave the same working tree"
    );
}

/// MG-03: a nested `refs/replace` whose replacement is MISSING makes the
/// checkout fail (it resolves replacements); the preview probe sees the trees
/// the way the checkout does, so `--dry-run` fails too — same verdict — and
/// the real merge fails before HEAD moves.
#[test]
fn merge_tree_walk_dry_run_follows_nested_replacements_like_the_checkout() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::create_dir_all(p.join("shared/a")).expect("dirs");
    std::fs::write(p.join("shared/a/leaf.txt"), "0\n").expect("leaf");
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    assert_cli_success(&run_libra_command(&["add", "."], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(&run_libra_command(&["branch", "alt"], p), "branch alt");
    commit_file(p, "top.txt", "ours\n", "ours edit");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(p, "other.txt", "theirs\n", "theirs edit");
    // A replacement tree for `shared/a`, then its object goes missing.
    assert_cli_success(&run_libra_command(&["checkout", "alt"], p), "co alt");
    commit_file(p, "shared/a/leaf.txt", "alt\n", "alt edit");
    let alt_sub = run_libra_command(&["rev-parse", "alt:shared/a"], p);
    assert_cli_success(&alt_sub, "alt nested tree");
    let alt_id = String::from_utf8_lossy(&alt_sub.stdout).trim().to_string();
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    let shared = run_libra_command(&["rev-parse", "HEAD:shared/a"], p);
    assert_cli_success(&shared, "shared nested tree");
    let shared_id = String::from_utf8_lossy(&shared.stdout).trim().to_string();
    assert_cli_success(
        &run_libra_command(&["replace", &shared_id, &alt_id], p),
        "replace the nested tree",
    );
    let object = p
        .join(".libra")
        .join("objects")
        .join(&alt_id[..2])
        .join(&alt_id[2..]);
    std::fs::remove_file(&object).expect("make the replacement dangle");
    let head_before = head_commit(p);

    let preview = run_libra_command(&["merge", "--dry-run", "feature"], p);
    assert!(
        !preview.status.success(),
        "the preview follows the replacement like the checkout and fails: {}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let real = run_libra_command(&["merge", "feature"], p);
    assert!(!real.status.success(), "the real merge fails at checkout");
    assert_eq!(head_commit(p), head_before, "HEAD never moved");
    assert!(
        !p.join(".libra").join("merge-state.json").exists(),
        "no merge state"
    );
}

/// MG-03: with a CONFLICT elsewhere, the real merge never checks out — it
/// writes conflict state through the raw tree view — so a dangling nested
/// `refs/replace` does not stop it. The preview mirrors that: it reports the
/// conflict (exit 1) instead of failing on the replacement the real merge
/// would never follow.
#[test]
fn merge_tree_walk_conflicted_dry_run_matches_the_real_conflict_path_under_a_dangling_replace() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::create_dir_all(p.join("shared/a")).expect("dirs");
    std::fs::write(p.join("shared/a/leaf.txt"), "0\n").expect("leaf");
    std::fs::write(p.join("top.txt"), "0\n").expect("top");
    assert_cli_success(&run_libra_command(&["add", "."], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
        "root",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(&run_libra_command(&["branch", "alt"], p), "branch alt");
    commit_file(p, "top.txt", "ours\n", "ours edit");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    commit_file(p, "top.txt", "theirs\n", "theirs conflicting edit");
    assert_cli_success(&run_libra_command(&["checkout", "alt"], p), "co alt");
    commit_file(p, "shared/a/leaf.txt", "alt\n", "alt edit");
    let alt_sub = run_libra_command(&["rev-parse", "alt:shared/a"], p);
    assert_cli_success(&alt_sub, "alt nested tree");
    let alt_id = String::from_utf8_lossy(&alt_sub.stdout).trim().to_string();
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    let shared = run_libra_command(&["rev-parse", "HEAD:shared/a"], p);
    assert_cli_success(&shared, "shared nested tree");
    let shared_id = String::from_utf8_lossy(&shared.stdout).trim().to_string();
    assert_cli_success(
        &run_libra_command(&["replace", &shared_id, &alt_id], p),
        "replace the nested tree",
    );
    let object = p
        .join(".libra")
        .join("objects")
        .join(&alt_id[..2])
        .join(&alt_id[2..]);
    std::fs::remove_file(&object).expect("make the replacement dangle");

    let preview = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_eq!(
        preview.status.code(),
        Some(1),
        "the preview reports the conflict, not the replacement: {}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let json = parse_json_stdout(&preview);
    assert_eq!(json["data"]["would_conflict"], true);

    let real = run_libra_command(&["merge", "feature"], p);
    assert_eq!(
        real.status.code(),
        Some(128),
        "the real merge writes conflict state: {}",
        String::from_utf8_lossy(&real.stderr)
    );
    assert!(
        p.join(".libra").join("merge-state.json").exists(),
        "conflict state written"
    );
    assert!(
        std::fs::read_to_string(p.join("top.txt"))
            .expect("top")
            .contains("<<<<<<<"),
        "the conflict is in the working tree"
    );
}

// ---------------------------------------------------------------------------
// MG-04: directory/file (D/F) collisions.
// ---------------------------------------------------------------------------

/// The D/F fixture: one side keeps (edits, or adds) a FILE `foo`, the other
/// side replaces it with a DIRECTORY `foo/` with content. `dir_on_theirs`
/// picks which side grows the directory; `file_in_base` says whether the
/// merge base already tracked the file (modify/delete + D/F, Git's stages 1+2)
/// or the file is a one-sided add (pure D/F, stage 2 only). Returns the repo
/// on `main` with `feature` ready to merge.
///
/// Libra's `checkout` cannot flip a path between file and directory yet
/// (pre-existing, registered in the dev doc), so every switch between the two
/// sides goes through the `root` branch, which tracks neither.
fn create_df_conflict_repo(dir_on_theirs: bool, file_in_base: bool) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "other.txt", "0\n", "root");
    assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
    if file_in_base {
        commit_file(p, "foo", "base file\n", "base with file foo");
    }
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    let make_dir = |p: &Path| {
        if file_in_base {
            assert_cli_success(&run_libra_command(&["rm", "foo"], p), "drop the file");
        }
        std::fs::create_dir_all(p.join("foo")).expect("mkdir foo");
        std::fs::write(p.join("foo/bar.txt"), "inside the directory\n").expect("bar");
        assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add dir");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "foo becomes a directory", "--no-verify"],
                p,
            ),
            "dir commit",
        );
    };
    let edit_file = |p: &Path| commit_file(p, "foo", "edited file\n", "foo edited");
    let switch = |p: &Path, branch: &str| {
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", branch], p), "checkout");
    };
    if dir_on_theirs {
        edit_file(p);
        switch(p, "feature");
        make_dir(p);
    } else {
        make_dir(p);
        switch(p, "feature");
        edit_file(p);
    }
    switch(p, "main");
    repo
}

/// Run a merge and require Libra's CONFLICT exit — not the 128 an I/O failure
/// also carries — returning the output for further assertions.
fn merge_expecting_conflict(p: &Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let output = run_libra_command_with_stdin_and_env(args, p, "", env);
    assert_eq!(
        output.status.code(),
        Some(128),
        "a D/F collision conflicts.\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-002", "{stderr}");
    output
}

fn index_stage_lines(p: &Path, path: &str) -> Vec<String> {
    let out = run_libra_command(&["ls-files", "-s"], p);
    assert_cli_success(&out, "ls-files -s");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.ends_with(&format!("\t{path}")))
        .map(|l| l.to_string())
        .collect()
}

/// G1 + G3 + G5 + G6: ours EDITED the file, theirs made the DIRECTORY. The
/// directory keeps `foo`; our file is moved to `foo~HEAD` and recorded there
/// as Git records it — the D/F relocation runs first, then the modify/delete
/// branch, so `foo~HEAD` carries the base on stage 1 and ours on stage 2
/// (git@3cb9185f6 merge-ort.c:4100-4198 then :4374; verified against
/// `git merge`: `100644 … 1 foo~HEAD` / `100644 … 2 foo~HEAD`), and the merge
/// prints Git's `CONFLICT (file/directory)` line.
#[test]
fn merge_df_conflict_file_on_ours_is_moved_to_head_suffix() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    let output = run_libra_command(&["merge", "feature"], p);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(128),
        "a D/F collision conflicts: {stderr}"
    );
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.contains(
            "CONFLICT (file/directory): directory in the way of foo from HEAD; moving it to foo~HEAD instead."
        ),
        "Git's message: {stdout}"
    );
    assert!(p.join("foo").is_dir(), "the directory keeps the path");
    assert_eq!(
        std::fs::read_to_string(p.join("foo/bar.txt")).expect("dir content"),
        "inside the directory\n"
    );
    assert!(
        stdout.contains(
            "CONFLICT (modify/delete): foo~HEAD deleted in feature and modified in HEAD.  Version HEAD of foo~HEAD left in tree."
        ),
        "Git's modify/delete line for the MOVED name: {stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo~HEAD")).expect("moved file"),
        "edited file\n",
        "our version is left verbatim at the unique path, as Git's line says"
    );
    assert!(!p.join("foo").is_file() && index_stage_lines(p, "foo").is_empty());
    let stages = index_stage_lines(p, "foo~HEAD");
    assert!(
        stages.iter().any(|l| l.contains(" 2\t")) && stages.iter().any(|l| l.contains(" 1\t")),
        "stage 2 (ours) and stage 1 (the base's file), nothing at stage 0/3: {stages:?}"
    );
    assert!(
        !stages
            .iter()
            .any(|l| l.contains(" 0\t") || l.contains(" 3\t")),
        "no stage 0/3 for the moved file: {stages:?}"
    );
    let state = read_merge_state(p);
    assert_eq!(
        state["conflicted_paths"],
        serde_json::json!(["foo~HEAD"]),
        "the unmerged path is the moved file"
    );
}

/// G2 + G4: ours made the DIRECTORY, theirs edited the FILE — the file is moved
/// to `foo~<branch>` on stage 3.
#[test]
fn merge_df_conflict_file_on_theirs_is_moved_to_branch_suffix() {
    let repo = create_df_conflict_repo(false, true);
    let p = repo.path();
    let output = merge_expecting_conflict(p, &["merge", "feature"], &[]);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.contains(
            "CONFLICT (file/directory): directory in the way of foo from feature; moving it to foo~feature instead."
        ),
        "{stdout}"
    );
    assert!(p.join("foo").is_dir());
    assert!(
        stdout.contains(
            "CONFLICT (modify/delete): foo~feature deleted in HEAD and modified in feature.  Version feature of foo~feature left in tree."
        ),
        "{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo~feature")).expect("moved file"),
        "edited file\n"
    );
    let stages = index_stage_lines(p, "foo~feature");
    assert!(
        stages.iter().any(|l| l.contains(" 3\t")) && stages.iter().any(|l| l.contains(" 1\t")),
        "stage 3 (theirs) and stage 1 (base): {stages:?}"
    );
    assert_eq!(
        read_merge_state(p)["conflicted_paths"],
        serde_json::json!(["foo~feature"])
    );
}

/// G7 + G8: `--abort` restores the pre-merge file `foo`, removes the directory
/// the merge created and the moved `foo~HEAD`, and leaves no merge state.
#[test]
fn merge_df_conflict_abort_restores_the_file_and_removes_the_moved_copy() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    let head_before = head_commit(p);
    merge_expecting_conflict(p, &["merge", "feature"], &[]);
    assert!(p.join("foo~HEAD").exists() && p.join("foo").is_dir());

    assert_cli_success(&run_libra_command(&["merge", "--abort"], p), "abort");
    assert_eq!(head_commit(p), head_before);
    assert!(p.join("foo").is_file(), "foo is a file again");
    assert_eq!(
        std::fs::read_to_string(p.join("foo")).expect("foo"),
        "edited file\n"
    );
    assert!(!p.join("foo~HEAD").exists(), "the moved copy is cleaned up");
    assert!(!p.join(".libra").join("merge-state.json").exists());
    let status = run_libra_command(&["status", "--short"], p);
    assert_cli_success(&status, "status");
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "clean after abort: {}",
        String::from_utf8_lossy(&status.stdout)
    );
}

/// The moved file resolves like any unmerged path: staging it lets
/// `--continue` finish, and the merge commit carries BOTH the directory and
/// the moved file.
#[test]
fn merge_df_conflict_continue_after_staging_the_moved_file() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    merge_expecting_conflict(p, &["merge", "feature"], &[]);
    assert_cli_success(
        &run_libra_command(&["add", "foo~HEAD"], p),
        "stage the moved file",
    );
    assert_cli_success(&run_libra_command(&["merge", "--continue"], p), "continue");
    let listing = run_libra_command(&["ls-tree", "-r", "HEAD"], p);
    assert_cli_success(&listing, "ls-tree");
    let listing = String::from_utf8_lossy(&listing.stdout).to_string();
    assert!(
        listing.contains("\tfoo/bar.txt") && listing.contains("\tfoo~HEAD"),
        "{listing}"
    );
}

/// Both walks reach the same D/F verdict (the collision pass is shared).
#[test]
fn merge_df_conflict_is_identical_on_the_flat_walk() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    // The complete `--dry-run` summaries agree, `files_changed` included
    // (Codex R3: the incremental count is adjusted when the post-pass moves a
    // kept file out of the result).
    let summaries: Vec<serde_json::Value> = [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ]
    .iter()
    .map(|env| {
        let out = run_libra_command_with_stdin_and_env(
            &["--json", "merge", "--dry-run", "feature"],
            p,
            "",
            env,
        );
        assert_eq!(out.status.code(), Some(1));
        parse_json_stdout(&out)["data"].clone()
    })
    .collect();
    assert_eq!(
        summaries[0], summaries[1],
        "both walks preview the same summary"
    );
    assert_eq!(
        summaries[0]["files_changed"], 2,
        "foo gone from the result + foo/bar.txt"
    );
    let output = merge_expecting_conflict(
        p,
        &["merge", "feature"],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")],
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("moving it to foo~HEAD instead."),
        "the flat walk prints the same line"
    );
    assert!(p.join("foo").is_dir() && p.join("foo~HEAD").is_file());
    let stages = index_stage_lines(p, "foo~HEAD");
    assert!(stages.iter().any(|l| l.contains(" 1	")) && stages.iter().any(|l| l.contains(" 2	")));
    assert_eq!(
        read_merge_state(p)["conflicted_paths"],
        serde_json::json!(["foo~HEAD"])
    );
}

/// Pure D/F (no file in the base): ours ADDED `foo`, theirs added `foo/`. Git
/// records only our side at `foo~HEAD` (stage 2, no stage 1) and prints just
/// the file/directory line — verified against `git merge`: `AU foo~HEAD`.
#[test]
fn merge_df_conflict_added_file_is_moved_with_only_its_own_stage() {
    let repo = create_df_conflict_repo(true, false);
    let p = repo.path();
    let output = merge_expecting_conflict(p, &["merge", "feature"], &[]);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.contains(
            "CONFLICT (file/directory): directory in the way of foo from HEAD; moving it to foo~HEAD instead."
        ),
        "{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo~HEAD")).expect("moved file"),
        "edited file\n",
        "a one-sided add is written verbatim"
    );
    let stages = index_stage_lines(p, "foo~HEAD");
    assert_eq!(stages.len(), 1, "{stages:?}");
    assert!(stages[0].contains(" 2\t"), "stage 2 only: {stages:?}");
    assert!(p.join("foo").is_dir());
}

/// An UNCHANGED file replaced by a directory on the other side is a clean
/// deletion, with no D/F message — verified against `git merge` on a divergent
/// history (`Merge made by the 'ort' strategy`, `delete mode 100644 foo`).
#[test]
fn merge_df_unchanged_file_replaced_by_a_directory_merges_cleanly() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "other.txt", "0\n", "root");
    assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
    commit_file(p, "foo", "base file\n", "base");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    commit_file(p, "other.txt", "ours\n", "unrelated");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    assert_cli_success(&run_libra_command(&["rm", "foo"], p), "rm");
    std::fs::create_dir_all(p.join("foo")).expect("dir");
    std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
    assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "dir", "--no-verify"], p),
        "dir",
    );
    // `checkout` cannot flip foo/ back into a file: go through the hub.
    assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    let out = run_libra_command(&["merge", "feature"], p);
    assert_cli_success(&out, "clean");
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("CONFLICT"),
        "no D/F message on a clean deletion"
    );
    assert!(p.join("foo").is_dir() && p.join("foo/bar.txt").is_file());
    assert!(index_stage_lines(p, "foo").is_empty());
}

/// A directory that merges to NOTHING is not in the way: the file stays put
/// (Git: "directory no longer in the way"). Ours deletes `foo/` entirely,
/// theirs adds file `foo` — a plain one-sided add, no conflict.
#[test]
fn merge_df_conflict_is_not_raised_when_the_directory_merges_to_nothing() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::create_dir_all(p.join("foo")).expect("dir");
    std::fs::write(p.join("foo/bar.txt"), "x\n").expect("bar");
    assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base with dir", "--no-verify"], p),
        "base",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    // ours: delete the directory
    std::fs::remove_dir_all(p.join("foo")).expect("rm dir");
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage removal");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "drop foo/", "--no-verify"], p),
        "ours",
    );
    // theirs: also delete the directory and add file foo
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    std::fs::remove_dir_all(p.join("foo")).expect("rm dir");
    std::fs::write(p.join("foo"), "now a file\n").expect("file");
    assert_cli_success(&run_libra_command(&["add", "-A", "."], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "foo is a file", "--no-verify"], p),
        "theirs",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    assert_cli_success(&run_libra_command(&["merge", "feature"], p), "clean merge");
    assert_eq!(
        std::fs::read_to_string(p.join("foo")).expect("foo"),
        "now a file\n"
    );
}

/// Codex MG-04 R1 (occupied name + nested directory), verified against
/// `git merge`: a tracked `foo~HEAD/` directory occupies the name, so the moved
/// file becomes `foo~HEAD_0`; the directory in the way is nested
/// (`foo/a/bar.txt`), and both `--restart` and `--abort` must put the file
/// `foo` back where `foo/a/` stood — the emptied directories are pruned rather
/// than left to block the write.
#[test]
fn merge_df_conflict_occupied_name_and_nested_directory_restart_and_abort() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "other.txt", "0\n", "root");
    assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
    std::fs::create_dir_all(p.join("foo~HEAD")).expect("dir");
    std::fs::write(p.join("foo~HEAD/bar.txt"), "taken\n").expect("taken");
    std::fs::write(p.join("foo"), "base file\n").expect("foo");
    assert_cli_success(
        &run_libra_command(&["add", "foo", "foo~HEAD/bar.txt"], p),
        "add",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "base",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    commit_file(p, "foo", "edited file\n", "ours edit");
    assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    assert_cli_success(&run_libra_command(&["rm", "foo"], p), "rm");
    std::fs::create_dir_all(p.join("foo/a")).expect("nested");
    std::fs::write(p.join("foo/a/bar.txt"), "nested\n").expect("nested file");
    assert_cli_success(
        &run_libra_command(&["add", "foo/a/bar.txt"], p),
        "add nested",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "nested dir", "--no-verify"], p),
        "dir",
    );
    assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    let head_before = head_commit(p);

    let out = merge_expecting_conflict(p, &["merge", "feature"], &[]);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains(
            "CONFLICT (file/directory): directory in the way of foo from HEAD; moving it to foo~HEAD_0 instead."
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "CONFLICT (modify/delete): foo~HEAD_0 deleted in feature and modified in HEAD.  Version HEAD of foo~HEAD_0 left in tree."
        ),
        "{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo~HEAD_0")).expect("moved"),
        "edited file\n"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo~HEAD/bar.txt")).expect("occupying dir"),
        "taken\n"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo/a/bar.txt")).expect("nested"),
        "nested\n"
    );
    let stages = index_stage_lines(p, "foo~HEAD_0");
    assert!(
        stages.iter().any(|l| l.contains(" 1\t")) && stages.iter().any(|l| l.contains(" 2\t")),
        "{stages:?}"
    );
    assert_eq!(
        read_merge_state(p)["conflicted_paths"],
        serde_json::json!(["foo~HEAD_0"])
    );

    // `--restart` re-runs from a restored tree and hits the same collision.
    let restart = merge_expecting_conflict(p, &["merge", "--restart"], &[]);
    assert!(String::from_utf8_lossy(&restart.stdout).contains("moving it to foo~HEAD_0 instead."));
    assert!(p.join("foo~HEAD_0").is_file() && p.join("foo/a/bar.txt").is_file());
    assert_eq!(
        read_merge_state(p)["conflicted_paths"],
        serde_json::json!(["foo~HEAD_0"])
    );

    assert_cli_success(&run_libra_command(&["merge", "--abort"], p), "abort");
    assert_eq!(head_commit(p), head_before);
    assert!(
        p.join("foo").is_file(),
        "foo is the file again (foo/a/ was pruned)"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo")).expect("foo"),
        "edited file\n"
    );
    assert!(!p.join("foo~HEAD_0").exists());
    assert_eq!(
        std::fs::read_to_string(p.join("foo~HEAD/bar.txt")).expect("kept"),
        "taken\n"
    );
    let status = run_libra_command(&["status", "--short"], p);
    assert_cli_success(&status, "status");
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "{}",
        String::from_utf8_lossy(&status.stdout)
    );
}

/// Codex MG-04 R1: under `--json` the D/F announcement must not reach stdout —
/// the conflict travels in the error envelope on stderr, and the merge state is
/// still written.
#[test]
fn merge_df_conflict_json_keeps_stdout_machine_clean() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    let output = merge_expecting_conflict(p, &["--json", "merge", "feature"], &[]);
    assert!(
        String::from_utf8_lossy(&output.stdout).trim().is_empty(),
        "stdout must stay machine-clean: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(p.join("foo~HEAD").is_file() && p.join("foo").is_dir());
    assert_eq!(
        read_merge_state(p)["conflicted_paths"],
        serde_json::json!(["foo~HEAD"])
    );
}

/// MG-04, verified against real `git merge` (git@3cb9185f6 + `git mktree`):
/// a directory holding nothing but an EMPTY tree is "in the way" of the file
/// at the same path only when the merge base had nothing there.
///
/// * base has `foo` as a file and ours edits it → Git traverses the new
///   directory, finds no file, and reports a plain
///   `CONFLICT (modify/delete): foo` with stages 1 + 2 at `foo` itself;
/// * base has nothing at `foo` and ours adds it → Git defers the new
///   directory and adopts its tree verbatim, so the file moves to `foo~HEAD`.
///
/// Both walks must agree on both shapes.
#[test]
fn merge_df_conflict_empty_subtree_follows_gits_base_presence_rule() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        // (1) the base tracks the file: no relocation.
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        commit_file(p, "foo", "base file\n", "base with file foo");
        let theirs = craft_commit_with_empty_dir(p, &head_commit(p), true, Some("bar"));
        assert_cli_success(
            &run_libra_command(&["update-ref", "refs/heads/feature", &theirs], p),
            "refs/heads/feature",
        );
        commit_file(p, "foo", "edited file\n", "ours edits foo");

        let output = merge_expecting_conflict(p, &["merge", "feature"], env);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        assert!(
            !stdout.contains("file/directory"),
            "walk {env:?}: an empty-only subtree the base already had is not in the way: {stdout}"
        );
        assert!(
            !p.join("foo~HEAD").exists(),
            "walk {env:?}: nothing was moved"
        );
        let stages = index_stage_lines(p, "foo");
        assert!(
            stages.iter().any(|l| l.contains(" 1\t")) && stages.iter().any(|l| l.contains(" 2\t")),
            "walk {env:?}: the modify/delete stays at `foo`: {stages:?}"
        );
        assert_eq!(
            read_merge_state(p)["conflicted_paths"],
            serde_json::json!(["foo"])
        );

        // (2) the base has nothing at `foo`: Git relocates the added file.
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        let theirs = craft_commit_with_empty_dir(p, &head_commit(p), false, Some("bar"));
        assert_cli_success(
            &run_libra_command(&["update-ref", "refs/heads/feature", &theirs], p),
            "refs/heads/feature",
        );
        commit_file(p, "foo", "added file\n", "ours adds foo");

        let output = merge_expecting_conflict(p, &["merge", "feature"], env);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        assert!(
            stdout.contains(
                "CONFLICT (file/directory): directory in the way of foo from HEAD; moving it to foo~HEAD instead."
            ),
            "walk {env:?}: {stdout}"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("foo~HEAD")).expect("moved file"),
            "added file\n"
        );
        let stages = index_stage_lines(p, "foo~HEAD");
        assert_eq!(stages.len(), 1, "walk {env:?}: stage 2 only: {stages:?}");
        assert!(stages[0].contains(" 2\t"), "walk {env:?}: {stages:?}");
        assert!(index_stage_lines(p, "foo").is_empty(), "walk {env:?}");
    }
}

/// Codex MG-04 R2, verified against `git merge`: a `foo~HEAD` only the merge
/// base had — deleted on both sides, absent from the result — still occupies
/// its name (Git's `unique_path` consults every input path), so the moved file
/// becomes `foo~HEAD_0`. Both walks.
#[test]
fn merge_df_conflict_deleted_input_path_still_occupies_its_name() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        std::fs::write(p.join("foo"), "base file\n").expect("foo");
        std::fs::write(p.join("foo~HEAD"), "gone\n").expect("foo~HEAD");
        assert_cli_success(&run_libra_command(&["add", "foo", "foo~HEAD"], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        assert_cli_success(
            &run_libra_command(&["rm", "foo~HEAD"], p),
            "ours drops foo~HEAD",
        );
        commit_file(p, "foo", "edited file\n", "ours edit + drop");
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        assert_cli_success(
            &run_libra_command(&["rm", "foo", "foo~HEAD"], p),
            "theirs drops both",
        );
        std::fs::create_dir_all(p.join("foo")).expect("dir");
        std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
        assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add dir");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "dir", "--no-verify"], p),
            "dir",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        // An UNTRACKED `foo~HEAD` recreated since is not the merge's to delete
        // (Codex R3): only paths tracked NOW and absent from the result go.
        std::fs::write(p.join("foo~HEAD"), "untracked\n").expect("untracked");

        let output = merge_expecting_conflict(p, &["merge", "feature"], env);
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("moving it to foo~HEAD_0 instead."),
            "walk {env:?}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(
            std::fs::read_to_string(p.join("foo~HEAD_0")).expect("moved"),
            "edited file\n"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("foo~HEAD")).expect("untracked survives"),
            "untracked\n",
            "walk {env:?}: a path the merge never tracked is left alone"
        );
        assert_eq!(
            read_merge_state(p)["conflicted_paths"],
            serde_json::json!(["foo~HEAD_0"])
        );
    }
}

/// Codex MG-04 R2, verified against `git merge -X theirs` / `-X ours`: a
/// strategy option settles content hunks, not a modify/delete under a
/// directory — the edited file still moves to `foo~HEAD` with stages 1 + 2
/// and both lines, whichever side `-X` favours.
#[test]
fn merge_df_conflict_strategy_option_keeps_the_modify_delete_conflict() {
    for favour in ["theirs", "ours"] {
        let repo = create_df_conflict_repo(true, true);
        let p = repo.path();
        let output = merge_expecting_conflict(p, &["merge", "-X", favour, "feature"], &[]);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        assert!(
            stdout.contains("moving it to foo~HEAD instead.")
                && stdout.contains(
                    "CONFLICT (modify/delete): foo~HEAD deleted in feature and modified in HEAD."
                ),
            "-X {favour}: {stdout}"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("foo~HEAD")).expect("moved file"),
            "edited file\n",
            "-X {favour}: the edited version is kept, not discarded"
        );
        let stages = index_stage_lines(p, "foo~HEAD");
        assert!(
            stages.iter().any(|l| l.contains(" 1\t")) && stages.iter().any(|l| l.contains(" 2\t")),
            "-X {favour}: {stages:?}"
        );
        assert!(p.join("foo").is_dir());
    }
}

/// Codex MG-04 R2: an IGNORED symlink sitting where the moved file goes is
/// invisible to the untracked scan; the write must replace the link itself,
/// never follow it out of the working tree.
#[cfg(unix)]
#[test]
fn merge_df_conflict_replaces_an_ignored_symlink_at_the_moved_name() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    let outside = tempfile::tempdir().expect("outside");
    std::fs::write(outside.path().join("target.txt"), "outside\n").expect("target");
    // The ignore rule is committed on main (the fixture tracks `.libraignore`,
    // so a bare edit would count as a dirty worktree).
    std::fs::write(p.join(".libraignore"), "foo~HEAD\n").expect("ignore");
    assert_cli_success(
        &run_libra_command(&["add", ".libraignore"], p),
        "add ignore",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ignore foo~HEAD", "--no-verify"], p),
        "commit ignore",
    );
    std::os::unix::fs::symlink(outside.path().join("target.txt"), p.join("foo~HEAD"))
        .expect("symlink");

    merge_expecting_conflict(p, &["merge", "feature"], &[]);
    let meta = std::fs::symlink_metadata(p.join("foo~HEAD")).expect("moved file");
    assert!(
        meta.file_type().is_file(),
        "the link was replaced by the file"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo~HEAD")).expect("moved file"),
        "edited file\n"
    );
    assert_eq!(
        std::fs::read_to_string(outside.path().join("target.txt")).expect("outside"),
        "outside\n",
        "nothing was written through the link"
    );
}

/// Codex MG-04 R2: a merge never writes THROUGH a symlinked directory (Git:
/// "beyond a symbolic link"). An ignored `foo -> <outside>` would redirect
/// `foo/bar.txt`; the merge is refused before HEAD, index or the outside
/// directory change — on both walks.
#[cfg(unix)]
#[test]
fn merge_refuses_to_write_through_an_ignored_symlinked_directory() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        commit_file(p, "other.txt", "ours\n", "unrelated");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        std::fs::create_dir_all(p.join("foo")).expect("dir");
        std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
        assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "dir", "--no-verify"], p),
            "dir",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(p.join(".libraignore"), "foo\n").expect("ignore");
        assert_cli_success(
            &run_libra_command(&["add", ".libraignore"], p),
            "add ignore",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "ignore foo", "--no-verify"], p),
            "commit ignore",
        );
        std::os::unix::fs::symlink(outside.path(), p.join("foo")).expect("symlink");
        let head_before = head_commit(p);

        let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert!(
            !output.status.success(),
            "walk {env:?}: the merge must be refused"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("symbolic link"), "walk {env:?}: {stderr}");
        assert_eq!(head_commit(p), head_before, "walk {env:?}: HEAD untouched");
        assert!(
            !outside.path().join("bar.txt").exists(),
            "walk {env:?}: nothing was written outside the working tree"
        );
        assert!(
            index_stage_lines(p, "foo/bar.txt").is_empty(),
            "walk {env:?}: index untouched"
        );
        assert!(!p.join(".libra").join("merge-state.json").exists());
    }
}

/// Build a commit whose root tree holds an EMPTY directory `foo` next to the
/// blobs of `parent`'s tree — a shape no working tree can produce, crafted
/// through the plumbing. Returns the commit id.
fn craft_commit_with_empty_dir(
    p: &Path,
    parent: &str,
    drop_foo_blob: bool,
    nested: Option<&str>,
) -> String {
    let raw = |hex: &str| -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
            .collect()
    };
    let hash_tree = |bytes: &[u8], name: &str| -> String {
        let path = p.join(name);
        std::fs::write(&path, bytes).expect("tree bytes");
        let out = run_libra_command(&["hash-object", "-t", "tree", "-w", "--literally", name], p);
        assert_cli_success(&out, "hash-object tree");
        std::fs::remove_file(&path).expect("cleanup");
        stdout_trimmed(&out)
    };
    let empty = hash_tree(b"", ".empty-tree");
    // The parent's leaves (blobs at the root only in these fixtures).
    let listing = run_libra_command(&["ls-tree", parent], p);
    assert_cli_success(&listing, "ls-tree");
    let mode_of = |meta: &str| meta.split_whitespace().next().expect("mode").to_string();
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for line in String::from_utf8_lossy(&listing.stdout).lines() {
        let (meta, name) = line.split_once('\t').expect("ls-tree line");
        if name == "foo" {
            // The crafted tree provides `foo` itself (as the directory below);
            // whatever the parent had there — a blob or a directory — is
            // replaced.
            assert!(
                drop_foo_blob || mode_of(meta) == "040000",
                "`foo` is replaced"
            );
            continue;
        }
        let mut parts = meta.split_whitespace();
        let mode = parts.next().expect("mode");
        let _kind = parts.next();
        let id = parts.next().expect("id");
        assert_ne!(mode, "040000", "fixture roots hold blobs beside `foo` only");
        let mut entry = format!("{} {name}\0", mode.trim_start_matches('0')).into_bytes();
        entry.extend(raw(id));
        entries.push((name.to_string(), entry));
    }
    // `nested`: `foo/` holds one EMPTY subtree under that name (a directory
    // that contributes no file); `None` makes `foo` itself the empty tree.
    let foo_tree = if let Some(name) = nested {
        let mut child = format!("40000 {name}\0").into_bytes();
        child.extend(raw(&empty));
        hash_tree(&child, ".foo-tree")
    } else {
        empty.clone()
    };
    let mut foo_entry = b"40000 foo\0".to_vec();
    foo_entry.extend(raw(&foo_tree));
    // Git orders tree entries by name, a directory as `name/`.
    entries.push(("foo/".to_string(), foo_entry));
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let root: Vec<u8> = entries.into_iter().flat_map(|(_, bytes)| bytes).collect();
    let root = hash_tree(&root, ".root-tree");
    let out = run_libra_command(
        &[
            "commit-tree",
            &root,
            "-p",
            parent,
            "-m",
            "crafted empty dir",
        ],
        p,
    );
    assert_cli_success(&out, "commit-tree");
    stdout_trimmed(&out)
}

/// Codex MG-04 R3: an empty `foo/` in the BASE next to two different files
/// `foo` added on either side is an add/add conflict — stages 2 and 3, no
/// stage 1 (the base's directory marker is not a file) — on both walks.
#[test]
fn merge_add_add_conflict_over_an_empty_base_directory_has_no_stage_1() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        let base = craft_commit_with_empty_dir(p, &head_commit(p), false, None);
        for branch in ["main", "feature"] {
            assert_cli_success(
                &run_libra_command(&["update-ref", &format!("refs/heads/{branch}"), &base], p),
                "branch at the crafted base",
            );
        }
        commit_file(p, "foo", "a\n", "ours adds foo");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        commit_file(p, "foo", "b\n", "theirs adds foo");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        merge_expecting_conflict(p, &["merge", "feature"], env);
        let stages = index_stage_lines(p, "foo");
        assert!(
            stages.iter().any(|l| l.contains(" 2\t")) && stages.iter().any(|l| l.contains(" 3\t")),
            "walk {env:?}: {stages:?}"
        );
        assert!(
            !stages.iter().any(|l| l.contains(" 1\t")),
            "walk {env:?}: the base's empty directory is not a stage: {stages:?}"
        );
        assert_eq!(
            read_merge_state(p)["conflicted_paths"],
            serde_json::json!(["foo"])
        );
    }
}

/// Codex MG-04 R3 (P2): an empty base directory turning into a file on one
/// side is one added file — both walks count it, and the merge is clean.
#[test]
fn merge_empty_directory_turning_into_a_file_counts_one_change() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        let base = craft_commit_with_empty_dir(p, &head_commit(p), false, None);
        for branch in ["main", "feature"] {
            assert_cli_success(
                &run_libra_command(&["update-ref", &format!("refs/heads/{branch}"), &base], p),
                "branch at the crafted base",
            );
        }
        commit_file(p, "other.txt", "ours\n", "unrelated");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        commit_file(p, "foo", "now a file\n", "theirs adds foo");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let preview = run_libra_command_with_stdin_and_env(
            &["--json", "merge", "--dry-run", "feature"],
            p,
            "",
            env,
        );
        assert_cli_success(&preview, "dry-run");
        let data = parse_json_stdout(&preview)["data"].clone();
        assert_eq!(data["files_changed"], 1, "walk {env:?}: {data}");
        assert!(data["would_conflict"].is_null());
        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge");
        assert_eq!(
            std::fs::read_to_string(p.join("foo")).expect("foo"),
            "now a file\n"
        );
    }
}

/// Codex MG-04 R3: a TRACKED symlink `foo` on ours giving way to theirs'
/// directory `foo/` is a legitimate transition — the link is one of the paths
/// the merge removes, so the write of `foo/bar.txt` is allowed, and the link
/// moves to `foo~HEAD` (stage 2, mode 120000) like any D/F file. Both walks.
#[cfg(unix)]
#[test]
fn merge_df_conflict_tracked_symlink_gives_way_to_a_directory() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        std::os::unix::fs::symlink("other.txt", p.join("foo")).expect("symlink");
        assert_cli_success(&run_libra_command(&["add", "foo"], p), "add link");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "ours: symlink foo", "--no-verify"], p),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        std::fs::create_dir_all(p.join("foo")).expect("dir");
        std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
        assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add dir");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "theirs: dir foo", "--no-verify"], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let output = merge_expecting_conflict(p, &["merge", "feature"], env);
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("moving it to foo~HEAD instead."),
            "walk {env:?}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            p.join("foo").is_dir() && p.join("foo/bar.txt").is_file(),
            "walk {env:?}"
        );
        let stages = index_stage_lines(p, "foo~HEAD");
        assert_eq!(stages.len(), 1, "walk {env:?}: {stages:?}");
        assert!(
            stages[0].starts_with("120000") && stages[0].contains(" 2\t"),
            "walk {env:?}: the link itself is what moved: {stages:?}"
        );
        // Git checks out a real link at the moved name, not its target text.
        let moved = std::fs::symlink_metadata(p.join("foo~HEAD")).expect("moved link");
        assert!(
            moved.file_type().is_symlink(),
            "walk {env:?}: a symlink moved as a symlink"
        );
        assert_eq!(
            std::fs::read_link(p.join("foo~HEAD")).expect("link target"),
            std::path::PathBuf::from("other.txt"),
            "walk {env:?}"
        );
    }
}

/// Codex MG-04 R3: a tracked `gone/file` the merge removes, while the working
/// tree's `gone` is an IGNORED symlink to a directory outside the repository:
/// unlinking through the link would delete an external file, so the merge is
/// refused before anything changes — both walks.
#[cfg(unix)]
#[test]
fn merge_refuses_to_unlink_a_tracked_file_through_an_ignored_symlinked_directory() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        std::fs::write(p.join("other.txt"), "0\n").expect("other");
        std::fs::create_dir_all(p.join("gone")).expect("dir");
        std::fs::write(p.join("gone/file"), "keep\n").expect("file");
        assert_cli_success(
            &run_libra_command(&["add", "other.txt", "gone/file"], p),
            "add",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "root", "--no-verify"], p),
            "root",
        );
        std::fs::write(p.join(".libraignore"), "gone\n").expect("ignore");
        assert_cli_success(
            &run_libra_command(&["add", ".libraignore"], p),
            "add ignore",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "ignore gone", "--no-verify"], p),
            "ignore",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        commit_file(p, "other.txt", "ours\n", "unrelated");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        assert_cli_success(
            &run_libra_command(&["rm", "gone/file"], p),
            "theirs drops it",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "drop gone/file", "--no-verify"], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("file"), "keep\n").expect("external file");
        std::fs::remove_dir_all(p.join("gone")).expect("drop the real dir");
        std::os::unix::fs::symlink(outside.path(), p.join("gone")).expect("symlink");
        let head_before = head_commit(p);

        let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert!(!output.status.success(), "walk {env:?}: refused");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("symbolic link"), "walk {env:?}: {stderr}");
        assert_eq!(head_commit(p), head_before, "walk {env:?}: HEAD untouched");
        assert_eq!(
            std::fs::read_to_string(outside.path().join("file")).expect("external file"),
            "keep\n",
            "walk {env:?}: nothing outside the working tree was unlinked"
        );
        assert!(!p.join(".libra").join("merge-state.json").exists());
    }
}

/// Codex MG-04 R3: a hook that plants an IGNORED symlink where the merge will
/// write is caught by the post-hook traversal recheck — before HEAD moves on
/// the flat walk, and inherently before HEAD on the incremental walk. Both
/// hook checkpoints, both walks.
#[cfg(unix)]
#[test]
fn merge_refuses_a_hook_planted_ignored_symlink_before_head_moves() {
    use std::os::unix::fs::PermissionsExt;
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        for hook_name in ["pre-merge-commit", "commit-msg"] {
            let repo = create_committed_repo_via_cli();
            let p = repo.path();
            commit_file(p, "top.txt", "0\n", "root");
            assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
            // The ignore rule is ours only, so theirs can still stage the path.
            std::fs::write(p.join(".libraignore"), "newdir\n").expect("ignore");
            assert_cli_success(
                &run_libra_command(&["add", ".libraignore"], p),
                "add ignore",
            );
            assert_cli_success(
                &run_libra_command(&["commit", "-m", "ignore newdir", "--no-verify"], p),
                "ignore",
            );
            commit_file(p, "top.txt", "ours\n", "ours edit");
            assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
            commit_file(
                p,
                "newdir/sub/leaf.txt",
                "theirs\n",
                "theirs adds a subtree",
            );
            assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
            let outside = tempfile::tempdir().expect("outside");
            let hooks = p.join(".libra").join("hooks");
            std::fs::create_dir_all(&hooks).expect("hooks dir");
            let hook = hooks.join(hook_name);
            std::fs::write(
                &hook,
                format!(
                    "#!/bin/sh\nln -s '{}' \"$LIBRA_WORK_TREE/newdir\"\n",
                    outside.path().display()
                ),
            )
            .expect("write hook");
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).expect("chmod");
            let head_before = head_commit(p);

            let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
            assert!(
                !output.status.success(),
                "walk {env:?} hook {hook_name}: refused"
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("symbolic link"),
                "walk {env:?} hook {hook_name}: {stderr}"
            );
            assert_eq!(
                head_commit(p),
                head_before,
                "walk {env:?} hook {hook_name}: HEAD"
            );
            assert!(
                !outside.path().join("sub").exists(),
                "walk {env:?} hook {hook_name}: nothing written outside"
            );
            assert!(!p.join(".libra").join("merge-state.json").exists());
        }
    }
}

/// Codex MG-04 pre-review: a tracked DANGLING symlink `foo` giving way to
/// theirs' directory `foo/` must merge cleanly — the removal test may not
/// follow the link (`exists()` does), or the write of `foo/bar.txt` fails
/// after the flat engine has already moved HEAD. Verified against
/// `git merge`: `delete mode 120000 foo` / `create mode 100644 foo/bar.txt`.
#[cfg(unix)]
#[test]
fn merge_dangling_tracked_symlink_gives_way_to_a_directory_cleanly() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        // The hub must predate both shapes: `checkout` cannot flip a path
        // between a directory and a file/symlink (registered residual).
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        std::os::unix::fs::symlink("does-not-exist", p.join("foo")).expect("dangling link");
        assert_cli_success(&run_libra_command(&["add", "foo"], p), "add link");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base: dangling link", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        commit_file(p, "other.txt", "ours\n", "ours unrelated");
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        assert_cli_success(
            &run_libra_command(&["rm", "foo"], p),
            "theirs drops the link",
        );
        std::fs::create_dir_all(p.join("foo")).expect("dir");
        std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
        assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add dir");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "theirs: dir foo", "--no-verify"], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge");
        assert_eq!(
            std::fs::read_to_string(p.join("foo/bar.txt")).expect("dir content"),
            "bar\n",
            "walk {env:?}"
        );
        assert!(
            std::fs::symlink_metadata(p.join("foo"))
                .expect("foo")
                .is_dir(),
            "walk {env:?}: the dangling link gave way"
        );
        let status = run_libra_command(&["status", "--short"], p);
        assert_cli_success(&status, "status");
        assert!(
            String::from_utf8_lossy(&status.stdout).trim().is_empty(),
            "walk {env:?}: HEAD, index and worktree agree: {}",
            String::from_utf8_lossy(&status.stdout)
        );
    }
}

/// Codex MG-04 pre-review: when the merge is REFUSED at the moved name (an
/// untracked `foo~HEAD` would be overwritten) nothing was moved, so — as in
/// Git — no `CONFLICT (file/directory)` line is printed and no merge state is
/// left behind.
#[test]
fn merge_df_conflict_refused_at_the_moved_name_announces_nothing() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    let head_before = head_commit(p);
    std::fs::write(p.join("foo~HEAD"), "untracked\n").expect("untracked");

    let output = run_libra_command(&["merge", "feature"], p);
    assert!(!output.status.success(), "refused");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        !stdout.contains("CONFLICT"),
        "nothing was moved, so nothing is announced: {stdout}"
    );
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-002", "{stderr}");
    assert!(
        stderr.contains("foo~HEAD"),
        "the collision is named: {stderr}"
    );
    assert_eq!(head_commit(p), head_before);
    assert_eq!(
        std::fs::read_to_string(p.join("foo~HEAD")).expect("untracked survives"),
        "untracked\n"
    );
    assert!(p.join("foo").is_file(), "our file is untouched");
    assert!(!p.join(".libra").join("merge-state.json").exists());
}

/// Codex MG-04 pre-review: a directory that must become a FILE while IGNORED
/// content lives inside it is refused BEFORE the merge mutates anything —
/// the takeover only ever removes this merge's own tracked files.
#[test]
fn merge_refuses_to_replace_a_directory_holding_ignored_content() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        std::fs::write(p.join(".libraignore"), "foo/keep.log\n").expect("ignore");
        std::fs::create_dir_all(p.join("foo")).expect("dir");
        std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
        assert_cli_success(
            &run_libra_command(&["add", ".libraignore", "foo/bar.txt"], p),
            "add",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base: dir foo", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        commit_file(p, "other.txt", "ours\n", "ours unrelated");
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        assert_cli_success(
            &run_libra_command(&["rm", "foo/bar.txt"], p),
            "drop the dir",
        );
        std::fs::write(p.join("foo"), "now a file\n").expect("file");
        assert_cli_success(&run_libra_command(&["add", "foo"], p), "add file");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "theirs: foo is a file", "--no-verify"], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        // An ignored build artefact inside the directory the merge must replace.
        std::fs::write(p.join("foo/keep.log"), "ignored\n").expect("ignored file");
        let head_before = head_commit(p);

        let output = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert!(!output.status.success(), "walk {env:?}: refused");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("foo/keep.log"),
            "walk {env:?}: the blocker is named: {stderr}"
        );
        assert_eq!(head_commit(p), head_before, "walk {env:?}: HEAD untouched");
        assert_eq!(
            std::fs::read_to_string(p.join("foo/keep.log")).expect("ignored file survives"),
            "ignored\n",
            "walk {env:?}"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("foo/bar.txt")).expect("tracked file survives"),
            "bar\n",
            "walk {env:?}: refused before any removal"
        );
    }
}

/// Codex MG-04 pre-review: with more than one conflict the unmerged paths,
/// the announcement and the `--dry-run` report follow path order, as Git's
/// sorted `process_entries` output does — the relocated path included.
#[test]
fn merge_df_conflict_paths_are_reported_in_path_order() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    // A second, ordinary content conflict on a path sorting BEFORE `foo`.
    // (Every switch goes through the `root` hub: `checkout` cannot flip `foo`
    // between a file and a directory.)
    commit_file(p, "a-shared.txt", "ours\n", "ours edits a-shared");
    assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    commit_file(p, "a-shared.txt", "theirs\n", "theirs edits a-shared");
    assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

    let preview = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_eq!(preview.status.code(), Some(1));
    let json = parse_json_stdout(&preview);
    assert_eq!(
        json["data"]["conflicted_paths"],
        serde_json::json!(["a-shared.txt", "foo~HEAD"]),
        "{json}"
    );
    merge_expecting_conflict(p, &["merge", "feature"], &[]);
    assert_eq!(
        read_merge_state(p)["conflicted_paths"],
        serde_json::json!(["a-shared.txt", "foo~HEAD"])
    );
}

/// Codex MG-04 R4: a tracked symlink whose target is not valid UTF-8 relocates
/// byte-for-byte (link targets are raw bytes), and an EMPTY directory standing
/// at the moved name is taken over rather than failing after the merge state
/// was written. Both walks.
#[cfg(unix)]
#[test]
fn merge_df_conflict_moves_a_non_utf8_symlink_over_an_empty_directory() {
    use std::os::unix::ffi::OsStrExt;
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        let target = std::ffi::OsStr::from_bytes(&[0xff, 0xfe, b'A']);
        std::os::unix::fs::symlink(target, p.join("foo")).expect("weird link");
        assert_cli_success(&run_libra_command(&["add", "foo"], p), "add link");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base: non-utf8 link", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        commit_file(p, "other.txt", "ours\n", "ours unrelated");
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        assert_cli_success(
            &run_libra_command(&["rm", "foo"], p),
            "theirs drops the link",
        );
        std::fs::create_dir_all(p.join("foo")).expect("dir");
        std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
        assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add dir");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "theirs: dir foo", "--no-verify"], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        // Ours also EDITED the link, so it must move instead of being deleted.
        std::fs::remove_file(p.join("foo")).expect("drop the link");
        let edited = std::ffi::OsStr::from_bytes(&[0xff, 0xfe, b'B']);
        std::os::unix::fs::symlink(edited, p.join("foo")).expect("edited link");
        assert_cli_success(&run_libra_command(&["add", "foo"], p), "stage the edit");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "ours: retarget the link", "--no-verify"],
                p,
            ),
            "ours",
        );
        // An EMPTY directory already sits where the link will move.
        std::fs::create_dir_all(p.join("foo~HEAD")).expect("empty dir at the moved name");

        let output = merge_expecting_conflict(p, &["merge", "feature"], env);
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("moving it to foo~HEAD instead."),
            "walk {env:?}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let moved = std::fs::read_link(p.join("foo~HEAD")).expect("moved link");
        assert_eq!(
            moved.as_os_str().as_bytes(),
            &[0xff, 0xfe, b'B'],
            "walk {env:?}: the raw target survives"
        );
        let stages = index_stage_lines(p, "foo~HEAD");
        assert!(
            stages.iter().any(|line| line.starts_with("120000")),
            "walk {env:?}: {stages:?}"
        );
    }
}

/// Codex MG-04 R4, verified against `git merge`: an IGNORED file standing at
/// the moved name is expendable — Git replaces it (only *untracked,
/// non-ignored* files refuse the merge), and Libra follows.
#[test]
fn merge_df_conflict_replaces_an_ignored_file_at_the_moved_name_like_git() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    std::fs::write(p.join(".libraignore"), "foo~HEAD\n").expect("ignore");
    assert_cli_success(
        &run_libra_command(&["add", ".libraignore"], p),
        "add ignore",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ignore the moved name", "--no-verify"], p),
        "commit ignore",
    );
    std::fs::write(p.join("foo~HEAD"), "expendable\n").expect("ignored file");

    let output = merge_expecting_conflict(p, &["merge", "feature"], &[]);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("moving it to foo~HEAD instead."),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        std::fs::read_to_string(p.join("foo~HEAD")).expect("moved file"),
        "edited file\n",
        "as in Git, an ignored file at the moved name is replaced"
    );
}

/// Codex MG-04 R5: `--abort` removes a moved DANGLING symlink. The restored
/// index does not track the moved name, so nothing else would ever clean it
/// up, and `is_file()` follows the link and reports false for a dangling one.
#[cfg(unix)]
#[test]
fn merge_df_conflict_abort_removes_a_moved_dangling_symlink() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
        std::os::unix::fs::symlink("does-not-exist", p.join("foo")).expect("dangling link");
        assert_cli_success(&run_libra_command(&["add", "foo"], p), "add link");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base: dangling link", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        // Ours retargets the link, so it must MOVE rather than be deleted.
        std::fs::remove_file(p.join("foo")).expect("drop");
        std::os::unix::fs::symlink("still-missing", p.join("foo")).expect("retarget");
        assert_cli_success(&run_libra_command(&["add", "foo"], p), "stage retarget");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "ours: retarget", "--no-verify"], p),
            "ours",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        assert_cli_success(
            &run_libra_command(&["rm", "foo"], p),
            "theirs drops the link",
        );
        std::fs::create_dir_all(p.join("foo")).expect("dir");
        std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
        assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add dir");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "theirs: dir foo", "--no-verify"], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        let head_before = head_commit(p);

        merge_expecting_conflict(p, &["merge", "feature"], env);
        assert!(
            std::fs::symlink_metadata(p.join("foo~HEAD"))
                .expect("moved link")
                .file_type()
                .is_symlink(),
            "walk {env:?}"
        );
        assert_cli_success(&run_libra_command(&["merge", "--abort"], p), "abort");
        assert_eq!(head_commit(p), head_before, "walk {env:?}");
        assert!(
            std::fs::symlink_metadata(p.join("foo~HEAD")).is_err(),
            "walk {env:?}: the moved dangling link is cleaned up"
        );
        let status = run_libra_command(&["status", "--short"], p);
        assert_cli_success(&status, "status");
        assert!(
            String::from_utf8_lossy(&status.stdout).trim().is_empty(),
            "walk {env:?}: clean after abort: {}",
            String::from_utf8_lossy(&status.stdout)
        );
    }
}

/// Codex MG-04 R5, verified against `git merge`: rewriting a tracked file
/// REPLACES the directory entry (a new inode), so a hard link to the old
/// content elsewhere keeps its own bytes instead of being truncated in place.
#[cfg(unix)]
#[test]
fn merge_replaces_the_directory_entry_of_a_rewritten_file() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "tracked.txt", "base\n", "base");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    commit_file(p, "other.txt", "ours\n", "ours unrelated");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    commit_file(p, "tracked.txt", "theirs\n", "theirs rewrites it");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    let alias_dir = tempfile::tempdir().expect("alias dir");
    let alias = alias_dir.path().join("alias.txt");
    std::fs::hard_link(p.join("tracked.txt"), &alias).expect("hard link");

    assert_cli_success(&run_libra_command(&["merge", "feature"], p), "clean merge");
    assert_eq!(
        std::fs::read_to_string(p.join("tracked.txt")).expect("merged file"),
        "theirs\n"
    );
    assert_eq!(
        std::fs::read_to_string(&alias).expect("alias"),
        "base\n",
        "the merge replaced the entry instead of truncating the shared inode"
    );
}

/// Codex MG-04 R6: the flat engine's own flattener (which keeps empty
/// subtrees as markers) must report a missing nested tree as repository
/// corruption instead of panicking, and refuse before HEAD moves.
#[test]
fn merge_flat_walk_reports_a_missing_nested_tree_instead_of_panicking() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "top.txt", "0\n", "root");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
    commit_file(p, "ours.txt", "ours\n", "ours edit");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
    commit_file(
        p,
        "newdir/sub/leaf.txt",
        "theirs\n",
        "theirs adds a subtree",
    );
    let nested = run_libra_command(&["rev-parse", "HEAD:newdir/sub"], p);
    assert_cli_success(&nested, "rev-parse the nested tree");
    let nested_id = stdout_trimmed(&nested);
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    let object = p
        .join(".libra")
        .join("objects")
        .join(&nested_id[..2])
        .join(&nested_id[2..]);
    assert!(object.exists(), "loose object: {}", object.display());
    std::fs::remove_file(&object).expect("simulate corruption");
    let head_before = head_commit(p);

    let output = run_libra_command_with_stdin_and_env(
        &["merge", "feature"],
        p,
        "",
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")],
    );
    assert!(!output.status.success(), "the merge is refused");
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(
        report.error_code, "LBR-REPO-002",
        "a missing tree is repository corruption, not a panic: {stderr}"
    );
    assert!(
        stderr.contains(&nested_id),
        "the unreadable tree is named: {stderr}"
    );
    assert_eq!(head_commit(p), head_before, "HEAD never moved");
    assert!(!p.join("newdir").exists());
}

/// Codex MG-04 R7: every merge write carries the entry's TYPE and MODE — a
/// tracked symlink stays a symlink and an executable keeps `0755` — on the
/// clean path, through `--abort`, and on both walks. (This also closes the
/// residual MG-03 registered: merge's checkout writer used to leave the
/// executable bit and symlink targets to whatever was already on disk.)
#[cfg(unix)]
#[test]
fn merge_preserves_symlinks_and_executable_bits_when_it_writes() {
    use std::os::unix::fs::PermissionsExt;
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "target.txt", "target\n", "root");
        std::fs::write(p.join("tool.sh"), "#!/bin/sh\necho ours\n").expect("tool");
        std::fs::set_permissions(p.join("tool.sh"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        std::os::unix::fs::symlink("target.txt", p.join("link")).expect("symlink");
        assert_cli_success(&run_libra_command(&["add", "tool.sh", "link"], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base: link + script", "--no-verify"], p),
            "base",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        commit_file(p, "ours.txt", "ours\n", "ours unrelated");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        // Theirs rewrites the script (same mode) so the merge must write it.
        std::fs::write(p.join("tool.sh"), "#!/bin/sh\necho theirs\n").expect("tool");
        assert_cli_success(&run_libra_command(&["add", "tool.sh"], p), "add tool");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "theirs rewrites the script", "--no-verify"],
                p,
            ),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        // A mode-only change is invisible to `status` (a pre-existing gap), so
        // the merge still runs — and its write must restore `0755` from the
        // entry rather than inherit whatever is on disk.
        std::fs::set_permissions(p.join("tool.sh"), std::fs::Permissions::from_mode(0o600))
            .expect("chmod down");

        assert_cli_success(
            &run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env),
            "clean merge",
        );
        let script = std::fs::metadata(p.join("tool.sh")).expect("script");
        assert_eq!(
            script.permissions().mode() & 0o777,
            0o755,
            "walk {env:?}: the executable bit is written, not inherited"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("tool.sh")).expect("script"),
            "#!/bin/sh\necho theirs\n"
        );
        let link = std::fs::symlink_metadata(p.join("link")).expect("link");
        assert!(
            link.file_type().is_symlink(),
            "walk {env:?}: a tracked symlink is written as a symlink"
        );
        assert_eq!(
            std::fs::read_link(p.join("link")).expect("target"),
            std::path::PathBuf::from("target.txt"),
            "walk {env:?}"
        );
        let listing = run_libra_command(&["ls-files", "-s"], p);
        assert_cli_success(&listing, "ls-files");
        let listing = String::from_utf8_lossy(&listing.stdout).to_string();
        assert!(
            listing.contains("100755") && listing.contains("120000"),
            "walk {env:?}: the index keeps both modes: {listing}"
        );
    }
}

/// Codex MG-04 R8: when the directory is NOT in the way (an empty-only
/// subtree the base already had) the surviving file must not end up beside a
/// leftover subtree entry — the result tree would carry a blob and a directory
/// under one name. Theirs REPLACES the base's empty `foo/bar` with an equally
/// empty `foo/baz`, so the incremental walk records a fresh subtree beneath
/// the file ours keeps. Both walks must produce the same tree: only `foo`.
#[test]
fn merge_keeps_only_the_file_when_an_empty_subtree_is_not_in_the_way() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        let root = head_commit(p);
        // base: `foo/` holding one empty subtree; ours: a file at `foo`.
        let base = craft_commit_with_empty_dir(p, &root, false, Some("bar"));
        for branch in ["main", "feature"] {
            assert_cli_success(
                &run_libra_command(&["update-ref", &format!("refs/heads/{branch}"), &base], p),
                "branch at the crafted base",
            );
        }
        commit_file(p, "foo", "ours file\n", "ours puts a file at foo");
        // theirs: the same shape with a DIFFERENT empty subtree name.
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        let theirs = craft_commit_with_empty_dir(p, &base, false, Some("baz"));
        assert_cli_success(
            &run_libra_command(&["update-ref", "refs/heads/feature", &theirs], p),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");

        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge");
        let listing = run_libra_command(&["ls-tree", "-r", "-t", "HEAD"], p);
        assert_cli_success(&listing, "ls-tree");
        let listing = String::from_utf8_lossy(&listing.stdout).to_string();
        let foo_entries: Vec<&str> = listing
            .lines()
            .filter(|line| line.ends_with("\tfoo") || line.contains("\tfoo/"))
            .collect();
        assert_eq!(
            foo_entries.len(),
            1,
            "walk {env:?}: exactly one entry at `foo`, and it is the file: {foo_entries:?}"
        );
        assert!(
            foo_entries[0].starts_with("100644") && foo_entries[0].ends_with("\tfoo"),
            "walk {env:?}: {foo_entries:?}"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("foo")).expect("foo"),
            "ours file\n",
            "walk {env:?}"
        );
    }
}

/// Codex MG-04 R9, verified against `git merge`: an IGNORED regular file
/// standing where a merged directory must go is replaced by that directory
/// (git: `create mode 100644 foo/bar.txt`, `foo` becomes a directory). Without
/// clearing it, `create_dir_all` fails mid-write — and on the flat engine that
/// happens after HEAD has already moved.
#[test]
fn merge_replaces_an_ignored_file_standing_where_a_directory_must_go() {
    for env in [
        &[][..],
        &[("LIBRA_TEST", "1"), ("LIBRA_TEST_MERGE_TREE_WALK", "flat")][..],
    ] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        commit_file(p, "other.txt", "0\n", "root");
        std::fs::write(p.join(".libraignore"), "foo\n").expect("ignore");
        assert_cli_success(
            &run_libra_command(&["add", ".libraignore"], p),
            "add ignore",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "ignore foo", "--no-verify"], p),
            "ignore",
        );
        assert_cli_success(&run_libra_command(&["branch", "feature"], p), "feature");
        commit_file(p, "other.txt", "ours\n", "ours unrelated");
        assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "feature");
        std::fs::create_dir_all(p.join("foo")).expect("dir");
        std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
        // A tracked SYMLINK beneath the same directory: its writer takes the
        // other branch and needs the same ancestor rule (Codex R10).
        #[cfg(unix)]
        std::os::unix::fs::symlink("bar.txt", p.join("foo/link")).expect("link");
        #[cfg(unix)]
        let staged = ["add", "-f", "foo/bar.txt", "foo/link"];
        #[cfg(not(unix))]
        let staged = ["add", "-f", "foo/bar.txt"];
        assert_cli_success(&run_libra_command(&staged, p), "add the ignored path");
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "theirs adds foo/bar.txt", "--no-verify"],
                p,
            ),
            "theirs",
        );
        assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
        // `main` never had `foo/`, so checkout already removed it; put an
        // IGNORED file exactly where the merge must create the directory.
        let _ = std::fs::remove_dir_all(p.join("foo"));
        std::fs::write(p.join("foo"), "expendable\n").expect("ignored file in the way");
        let head_before = head_commit(p);

        let out = run_libra_command_with_stdin_and_env(&["merge", "feature"], p, "", env);
        assert_cli_success(&out, "clean merge");
        assert_ne!(
            head_commit(p),
            head_before,
            "walk {env:?}: the merge completed"
        );
        assert!(
            p.join("foo").is_dir(),
            "walk {env:?}: the ignored file gave way to the directory"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("foo/bar.txt")).expect("merged file"),
            "bar\n",
            "walk {env:?}"
        );
        let status = run_libra_command(&["status", "--short"], p);
        assert_cli_success(&status, "status");
        assert!(
            String::from_utf8_lossy(&status.stdout).trim().is_empty(),
            "walk {env:?}: HEAD, index and worktree agree: {}",
            String::from_utf8_lossy(&status.stdout)
        );
    }
}

/// G9 + G10: `--dry-run` reports a D/F collision as a conflict (exit 1) and the
/// JSON names its category, the path the file would move to, and the original.
#[test]
fn merge_dry_run_reports_a_df_conflict_with_its_kind() {
    let repo = create_df_conflict_repo(true, true);
    let p = repo.path();
    let head_before = head_commit(p);
    let out = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_eq!(out.status.code(), Some(1), "would-conflict preview exits 1");
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["dry_run"], true);
    assert_eq!(json["data"]["would_conflict"], true);
    assert_eq!(
        json["data"]["conflicted_paths"],
        serde_json::json!(["foo~HEAD"])
    );
    assert_eq!(
        json["data"]["conflict_kinds"],
        serde_json::json!([{"path": "foo~HEAD", "kind": "modify-delete", "original_path": "foo"}]),
        "an edited file under a directory is Git's modify/delete at the moved path: {json}"
    );
    assert_eq!(head_commit(p), head_before);
    assert!(!p.join("foo~HEAD").exists(), "a preview writes nothing");
    assert!(p.join("foo").is_file(), "the working tree is untouched");
    assert!(!p.join(".libra").join("merge-state.json").exists());

    // A one-sided add under a directory is the pure file/directory kind.
    let repo = create_df_conflict_repo(true, false);
    let p = repo.path();
    let out = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_eq!(out.status.code(), Some(1));
    let json = parse_json_stdout(&out);
    assert_eq!(
        json["data"]["conflict_kinds"],
        serde_json::json!([{"path": "foo~HEAD", "kind": "file-directory", "original_path": "foo"}]),
        "{json}"
    );
    assert!(!p.join("foo~HEAD").exists() && p.join("foo").is_file());
}

/// The category field also distinguishes ordinary conflicts.
#[test]
fn merge_dry_run_reports_content_conflict_kind() {
    let temp_repo = create_diverged_repo_for_conflict();
    let p = temp_repo.path();
    let out = run_libra_command(&["--json", "merge", "--dry-run", "feature"], p);
    assert_eq!(out.status.code(), Some(1));
    let json = parse_json_stdout(&out);
    assert_eq!(
        json["data"]["conflict_kinds"],
        serde_json::json!([{"path": "shared.txt", "kind": "content"}]),
        "{json}"
    );
}

/// A D/F collision INSIDE the recursive fold (criss-cross whose two bases
/// disagree about `foo` being a file or a directory) is settled the way Git
/// settles it at `call_depth > 0`: the file moves to
/// `foo~Temporary merge branch N` in the virtual ancestor, so the outer merge
/// sees an ancestor that never had a file at `foo` — our re-created `foo`
/// survives as a one-sided add instead of being "deleted by theirs".
#[test]
fn merge_crisscross_df_collision_inside_the_fold_moves_the_file_like_git() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file(p, "base.txt", "root\n", "root");
    // Codex R3: names only the ROOT had — deleted by both folded sides — still
    // occupy the fold's temporary-branch names (Git's `unique_path` consults
    // every input), so the relocated file must take `…_0` in the ancestor.
    // Both labels are planted because the fold's ours/theirs order follows the
    // bases' ids.
    for label in ["1", "2"] {
        std::fs::write(
            p.join(format!("foo~Temporary merge branch {label}")),
            "gone\n",
        )
        .expect("planted name");
    }
    assert_cli_success(
        &run_libra_command(
            &[
                "add",
                "foo~Temporary merge branch 1",
                "foo~Temporary merge branch 2",
            ],
            p,
        ),
        "add planted",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "plant temporary names", "--no-verify"], p),
        "plant",
    );
    assert_cli_success(&run_libra_command(&["branch", "root"], p), "root hub");
    // a: file `foo`; b: directory `foo/`. (Switching between them goes through
    // `root`: `checkout` cannot flip a path between file and directory yet.)
    assert_cli_success(&run_libra_command(&["checkout", "-b", "a"], p), "a");
    assert_cli_success(
        &run_libra_command(
            &[
                "rm",
                "foo~Temporary merge branch 1",
                "foo~Temporary merge branch 2",
            ],
            p,
        ),
        "a drops the planted names",
    );
    commit_file(p, "foo", "file\n", "a: file foo");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "main");
    assert_cli_success(&run_libra_command(&["checkout", "-b", "b"], p), "b");
    assert_cli_success(
        &run_libra_command(
            &[
                "rm",
                "foo~Temporary merge branch 1",
                "foo~Temporary merge branch 2",
            ],
            p,
        ),
        "b drops the planted names",
    );
    std::fs::create_dir_all(p.join("foo")).expect("dir");
    std::fs::write(p.join("foo/bar.txt"), "bar\n").expect("bar");
    assert_cli_success(&run_libra_command(&["add", "foo/bar.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "b: dir foo", "--no-verify"], p),
        "b commit",
    );
    // Criss-cross: x = a + b (D/F resolved by keeping foo~HEAD), y = b + a.
    for (from, tip, other, moved) in [("a", "x", "b", "foo~HEAD"), ("b", "y", "a", "foo~a")] {
        assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
        assert_cli_success(&run_libra_command(&["checkout", from], p), "from");
        assert_cli_success(&run_libra_command(&["checkout", "-b", tip], p), "tip");
        // The criss-cross merges themselves hit the D/F collision.
        merge_expecting_conflict(p, &["merge", other], &[]);
        assert_cli_success(&run_libra_command(&["add", moved], p), "stage moved");
        assert_cli_success(&run_libra_command(&["merge", "--continue"], p), "continue");
    }
    // x': drop the directory and put the file back at `foo`.
    assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
    assert_cli_success(&run_libra_command(&["checkout", "x"], p), "x");
    assert_cli_success(&run_libra_command(&["rm", "foo/bar.txt"], p), "rm dir file");
    assert_cli_success(&run_libra_command(&["rm", "foo~HEAD"], p), "rm moved");
    let _ = std::fs::remove_dir(p.join("foo"));
    std::fs::write(p.join("foo"), "file\n").expect("file again");
    // x' also re-adds the planted names: against a correct ancestor (which
    // holds `…_0`, not these names) they are one-sided adds; against a wrong
    // one they would be modify/delete conflicts.
    for label in ["1", "2"] {
        std::fs::write(p.join(format!("foo~Temporary merge branch {label}")), "x\n")
            .expect("re-add");
    }
    assert_cli_success(
        &run_libra_command(
            &[
                "add",
                "foo",
                "foo~Temporary merge branch 1",
                "foo~Temporary merge branch 2",
            ],
            p,
        ),
        "stage",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "x: file foo again", "--no-verify"], p),
        "x commit",
    );
    // y': an unrelated edit so y is not an ancestor of x.
    assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
    assert_cli_success(&run_libra_command(&["checkout", "y"], p), "y");
    commit_file(p, "base.txt", "y\n", "y edit");
    assert_cli_success(&run_libra_command(&["checkout", "root"], p), "via root");
    assert_cli_success(&run_libra_command(&["checkout", "x"], p), "x");

    let out = run_libra_command(&["merge", "y"], p);
    assert_cli_success(&out, "the outer merge is clean");
    assert_eq!(
        std::fs::read_to_string(p.join("foo")).expect("foo"),
        "file\n",
        "the file we re-created is a one-sided add against the virtual ancestor"
    );
    assert!(!p.join("foo").is_dir());
    assert_eq!(
        std::fs::read_to_string(p.join("foo~a")).expect("foo~a"),
        "file\n"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("base.txt")).expect("base"),
        "y\n"
    );
    for label in ["1", "2"] {
        assert_eq!(
            std::fs::read_to_string(p.join(format!("foo~Temporary merge branch {label}")))
                .expect("re-added name"),
            "x\n",
            "the fold took `…_0` for its relocation, leaving this name to x'"
        );
    }
    let parents = run_libra_command(&["cat-file", "-p", "HEAD"], p);
    assert_cli_success(&parents, "cat-file");
    assert_eq!(
        String::from_utf8_lossy(&parents.stdout)
            .lines()
            .filter(|l| l.starts_with("parent "))
            .count(),
        2
    );
}
