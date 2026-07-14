//! Tests pull command integration that combines fetch with merge or rebase behaviors.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use git_internal::internal::object::commit::Commit;
use libra::{command::load_object, internal::head::Head, utils::test::ChangeDirGuard};
use serial_test::serial;
use tempfile::{TempDir, tempdir};

use super::{
    assert_cli_success, configure_identity_via_cli, create_committed_repo_via_cli,
    init_repo_via_cli, parse_cli_error_stderr, parse_json_stdout, run_libra_command,
};

fn git(args: &[&str], cwd: &Path) {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("failed to execute git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(args: &[&str], cwd: &Path) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("failed to execute git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output should be utf8")
        .trim()
        .to_string()
}

fn create_remote_fixture() -> (TempDir, PathBuf, PathBuf, String) {
    let temp_root = tempdir().expect("failed to create temp root");
    let remote_dir = temp_root.path().join("remote.git");
    let work_dir = temp_root.path().join("workdir");

    git(
        &["init", "--bare", remote_dir.to_str().unwrap()],
        temp_root.path(),
    );
    git(&["init", work_dir.to_str().unwrap()], temp_root.path());
    git(&["config", "user.name", "Libra Tester"], &work_dir);
    git(&["config", "user.email", "tester@example.com"], &work_dir);

    fs::write(work_dir.join("README.md"), "hello libra\n").expect("failed to write README");
    git(&["add", "README.md"], &work_dir);
    git(&["commit", "-m", "initial commit"], &work_dir);

    let branch = git_stdout(&["rev-parse", "--abbrev-ref", "HEAD"], &work_dir);
    git(
        &["remote", "add", "origin", remote_dir.to_str().unwrap()],
        &work_dir,
    );
    git(
        &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
        &work_dir,
    );

    (temp_root, remote_dir, work_dir, branch)
}

fn push_remote_commit(
    work_dir: &Path,
    branch: &str,
    file: &str,
    content: &str,
    message: &str,
) -> String {
    fs::write(work_dir.join(file), content).expect("failed to write remote file");
    git(&["add", file], work_dir);
    git(&["commit", "-m", message], work_dir);
    git(
        &["push", "origin", &format!("HEAD:refs/heads/{branch}")],
        work_dir,
    );
    git_stdout(&["rev-parse", "HEAD"], work_dir)
}

fn configure_pull_tracking(repo: &Path, remote_dir: &Path, branch: &str) {
    let remote_output = run_libra_command(
        &["remote", "add", "origin", remote_dir.to_str().unwrap()],
        repo,
    );
    assert_cli_success(&remote_output, "remote add");

    let branch_remote = run_libra_command(&["config", "branch.main.remote", "origin"], repo);
    assert_cli_success(&branch_remote, "set branch.main.remote");

    let merge_ref = format!("refs/heads/{branch}");
    let branch_merge = run_libra_command(&["config", "branch.main.merge", &merge_ref], repo);
    assert_cli_success(&branch_merge, "set branch.main.merge");
}

fn parse_json_stderr(stderr: &[u8]) -> serde_json::Value {
    serde_json::from_str(String::from_utf8_lossy(stderr).trim())
        .expect("stderr should contain a JSON error report")
}

#[test]
#[serial]
fn test_pull_cli_without_tracking_returns_repo_exit_code() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["pull"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-003");
    assert!(
        stderr.starts_with(concat!(
            "There is no tracking information for the current branch.\n",
            "Please specify which branch you want to merge with.\n",
            "See git-pull(1) for details.\n\n",
            "    libra pull <remote> <branch>\n\n",
            "If you wish to set tracking information for this branch you can do so with:\n\n",
            "    libra branch --set-upstream-to=<remote>/<branch> main",
        )),
        "pull without tracking should match git-style advice: {stderr}"
    );
    assert!(
        !stderr.starts_with("error:"),
        "git-style pull advice is unprefixed: {stderr}"
    );
}

#[test]
#[serial]
fn test_pull_cli_without_tracking_uses_single_remote_in_advice() {
    let repo = create_committed_repo_via_cli();
    let remote = tempdir().expect("failed to create remote root");

    let remote_output = run_libra_command(
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
        repo.path(),
    );
    assert_cli_success(&remote_output, "remote add");

    let output = run_libra_command(&["pull"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-003");
    assert!(
        stderr.contains("    libra branch --set-upstream-to=origin/<branch> main"),
        "single remote should appear in git-style set-upstream advice: {stderr}"
    );
}

#[test]
#[serial]
fn test_pull_cli_remote_not_found_returns_cli_exit_code() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["pull", "origin", "main"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(stderr.contains("remote 'origin' not found"));
}

#[test]
fn test_pull_ff_only_conflicts_with_rebase_at_parse_time() {
    let repo = tempdir().expect("failed to create local repo");

    let output = run_libra_command(&["pull", "--ff-only", "--rebase"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        stderr.contains("cannot be used with")
            && stderr.contains("--ff-only")
            && stderr.contains("--rebase"),
        "pull should reject conflicting integration modes before repo preflight: {stderr}"
    );
}

#[test]
fn test_pull_no_rebase_countermands_rebase_at_parse_time() {
    let repo = tempdir().expect("failed to create local repo");

    // `--rebase --no-rebase` (last wins) is NOT a clap conflict: `--no-rebase`
    // countermands `--rebase` via the symmetric override, so it parses and
    // fails later at remote/tracking resolution, not at clap.
    let output = run_libra_command(&["pull", "--rebase", "--no-rebase"], repo.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("cannot be used with") && !stderr.contains("unexpected argument"),
        "pull --rebase --no-rebase parses (override, no conflict): {stderr}"
    );

    // `--no-rebase` is compatible with merge options like `--no-ff` (unlike
    // `--rebase`, which conflicts with them).
    let merge_opts = run_libra_command(&["pull", "--no-ff", "--no-rebase"], repo.path());
    let merge_stderr = String::from_utf8_lossy(&merge_opts.stderr);
    assert!(
        !merge_stderr.contains("cannot be used with"),
        "pull --no-ff --no-rebase parses (merge path): {merge_stderr}"
    );
}

#[tokio::test]
#[serial]
async fn test_pull_fast_forward_updates_head_from_tracking_remote() {
    let (_temp_root, remote_dir, work_dir, branch) = create_remote_fixture();
    let remote_head = git_stdout(&["rev-parse", "HEAD"], &work_dir);

    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);

    let output = run_libra_command(&["pull"], local_repo.path());
    assert_cli_success(&output, "pull fast-forward");

    let _guard = ChangeDirGuard::new(local_repo.path());
    let head = Head::current_commit()
        .await
        .expect("pull should update HEAD to the fetched commit");
    assert_eq!(head.to_string(), remote_head);
    assert!(
        local_repo.path().join("README.md").exists(),
        "pull should restore the fetched worktree"
    );
}

#[tokio::test]
#[serial]
async fn test_pull_ff_only_fast_forward_updates_head_from_tracking_remote() {
    let (_temp_root, remote_dir, work_dir, branch) = create_remote_fixture();

    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);

    let first_pull = run_libra_command(&["pull"], local_repo.path());
    assert_cli_success(&first_pull, "initial pull");
    let new_head = push_remote_commit(
        &work_dir,
        &branch,
        "remote.txt",
        "remote change\n",
        "remote update",
    );

    let output = run_libra_command(&["pull", "--ff-only"], local_repo.path());
    assert_cli_success(&output, "pull --ff-only fast-forward");

    let _guard = ChangeDirGuard::new(local_repo.path());
    let head = Head::current_commit()
        .await
        .expect("pull --ff-only should update HEAD to the fetched commit");
    assert_eq!(head.to_string(), new_head);
    assert!(
        local_repo.path().join("remote.txt").exists(),
        "pull --ff-only should restore the fetched worktree"
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_pull_fast_forward_skips_untracked_artifacts_during_restore() {
    use std::os::unix::fs::PermissionsExt;

    let (_temp_root, remote_dir, work_dir, branch) = create_remote_fixture();

    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);

    let first_pull = run_libra_command(&["pull"], local_repo.path());
    assert_cli_success(&first_pull, "initial pull");

    fs::write(local_repo.path().join(".libraignore"), "target/\n")
        .expect("failed to write ignore file");
    let artifact_dir = local_repo.path().join("target/deep");
    fs::create_dir_all(&artifact_dir).expect("failed to create artifact dir");
    fs::write(artifact_dir.join("artifact.bin"), b"untracked build output")
        .expect("failed to write artifact");
    fs::set_permissions(&artifact_dir, fs::Permissions::from_mode(0o000))
        .expect("failed to lock artifact dir");

    let new_head = push_remote_commit(
        &work_dir,
        &branch,
        "remote.txt",
        "remote change\n",
        "remote update",
    );
    let output = run_libra_command(&["pull", "--ff-only"], local_repo.path());

    fs::set_permissions(&artifact_dir, fs::Permissions::from_mode(0o700))
        .expect("failed to unlock artifact dir");

    assert_cli_success(&output, "pull should not scan untracked build artifacts");

    let _guard = ChangeDirGuard::new(local_repo.path());
    let head = Head::current_commit()
        .await
        .expect("pull should update HEAD to the fetched commit");
    assert_eq!(head.to_string(), new_head);
    assert!(
        local_repo.path().join("remote.txt").exists(),
        "pull should restore tracked files from the fetched commit"
    );
    assert!(
        artifact_dir.join("artifact.bin").exists(),
        "pull should leave untracked artifacts alone"
    );
}

#[tokio::test]
#[serial]
async fn test_pull_diverged_remote_creates_three_way_merge() {
    let (_temp_root, remote_dir, work_dir, branch) = create_remote_fixture();

    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);

    let first_pull = run_libra_command(&["pull"], local_repo.path());
    assert_cli_success(&first_pull, "initial pull");

    let _remote_head = push_remote_commit(
        &work_dir,
        &branch,
        "remote.txt",
        "remote change\n",
        "remote update",
    );

    fs::write(local_repo.path().join("local.txt"), "local change\n").expect("write local change");
    let add = run_libra_command(&["add", "local.txt"], local_repo.path());
    assert_cli_success(&add, "stage local change");
    let commit = run_libra_command(
        &["commit", "-m", "local update", "--no-verify"],
        local_repo.path(),
    );
    assert_cli_success(&commit, "commit local change");

    let output = run_libra_command(&["pull"], local_repo.path());
    assert_cli_success(&output, "pull three-way merge");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Merge made by the 'three-way' strategy."),
        "pull should report three-way strategy, stdout: {stdout}"
    );

    let _guard = ChangeDirGuard::new(local_repo.path());
    let head = Head::current_commit()
        .await
        .expect("pull should create a merge commit");
    let commit: Commit = load_object(&head).expect("load pull merge commit");
    assert_eq!(commit.parent_commit_ids.len(), 2);
    assert!(
        commit.message.starts_with('\n'),
        "pull merge commit body must retain Git's blank-line separator before the message"
    );
    assert!(local_repo.path().join("remote.txt").exists());
    assert!(local_repo.path().join("local.txt").exists());
}

/// `pull --squash` integrates the diverged upstream into the index/worktree but
/// does not commit, move HEAD, or record merge state; the staged result then
/// finalizes as an ordinary single-parent commit.
#[tokio::test]
#[serial]
async fn test_pull_squash_stages_merge_without_committing() {
    let (_temp_root, remote_dir, work_dir, branch) = create_remote_fixture();

    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);

    assert_cli_success(
        &run_libra_command(&["pull"], local_repo.path()),
        "initial pull",
    );
    push_remote_commit(
        &work_dir,
        &branch,
        "remote.txt",
        "remote change\n",
        "remote update",
    );

    fs::write(local_repo.path().join("local.txt"), "local change\n").expect("write local change");
    assert_cli_success(
        &run_libra_command(&["add", "local.txt"], local_repo.path()),
        "stage local change",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "local update", "--no-verify"],
            local_repo.path(),
        ),
        "commit local change",
    );

    let local_head = {
        let _guard = ChangeDirGuard::new(local_repo.path());
        Head::current_commit()
            .await
            .expect("local commit should leave HEAD")
    };

    let output = run_libra_command(&["pull", "--squash"], local_repo.path());
    assert_cli_success(&output, "pull --squash");

    {
        let _guard = ChangeDirGuard::new(local_repo.path());
        let head_after = Head::current_commit().await.expect("HEAD after squash");
        assert_eq!(head_after, local_head, "pull --squash must not move HEAD");
    }
    assert!(
        local_repo.path().join("remote.txt").exists(),
        "squash should apply the merged worktree"
    );
    assert!(
        !local_repo
            .path()
            .join(".libra")
            .join("merge-state.json")
            .exists(),
        "pull --squash must not record merge state"
    );

    // The staged merge result finalizes as a normal single-parent commit.
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "squashed merge", "--no-verify"],
            local_repo.path(),
        ),
        "commit squashed result",
    );
    {
        let _guard = ChangeDirGuard::new(local_repo.path());
        let squashed = Head::current_commit().await.expect("squashed HEAD");
        let commit: Commit = load_object(&squashed).expect("load squashed commit");
        assert_eq!(
            commit.parent_commit_ids.len(),
            1,
            "a squashed pull commits a single-parent commit, not a merge"
        );
    }
}

/// `pull --no-commit` performs the merge and stages it but stops before
/// committing, recording merge state so `merge --continue` finalizes the
/// two-parent merge commit.
#[tokio::test]
#[serial]
async fn test_pull_no_commit_stops_before_commit_and_records_merge_state() {
    let (_temp_root, remote_dir, work_dir, branch) = create_remote_fixture();

    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);

    assert_cli_success(
        &run_libra_command(&["pull"], local_repo.path()),
        "initial pull",
    );
    push_remote_commit(
        &work_dir,
        &branch,
        "remote.txt",
        "remote change\n",
        "remote update",
    );

    fs::write(local_repo.path().join("local.txt"), "local change\n").expect("write local change");
    assert_cli_success(
        &run_libra_command(&["add", "local.txt"], local_repo.path()),
        "stage local change",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "local update", "--no-verify"],
            local_repo.path(),
        ),
        "commit local change",
    );

    let local_head = {
        let _guard = ChangeDirGuard::new(local_repo.path());
        Head::current_commit()
            .await
            .expect("local commit should leave HEAD")
    };

    let output = run_libra_command(&["pull", "--no-commit"], local_repo.path());
    assert_cli_success(&output, "pull --no-commit");

    {
        let _guard = ChangeDirGuard::new(local_repo.path());
        let head_after = Head::current_commit().await.expect("HEAD after no-commit");
        assert_eq!(
            head_after, local_head,
            "pull --no-commit must not move HEAD"
        );
    }
    assert!(
        local_repo.path().join("remote.txt").exists(),
        "no-commit should apply the merged worktree"
    );
    assert!(
        local_repo
            .path()
            .join(".libra")
            .join("merge-state.json")
            .exists(),
        "pull --no-commit must record merge state for `merge --continue`"
    );

    // `merge --continue` finalizes the two-parent merge commit.
    assert_cli_success(
        &run_libra_command(&["merge", "--continue"], local_repo.path()),
        "merge --continue after pull --no-commit",
    );
    {
        let _guard = ChangeDirGuard::new(local_repo.path());
        let merged = Head::current_commit().await.expect("merged HEAD");
        assert_ne!(merged, local_head, "merge --continue should advance HEAD");
        let commit: Commit = load_object(&merged).expect("load merge commit");
        assert_eq!(
            commit.parent_commit_ids.len(),
            2,
            "finalized no-commit merge must have two parents"
        );
    }
}

#[tokio::test]
#[serial]
async fn test_pull_ff_only_diverged_remote_rejects_without_changing_head_or_worktree() {
    let (_temp_root, remote_dir, work_dir, branch) = create_remote_fixture();

    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);

    let first_pull = run_libra_command(&["pull"], local_repo.path());
    assert_cli_success(&first_pull, "initial pull");

    let _remote_head = push_remote_commit(
        &work_dir,
        &branch,
        "remote.txt",
        "remote change\n",
        "remote update",
    );

    fs::write(local_repo.path().join("local.txt"), "local change\n").expect("write local change");
    let add = run_libra_command(&["add", "local.txt"], local_repo.path());
    assert_cli_success(&add, "stage local change");
    let commit = run_libra_command(
        &["commit", "-m", "local update", "--no-verify"],
        local_repo.path(),
    );
    assert_cli_success(&commit, "commit local change");

    let guard = ChangeDirGuard::new(local_repo.path());
    let local_head = Head::current_commit()
        .await
        .expect("local commit should leave HEAD");
    drop(guard);

    let output = run_libra_command(&["--json", "pull", "--ff-only"], local_repo.path());
    let report = parse_json_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report["ok"], false);
    assert_eq!(report["error_code"], "LBR-CONFLICT-002");
    assert_eq!(report["details"]["phase"], "merge");
    assert!(
        report["message"]
            .as_str()
            .is_some_and(|text| text.contains("non-fast-forward")),
        "pull --ff-only should explain the rejected merge: {report}"
    );
    assert!(
        report["hints"]
            .as_array()
            .expect("hints")
            .iter()
            .any(|hint| hint
                .as_str()
                .is_some_and(|text| text.contains("without --ff-only"))),
        "pull --ff-only should hint how to allow a merge commit: {report}"
    );

    let _guard = ChangeDirGuard::new(local_repo.path());
    let head_after = Head::current_commit()
        .await
        .expect("failed pull --ff-only should leave HEAD unchanged");
    assert_eq!(head_after, local_head);
    assert!(
        local_repo.path().join("local.txt").exists(),
        "local worktree file must remain"
    );
    assert!(
        !local_repo.path().join("remote.txt").exists(),
        "ff-only rejection must not apply remote worktree changes"
    );
    assert!(
        !local_repo
            .path()
            .join(".libra")
            .join("merge-state.json")
            .exists(),
        "ff-only rejection must not create merge state"
    );
}

#[tokio::test]
#[serial]
async fn test_pull_detached_head_returns_repo_exit_code() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    let head = Head::current_commit().await.expect("repo should have HEAD");
    Head::update(Head::Detached(head), None).await;

    let output = run_libra_command(&["pull"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-003");
    assert!(stderr.contains("you are not currently on a branch"));
}

#[test]
#[serial]
fn test_pull_quiet_suppresses_stdout() {
    let (_temp_root, remote_dir, _work_dir, branch) = create_remote_fixture();

    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);

    let output = run_libra_command(&["--quiet", "pull"], local_repo.path());
    assert_cli_success(&output, "quiet pull");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().is_empty(),
        "quiet pull should suppress stdout, got: {stdout}"
    );
}

#[test]
#[serial]
fn test_pull_human_output_reports_update_range_after_follow_up_fast_forward() {
    let (_temp_root, remote_dir, work_dir, branch) = create_remote_fixture();

    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);

    let first_pull = run_libra_command(&["pull"], local_repo.path());
    assert_cli_success(&first_pull, "initial pull");

    let new_head = push_remote_commit(
        &work_dir,
        &branch,
        "next.txt",
        "next change\n",
        "remote follow-up",
    );
    let previous_head = git_stdout(&["rev-parse", "HEAD~1"], &work_dir);

    let output = run_libra_command(&["pull"], local_repo.path());
    assert_cli_success(&output, "follow-up pull");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("From "),
        "pull should include the fetched remote, stdout: {stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "Updating {}..{}",
            &previous_head[..7],
            &new_head[..7]
        )),
        "pull should report the fast-forward range, stdout: {stdout}"
    );
    assert!(
        stdout.lines().any(|line| line == "Fast-forward"),
        "pull should report the merge strategy, stdout: {stdout}"
    );
    assert!(
        stdout.contains("1 file changed"),
        "pull should summarize changed files, stdout: {stdout}"
    );
}

#[test]
#[serial]
fn test_pull_json_fetch_error_includes_phase_detail() {
    let repo = create_committed_repo_via_cli();

    let missing_remote = repo.path().join("missing-remote.git");
    let missing_remote_str = missing_remote.to_string_lossy().to_string();

    let remote_output = run_libra_command(
        &["remote", "add", "origin", &missing_remote_str],
        repo.path(),
    );
    assert_cli_success(&remote_output, "remote add");
    let branch_remote = run_libra_command(&["config", "branch.main.remote", "origin"], repo.path());
    assert_cli_success(&branch_remote, "set branch.main.remote");
    let branch_merge = run_libra_command(
        &["config", "branch.main.merge", "refs/heads/main"],
        repo.path(),
    );
    assert_cli_success(&branch_merge, "set branch.main.merge");

    let output = run_libra_command(&["--json", "pull"], repo.path());
    let report = parse_json_stderr(&output.stderr);

    assert_eq!(report["ok"], false);
    assert_eq!(report["details"]["phase"], "fetch");
}

#[test]
#[serial]
fn test_pull_json_diverged_remote_reports_three_way_merge() {
    let (_temp_root, remote_dir, work_dir, branch) = create_remote_fixture();

    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);

    let first_pull = run_libra_command(&["pull"], local_repo.path());
    assert_cli_success(&first_pull, "initial pull");

    let _remote_head = push_remote_commit(
        &work_dir,
        &branch,
        "remote.txt",
        "remote change\n",
        "remote update",
    );

    fs::write(local_repo.path().join("local.txt"), "local change\n").expect("write local change");
    let add = run_libra_command(&["add", "local.txt"], local_repo.path());
    assert_cli_success(&add, "stage local change");
    let commit = run_libra_command(
        &["commit", "-m", "local update", "--no-verify"],
        local_repo.path(),
    );
    assert_cli_success(&commit, "commit local change");

    let output = run_libra_command(&["--json", "pull"], local_repo.path());
    assert_cli_success(&output, "json pull three-way merge");
    assert!(output.stderr.is_empty());
    let report = parse_json_stdout(&output);

    assert_eq!(report["ok"], true);
    assert_eq!(report["command"], "pull");
    assert_eq!(report["data"]["merge"]["strategy"], "three-way");
    assert_eq!(
        report["data"]["merge"]["parents"]
            .as_array()
            .expect("parents")
            .len(),
        2
    );
}

#[test]
#[serial]
fn test_pull_conflict_error_includes_merge_phase_and_hints() {
    let (_temp_root, remote_dir, work_dir, branch) = create_remote_fixture();

    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);

    let first_pull = run_libra_command(&["pull"], local_repo.path());
    assert_cli_success(&first_pull, "initial pull");

    let _remote_head = push_remote_commit(
        &work_dir,
        &branch,
        "README.md",
        "remote change\n",
        "remote update",
    );

    fs::write(local_repo.path().join("README.md"), "local change\n").expect("write local change");
    let add = run_libra_command(&["add", "README.md"], local_repo.path());
    assert_cli_success(&add, "stage local change");
    let commit = run_libra_command(
        &["commit", "-m", "local update", "--no-verify"],
        local_repo.path(),
    );
    assert_cli_success(&commit, "commit local change");

    let output = run_libra_command(&["--json", "pull"], local_repo.path());
    let report = parse_json_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report["ok"], false);
    assert_eq!(report["error_code"], "LBR-CONFLICT-002");
    assert_eq!(report["details"]["phase"], "merge");
    assert!(
        report["hints"]
            .as_array()
            .expect("hints")
            .iter()
            .any(|hint| hint
                .as_str()
                .is_some_and(|text| text.contains("merge --continue"))),
        "pull conflict should hint merge --continue: {report}"
    );

    let conflicted =
        fs::read_to_string(local_repo.path().join("README.md")).expect("read conflict markers");
    assert!(conflicted.contains("<<<<<<< HEAD"), "{conflicted}");
    assert!(conflicted.contains(">>>>>>>"), "{conflicted}");
}

/// `libra pull --rebase` replays the local-only commit on top of the
/// freshly-fetched upstream tip when the histories have diverged.
#[tokio::test]
#[serial]
async fn test_pull_rebase_replays_local_commit_onto_diverged_upstream() {
    let (_temp_root, remote_dir, work_dir, branch) = create_remote_fixture();

    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);

    let first_pull = run_libra_command(&["pull"], local_repo.path());
    assert_cli_success(&first_pull, "initial pull");

    let remote_head = push_remote_commit(
        &work_dir,
        &branch,
        "remote.txt",
        "remote change\n",
        "remote update",
    );

    fs::write(local_repo.path().join("local.txt"), "local change\n").expect("write local change");
    let add = run_libra_command(&["add", "local.txt"], local_repo.path());
    assert_cli_success(&add, "stage local change");
    let commit = run_libra_command(
        &["commit", "-m", "local update", "--no-verify"],
        local_repo.path(),
    );
    assert_cli_success(&commit, "commit local change");

    let output = run_libra_command(&["--json", "pull", "--rebase"], local_repo.path());
    assert_cli_success(&output, "rebase pull");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON on stdout, got: {stdout}\nerror: {e}"));
    let data = &parsed["data"];

    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["command"], "pull");
    assert!(data["merge"].is_null());
    assert_eq!(data["rebase"]["status"], "completed");
    assert_eq!(data["rebase"]["replay_count"], 1);
    assert_eq!(data["rebase"]["up_to_date"], false);
    assert!(data["rebase"]["commit"].is_string());
    assert!(data["rebase"]["old_commit"].is_string());
    assert!(
        local_repo.path().join("remote.txt").exists(),
        "rebase should have brought in remote.txt"
    );
    assert!(
        local_repo.path().join("local.txt").exists(),
        "rebase should keep local.txt"
    );

    let new_commit = data["rebase"]["commit"].as_str().expect("commit string");
    assert_ne!(
        new_commit, remote_head,
        "rebased commit must be a child of upstream, not the upstream tip itself"
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_pull_rebase_runs_pre_rebase_before_moving_local_history() {
    use std::os::unix::fs::PermissionsExt;

    let (_temp_root, remote_dir, work_dir, branch) = create_remote_fixture();
    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);
    assert_cli_success(
        &run_libra_command(&["pull"], local_repo.path()),
        "initial pull",
    );

    push_remote_commit(
        &work_dir,
        &branch,
        "remote-hook.txt",
        "remote hook change\n",
        "remote hook update",
    );
    fs::write(
        local_repo.path().join("local-hook.txt"),
        "local hook change\n",
    )
    .expect("write local hook fixture");
    assert_cli_success(
        &run_libra_command(&["add", "local-hook.txt"], local_repo.path()),
        "stage local hook fixture",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "--no-verify", "-m", "local hook update"],
            local_repo.path(),
        ),
        "commit local hook fixture",
    );
    let head_before =
        String::from_utf8(run_libra_command(&["rev-parse", "HEAD"], local_repo.path()).stdout)
            .expect("HEAD output is utf8")
            .trim()
            .to_string();

    let hook = local_repo.path().join(".libra/hooks/pre-rebase");
    fs::write(
        &hook,
        "#!/bin/sh\nprintf 'hook-stdout-must-not-pollute-json\\n'\nprintf 'hook-stderr-must-not-pollute-json\\n' >&2\nprintf '%s:%s\\n' \"$1\" \"$2\" > \"$LIBRA_WORK_TREE/pre-rebase.log\"\nexit 23\n",
    )
    .expect("write pull pre-rebase hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755))
        .expect("make pull pre-rebase hook executable");

    let blocked = run_libra_command(&["--json", "pull", "--rebase"], local_repo.path());
    assert!(
        !blocked.status.success(),
        "pre-rebase must be able to block pull --rebase"
    );
    assert!(
        String::from_utf8_lossy(&blocked.stderr).contains("pre-rebase hook failed"),
        "{}",
        String::from_utf8_lossy(&blocked.stderr)
    );
    assert!(
        blocked.stdout.is_empty(),
        "nested hook stdout must not pollute pull's JSON error surface: {}",
        String::from_utf8_lossy(&blocked.stdout)
    );
    assert!(
        !String::from_utf8_lossy(&blocked.stderr).contains("hook-stderr-must-not-pollute-json"),
        "nested hook stderr must be suppressed in JSON mode: {}",
        String::from_utf8_lossy(&blocked.stderr)
    );
    assert_eq!(
        fs::read_to_string(local_repo.path().join("pre-rebase.log"))
            .expect("read pull pre-rebase argv"),
        format!("origin/{branch}:\n")
    );
    let head_after =
        String::from_utf8(run_libra_command(&["rev-parse", "HEAD"], local_repo.path()).stdout)
            .expect("HEAD output is utf8")
            .trim()
            .to_string();
    assert_eq!(
        head_after, head_before,
        "blocked pull must not rewrite HEAD"
    );
}

#[tokio::test]
#[serial]
async fn test_pull_rebase_already_up_to_date_reports_noop() {
    let (_temp_root, remote_dir, _work_dir, branch) = create_remote_fixture();

    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);

    let first_pull = run_libra_command(&["pull"], local_repo.path());
    assert_cli_success(&first_pull, "initial pull");

    let output = run_libra_command(&["--json", "pull", "--rebase"], local_repo.path());
    assert_cli_success(&output, "rebase pull (no-op)");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON on stdout, got: {stdout}\nerror: {e}"));
    let data = &parsed["data"];

    assert_eq!(data["rebase"]["replay_count"], 0);
    assert_eq!(data["rebase"]["up_to_date"], true);
    let old = data["rebase"]["old_commit"]
        .as_str()
        .expect("old_commit string");
    let new_commit = data["rebase"]["commit"].as_str().expect("commit string");
    assert_eq!(
        old, new_commit,
        "HEAD must not move when there is nothing to rebase"
    );
}

#[test]
fn test_pull_ff_conflicts_with_no_ff_at_parse_time() {
    // `--ff` and `--no-ff` are clap-conflicting and must be rejected before any
    // repository / network work happens.
    let repo = tempdir().expect("failed to create local repo");
    let output = run_libra_command(&["pull", "--ff", "--no-ff"], repo.path());
    let (stderr, _report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(129));
    assert!(
        stderr.contains("cannot be used with")
            && stderr.contains("--ff")
            && stderr.contains("--no-ff"),
        "pull should reject --ff with --no-ff before preflight: {stderr}"
    );
}

#[test]
fn test_pull_no_ff_conflicts_with_ff_only_at_parse_time() {
    let repo = tempdir().expect("failed to create local repo");
    let output = run_libra_command(&["pull", "--no-ff", "--ff-only"], repo.path());
    let (stderr, _report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(129));
    assert!(
        stderr.contains("cannot be used with"),
        "pull should reject --no-ff with --ff-only before preflight: {stderr}"
    );
}

/// `libra pull --no-ff` against a fast-forwardable upstream records a real
/// two-parent merge commit instead of fast-forwarding HEAD to the remote tip.
#[tokio::test]
#[serial]
async fn test_pull_no_ff_forces_merge_commit_on_fast_forwardable_history() {
    let (_temp_root, remote_dir, work_dir, branch) = create_remote_fixture();

    let local_repo = tempdir().expect("failed to create local repo");
    init_repo_via_cli(local_repo.path());
    configure_identity_via_cli(local_repo.path());
    configure_pull_tracking(local_repo.path(), &remote_dir, &branch);

    let first_pull = run_libra_command(&["pull"], local_repo.path());
    assert_cli_success(&first_pull, "initial pull");
    let new_head = push_remote_commit(
        &work_dir,
        &branch,
        "remote.txt",
        "remote change\n",
        "remote update",
    );

    let output = run_libra_command(&["--json", "pull", "--no-ff"], local_repo.path());
    assert_cli_success(&output, "pull --no-ff");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON on stdout, got: {stdout}\nerror: {e}"));
    let data = &parsed["data"];

    assert_eq!(
        data["merge"]["strategy"], "three-way",
        "--no-ff must force a merge commit even when fast-forward is possible"
    );
    let parents = data["merge"]["parents"]
        .as_array()
        .expect("merge parents array");
    assert_eq!(
        parents.len(),
        2,
        "--no-ff merge commit must have two parents"
    );
    assert!(
        parents
            .iter()
            .any(|p| p.as_str() == Some(new_head.as_str())),
        "the fetched upstream tip must be one of the merge parents"
    );

    let _guard = ChangeDirGuard::new(local_repo.path());
    let head = Head::current_commit()
        .await
        .expect("pull --no-ff should record a merge commit at HEAD");
    assert_ne!(
        head.to_string(),
        new_head,
        "--no-ff must not fast-forward HEAD to the remote tip"
    );
    assert!(
        local_repo.path().join("remote.txt").exists(),
        "pull --no-ff should still bring in the fetched content"
    );
}

#[test]
fn pull_autostash_flag_is_accepted() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // `--autostash` parses and reaches the runtime: without an upstream it fails
    // with a tracking-information error, NOT a clap "unexpected argument" error.
    // (The stash/integrate/pop orchestration itself requires a remote and is
    // covered by the network-gated pull tests; the stash mechanics it reuses are
    // covered by the stash command tests.)
    let out = run_libra_command(&["pull", "--autostash"], p);
    assert!(!out.status.success(), "pull without an upstream fails");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("unexpected argument"),
        "--autostash is accepted by the parser: {err:?}"
    );
    assert!(
        err.contains("tracking information"),
        "--autostash reaches the tracking check: {err:?}"
    );
}

#[test]
fn pull_no_progress_flag_is_accepted() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // `--no-progress` parses and reaches the runtime (the fetch progress
    // suppression is covered by fetch's `apply_no_progress` unit test, which
    // pull reuses). With no upstream it fails with a tracking error, not clap.
    let out = run_libra_command(&["pull", "--no-progress"], p);
    assert!(!out.status.success(), "pull without an upstream fails");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("unexpected argument"),
        "--no-progress is accepted by the parser: {err}"
    );
}
