//! Tests diff command across commits, stage, and working tree with algorithm and pathspec options.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::{fs, io::Write};

use clap::Parser;
use libra::{
    command::diff::{self, DiffArgs},
    utils::{output::OutputConfig, pager::LIBRA_PAGER_ENV},
};

use super::*;

#[test]
fn test_diff_cli_outside_repository_returns_fatal_128() {
    let temp = tempdir().unwrap();
    let output = run_libra_command(&["diff"], temp.path());
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal: not a libra repository"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn test_diff_json_output_includes_file_stats() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\nupdated\n").unwrap();

    let output = run_libra_command(&["--json", "diff"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "diff");
    assert_eq!(json["data"]["files_changed"], 1);
    assert_eq!(json["data"]["files"][0]["path"], "tracked.txt");
    assert!(json["data"]["files"][0]["hunks"].as_array().is_some());
}

#[test]
fn test_diff_two_dot_range_positional() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    fs::write(p.join("a.txt"), "one\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add c1");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    let c1 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();

    fs::write(p.join("a.txt"), "two\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add c2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );
    let c2 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();

    // `diff A..B` (positional two-dot range) should diff the two commits.
    let out = run_libra_command(&["diff", &format!("{c1}..{c2}")], p);
    assert_cli_success(&out, "diff A..B");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("a.txt"),
        "diff A..B should mention a.txt: {stdout}"
    );
    assert!(
        stdout.contains("one") && stdout.contains("two"),
        "diff A..B should show the one->two change: {stdout}"
    );
}

#[test]
fn test_diff_machine_output_is_single_line_json() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\nupdated\n").unwrap();

    let output = run_libra_command(&["--machine", "diff"], repo.path());
    assert_cli_success(&output, "machine diff");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let non_empty_lines: Vec<&str> = stdout.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(
        non_empty_lines.len(),
        1,
        "machine output should be exactly one non-empty line, got: {stdout}"
    );

    let parsed: serde_json::Value =
        serde_json::from_str(non_empty_lines[0]).expect("machine output should be valid JSON");
    assert_eq!(parsed["command"], "diff");
    assert_eq!(parsed["data"]["files_changed"], 1);
}

#[test]
fn test_diff_reports_tracked_files_inside_ignored_directories() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join(".libraignore"), "target/\n").unwrap();
    fs::create_dir_all(repo.path().join("target")).unwrap();
    fs::write(repo.path().join("target/tracked.txt"), "tracked\n").unwrap();

    let add = run_libra_command(
        &["add", "-f", ".libraignore", "target/tracked.txt"],
        repo.path(),
    );
    assert_cli_success(&add, "force-add tracked file under ignored directory");
    let commit = run_libra_command(
        &[
            "commit",
            "-m",
            "track ignored directory file",
            "--no-verify",
        ],
        repo.path(),
    );
    assert_cli_success(&commit, "commit ignored directory fixture");

    fs::write(repo.path().join("target/tracked.txt"), "tracked\nupdated\n").unwrap();
    fs::write(repo.path().join("target/untracked.txt"), "ignored\n").unwrap();

    let output = run_libra_command(&["diff", "--name-only"], repo.path());
    assert_cli_success(&output, "diff ignored directory tracked file");

    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "target/tracked.txt"
    );
}

#[test]
fn test_diff_human_worktree_diff_emits_scan_progress() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\nupdated\n").unwrap();

    let output = run_libra_command(&["diff", "--name-only"], repo.path());
    assert_cli_success(&output, "human worktree diff with scan progress");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Scanning working tree"),
        "expected worktree scan progress on stderr, got: {stderr}"
    );
}

#[test]
fn test_diff_progress_none_suppresses_scan_progress() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\nupdated\n").unwrap();

    let output = run_libra_command(&["--progress=none", "diff", "--name-only"], repo.path());
    assert_cli_success(&output, "diff with progress disabled");

    assert!(
        output.stderr.is_empty(),
        "explicit --progress=none should suppress scan progress, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_diff_non_default_algorithm_fails_instead_of_silent_noop() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\nupdated\n").unwrap();

    let output = run_libra_command(&["diff", "--algorithm", "myers"], repo.path());
    assert_eq!(output.status.code(), Some(129));
    assert!(
        output.stdout.is_empty(),
        "unsupported algorithm must not emit a best-effort diff to stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("diff --algorithm=myers is not supported yet"),
        "unsupported algorithm should be explicit, stderr={stderr}"
    );
    assert!(
        stderr.contains("Error-Code: LBR-CLI-002"),
        "unsupported algorithm should carry a stable CLI error code, stderr={stderr}"
    );
    assert!(
        !stderr.contains("Scanning working tree"),
        "algorithm validation should fail before the worktree scan, stderr={stderr}"
    );
}

#[tokio::test]
#[serial]
async fn test_diff_empty_output_does_not_initialize_pager() {
    if cfg!(windows) {
        return;
    }

    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());
    let missing_bin_dir = tempdir().unwrap();
    let _path = test::ScopedEnvVar::set("PATH", missing_bin_dir.path());
    let _pager = test::ScopedEnvVar::set(LIBRA_PAGER_ENV, "always");

    let args = DiffArgs::try_parse_from(["libra"]).unwrap();
    let result = diff::execute_safe(args, &OutputConfig::default()).await;
    assert!(
        result.is_ok(),
        "empty diff should not initialize pager: {result:?}"
    );
}

#[test]
fn test_diff_name_only_and_name_status_flags_render_cli_output() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\nupdated\n").unwrap();

    let name_only = run_libra_command(&["diff", "--name-only"], repo.path());
    assert_cli_success(&name_only, "diff --name-only");
    assert_eq!(
        String::from_utf8_lossy(&name_only.stdout).trim(),
        "tracked.txt"
    );

    let name_status = run_libra_command(&["diff", "--name-status"], repo.path());
    assert_cli_success(&name_status, "diff --name-status");
    assert_eq!(
        String::from_utf8_lossy(&name_status.stdout).trim(),
        "M\ttracked.txt"
    );
}

#[test]
fn test_diff_numstat_and_stat_flags_render_cli_output() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\nupdated\n").unwrap();

    let numstat = run_libra_command(&["diff", "--numstat"], repo.path());
    assert_cli_success(&numstat, "diff --numstat");
    assert_eq!(
        String::from_utf8_lossy(&numstat.stdout).trim(),
        "1\t0\ttracked.txt"
    );

    let stat = run_libra_command(&["diff", "--stat"], repo.path());
    assert_cli_success(&stat, "diff --stat");
    let stat_stdout = String::from_utf8_lossy(&stat.stdout);
    assert!(
        stat_stdout.contains("tracked.txt | 1 +"),
        "expected per-file stat line, got: {stat_stdout}"
    );
    assert!(
        stat_stdout.contains("1 file changed, 1 insertion(+), 0 deletions(-)"),
        "expected stat summary, got: {stat_stdout}"
    );
}

#[test]
fn test_diff_quiet_uses_exit_code_to_signal_changes() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\nupdated\n").unwrap();

    let output = run_libra_command(&["--quiet", "diff"], repo.path());
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "unexpected stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_diff_quiet_with_output_file_still_returns_exit_code_1() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\nupdated\n").unwrap();
    let output_file = repo.path().join("captured.diff");
    let output_path = output_file.to_str().unwrap();

    let output = run_libra_command(&["--quiet", "diff", "--output", output_path], repo.path());
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let written = fs::read_to_string(&output_file).unwrap();
    assert!(
        written.contains("diff --git"),
        "expected diff output file to be written, got: {written}"
    );
}

#[test]
fn test_diff_json_ignores_output_file_flag() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\nupdated\n").unwrap();
    let output_file = repo.path().join("ignored.diff");
    let output_path = output_file.to_str().unwrap();

    let output = run_libra_command(&["--json", "diff", "--output", output_path], repo.path());
    assert_cli_success(&output, "json diff with output flag");
    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "diff");
    assert!(
        !output_file.exists(),
        "--output should be ignored in JSON mode, but {:?} was created",
        output_file
    );
}

#[test]
fn test_diff_status_detection_ignores_patch_body_text() {
    let repo = create_committed_repo_via_cli();
    fs::write(
        repo.path().join("tracked.txt"),
        "tracked\nnew file mode 100644\ndeleted file mode 100644\n",
    )
    .unwrap();

    let name_status = run_libra_command(&["diff", "--name-status"], repo.path());
    assert_cli_success(&name_status, "diff --name-status");
    assert_eq!(
        String::from_utf8_lossy(&name_status.stdout).trim(),
        "M\ttracked.txt"
    );

    let json = run_libra_command(&["--json", "diff"], repo.path());
    assert_cli_success(&json, "diff --json");
    let json = parse_json_stdout(&json);
    assert_eq!(json["data"]["files"][0]["path"], "tracked.txt");
    assert_eq!(json["data"]["files"][0]["status"], "modified");
}

#[test]
fn test_diff_stats_count_hunk_lines_that_start_with_header_prefixes() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\n---gone\n").unwrap();
    let add_output = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert_cli_success(&add_output, "add tracked.txt");
    let commit_output =
        run_libra_command(&["commit", "-m", "seed header-like content"], repo.path());
    assert_cli_success(&commit_output, "commit seed header-like content");

    fs::write(repo.path().join("tracked.txt"), "+++added\n").unwrap();

    let numstat = run_libra_command(&["diff", "--numstat"], repo.path());
    assert_cli_success(&numstat, "diff --numstat");
    assert_eq!(
        String::from_utf8_lossy(&numstat.stdout).trim(),
        "1\t2\ttracked.txt"
    );

    let stat = run_libra_command(&["diff", "--stat"], repo.path());
    assert_cli_success(&stat, "diff --stat");
    let stat_stdout = String::from_utf8_lossy(&stat.stdout);
    assert!(
        stat_stdout.contains("tracked.txt | 3 +--"),
        "expected stat output to count header-like hunk lines, got: {stat_stdout}"
    );
    assert!(
        stat_stdout.contains("1 file changed, 1 insertion(+), 2 deletions(-)"),
        "expected stat summary to count header-like hunk lines, got: {stat_stdout}"
    );

    let json = run_libra_command(&["--json", "diff"], repo.path());
    assert_cli_success(&json, "diff --json");
    let json = parse_json_stdout(&json);
    assert_eq!(json["data"]["files"][0]["insertions"], 1);
    assert_eq!(json["data"]["files"][0]["deletions"], 2);
    assert_eq!(json["data"]["total_insertions"], 1);
    assert_eq!(json["data"]["total_deletions"], 2);
}

#[test]
fn test_diff_added_and_deleted_files_use_dev_null_headers() {
    let repo = create_committed_repo_via_cli();

    // Since P1-03 the default diff matches Git: untracked files are NOT part
    // of it. A staged new file shows the /dev/null old header via --staged.
    fs::write(repo.path().join("added.txt"), "added\n").unwrap();
    let untracked = run_libra_command(&["diff"], repo.path());
    assert_cli_success(&untracked, "diff with untracked file");
    let untracked_stdout = String::from_utf8_lossy(&untracked.stdout);
    assert!(
        !untracked_stdout.contains("added.txt"),
        "untracked files must not appear in the default diff, got: {untracked_stdout}"
    );

    let add = run_libra_command(&["add", "added.txt"], repo.path());
    assert_cli_success(&add, "stage added file");
    let added = run_libra_command(&["diff", "--staged"], repo.path());
    assert_cli_success(&added, "diff staged added file");
    let added_stdout = String::from_utf8_lossy(&added.stdout);
    assert!(
        added_stdout.contains("--- /dev/null"),
        "expected staged added file diff to use /dev/null old header, got: {added_stdout}"
    );
    assert!(
        added_stdout.contains("+++ b/added.txt"),
        "expected staged added file diff to use b/ path in new header, got: {added_stdout}"
    );

    fs::remove_file(repo.path().join("tracked.txt")).unwrap();
    let deleted = run_libra_command(&["diff"], repo.path());
    assert_cli_success(&deleted, "diff deleted file");
    let deleted_stdout = String::from_utf8_lossy(&deleted.stdout);
    assert!(
        deleted_stdout.contains("--- a/tracked.txt"),
        "expected deleted file diff to use a/ path in old header, got: {deleted_stdout}"
    );
    assert!(
        deleted_stdout.contains("+++ /dev/null"),
        "expected deleted file diff to use /dev/null new header, got: {deleted_stdout}"
    );
}

/// Helper function to create a file with content.
fn create_file(path: &str, content: &str) {
    let mut file = fs::File::create(path).unwrap();
    file.write_all(content.as_bytes()).unwrap();
}

/// Helper function to modify a file with new content.
fn modify_file(path: &str, content: &str) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .unwrap();
    file.write_all(content.as_bytes()).unwrap();
}

#[tokio::test]
#[serial]
/// Tests diff command immediately after libra init (empty repository scenario).
/// This tests the edge case where there are no commits and no staged changes.
async fn test_diff_after_init() {
    let test_dir = tempdir().unwrap();
    let output_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = ChangeDirGuard::new(test_dir.path());

    let output_file = output_dir.path().join("diff_output.txt");
    let output_str = output_file.to_str().unwrap();
    let args = DiffArgs::parse_from(["diff", "--output", output_str]);
    diff::execute(args).await;

    // Since P1-03 the default diff matches Git: the untracked .libraignore
    // created by init is NOT part of it, and an empty repository diffs empty.
    let content = fs::read_to_string(&output_file).unwrap_or_default();
    assert!(
        content.trim().is_empty(),
        "an empty repository's default diff must be empty (untracked files \
         excluded since P1-03), got: {content}"
    );

    // Once staged, the file appears with the /dev/null old header via --staged.
    add::execute(AddArgs {
        pathspec: vec![String::from(".libraignore")],
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
    let staged_file = output_dir.path().join("diff_staged_output.txt");
    let staged_str = staged_file.to_str().unwrap();
    let staged_args = DiffArgs::parse_from(["diff", "--staged", "--output", staged_str]);
    diff::execute(staged_args).await;
    let staged_content = fs::read_to_string(&staged_file).unwrap_or_default();
    assert!(
        staged_content.contains("diff --git a/.libraignore b/.libraignore"),
        "staged .libraignore must be visible in diff --staged, got: {staged_content}"
    );
    assert!(
        staged_content.contains("# Libra ignore file"),
        "default .libraignore contents expected in diff --staged, got: {staged_content}"
    );
}

#[tokio::test]
#[serial]
/// Tests the basic diff functionality between working directory and HEAD.
async fn test_basic_diff() {
    let test_dir = tempdir().unwrap();
    let output_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a file and add it to index
    create_file("file1.txt", "Initial content\nLine 2\nLine 3\n");

    add::execute(AddArgs {
        pathspec: vec![String::from("file1.txt")],
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

    // Create initial commit
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

    // Modify the file
    modify_file("file1.txt", "Modified content\nLine 2\nLine 3 changed\n");

    // Run diff command with output to file to avoid pager
    let output_file = output_dir.path().join("diff_output.txt");
    let output_str = output_file.to_str().unwrap();
    let args = DiffArgs::parse_from(["diff", "--algorithm", "histogram", "--output", output_str]);
    diff::execute(args).await;

    let content = fs::read_to_string(&output_file).unwrap();
    assert!(
        content.contains("diff --git"),
        "Output should contain diff header"
    );
    assert!(
        content.contains("-Initial content"),
        "Output should show removed line"
    );
    assert!(
        content.contains("+Modified content"),
        "Output should show added line"
    );
}

#[tokio::test]
#[serial]
/// Tests diff with staged changes
async fn test_diff_staged() {
    let test_dir = tempdir().unwrap();
    let output_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a file and add it to index
    create_file("file1.txt", "Initial content\nLine 2\nLine 3\n");

    add::execute(AddArgs {
        pathspec: vec![String::from("file1.txt")],
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

    // Create initial commit
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

    // Modify the file and stage it
    modify_file("file1.txt", "Modified content\nLine 2\nLine 3 changed\n");

    add::execute(AddArgs {
        pathspec: vec![String::from("file1.txt")],
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

    // Modify the file again (so working dir differs from staged)
    modify_file(
        "file1.txt",
        "Modified content again\nLine 2\nLine 3 changed again\n",
    );

    // Run diff command with --staged flag, output to file to avoid pager
    let output_file = output_dir.path().join("diff_output.txt");
    let output_str = output_file.to_str().unwrap();
    let args = DiffArgs::parse_from([
        "diff",
        "--staged",
        "--algorithm",
        "histogram",
        "--output",
        output_str,
    ]);
    diff::execute(args).await;

    let content = fs::read_to_string(&output_file).unwrap();
    assert!(
        content.contains("diff --git"),
        "Staged diff should contain diff header"
    );
    assert!(
        content.contains("-Initial content"),
        "Staged diff should show removed line"
    );
    assert!(
        content.contains("+Modified content"),
        "Staged diff should show added line"
    );
}

#[tokio::test]
#[serial]
/// Tests diff between two specific commits
async fn test_diff_between_commits() {
    let test_dir = tempdir().unwrap();
    let output_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a file and make initial commit
    create_file("file1.txt", "Initial content\nLine 2\nLine 3\n");

    add::execute(AddArgs {
        pathspec: vec![String::from("file1.txt")],
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

    // Get the first commit hash
    let first_commit = Head::current_commit().await.unwrap();

    // Modify file and create a second commit
    modify_file("file1.txt", "Modified content\nLine 2\nLine 3 changed\n");

    add::execute(AddArgs {
        pathspec: vec![String::from("file1.txt")],
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

    // Get the second commit hash
    let second_commit = Head::current_commit().await.unwrap();

    // Run diff command comparing the two commits, output to file to avoid pager
    let output_file = output_dir.path().join("diff_output.txt");
    let output_str = output_file.to_str().unwrap();
    let args = DiffArgs::parse_from([
        "diff",
        "--old",
        &first_commit.to_string(),
        "--new",
        &second_commit.to_string(),
        "--algorithm",
        "histogram",
        "--output",
        output_str,
    ]);
    diff::execute(args).await;

    let content = fs::read_to_string(&output_file).unwrap();
    assert!(
        content.contains("diff --git"),
        "Commit diff should contain diff header"
    );
    assert!(
        content.contains("-Initial content"),
        "Commit diff should show removed line"
    );
    assert!(
        content.contains("+Modified content"),
        "Commit diff should show added line"
    );
}

#[tokio::test]
#[serial]
/// Tests diff with specific file path
async fn test_diff_with_pathspec() {
    let test_dir = tempdir().unwrap();
    let output_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create multiple files and commit them
    create_file("file1.txt", "File 1 content\nLine 2\nLine 3\n");
    create_file("file2.txt", "File 2 content\nLine 2\nLine 3\n");

    add::execute(AddArgs {
        pathspec: vec![String::from(".")],
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

    // Modify both files
    modify_file("file1.txt", "File 1 modified\nLine 2\nLine 3 changed\n");
    modify_file("file2.txt", "File 2 modified\nLine 2\nLine 3 changed\n");

    // Run diff command with specific file path, output to file to avoid pager
    let output_file = output_dir.path().join("diff_output.txt");
    let output_str = output_file.to_str().unwrap();
    let args = DiffArgs::parse_from([
        "diff",
        "--algorithm",
        "histogram",
        "--output",
        output_str,
        "file1.txt",
    ]);
    diff::execute(args).await;

    let content = fs::read_to_string(&output_file).unwrap();
    assert!(
        content.contains("diff --git"),
        "Pathspec diff should contain diff header"
    );
    assert!(
        content.contains("file1.txt"),
        "Pathspec diff should reference file1.txt"
    );
    // file2.txt should NOT appear in the output since we filtered by pathspec
    assert!(
        !content.contains("file2.txt"),
        "Pathspec diff should not contain file2.txt"
    );
}

#[tokio::test]
#[serial]
/// Tests diff with output to a file
async fn test_diff_output_to_file() {
    let test_dir = tempdir().unwrap();
    let output_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a file and commit it
    create_file("file1.txt", "Initial content\nLine 2\nLine 3\n");

    add::execute(AddArgs {
        pathspec: vec![String::from("file1.txt")],
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

    // Modify the file
    modify_file("file1.txt", "Modified content\nLine 2\nLine 3 changed\n");

    // Output file path outside the repo
    let output_file = output_dir.path().join("diff_output.txt");
    let output_str = output_file.to_str().unwrap();

    // Run diff command with output to file
    let args = DiffArgs::parse_from(["diff", "--algorithm", "histogram", "--output", output_str]);
    diff::execute(args).await;

    // Verify the output file exists
    assert!(
        fs::metadata(&output_file).is_ok(),
        "Output file should exist"
    );

    // Read the file content to make sure it contains diff output
    let content = fs::read_to_string(&output_file).unwrap();
    assert!(
        content.contains("diff --git"),
        "Output should contain diff header"
    );
}

#[tokio::test]
#[serial]
/// Tests diff with different algorithms
async fn test_diff_algorithms() {
    let test_dir = tempdir().unwrap();
    let output_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a file with some content to make a non-trivial diff
    create_file(
        "file1.txt",
        "Line 1\nLine 2\nLine 3\nLine 4\nLine 5\nLine 6\nLine 7\n",
    );

    add::execute(AddArgs {
        pathspec: vec![String::from("file1.txt")],
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

    // Make complex changes to test different algorithms
    modify_file(
        "file1.txt",
        "Line 1\nModified Line\nLine 3\nNew Line\nLine 5\nLine 6\nDeleted Line 7\n",
    );

    // Test histogram algorithm
    let histogram_file = output_dir.path().join("histogram_diff.txt");
    let histogram_str = histogram_file.to_str().unwrap();
    let args = DiffArgs::parse_from([
        "diff",
        "--algorithm",
        "histogram",
        "--output",
        histogram_str,
    ]);
    diff::execute(args).await;

    // Non-default algorithms are accepted by clap for forward
    // compatibility but fail closed until the backend is actually wired.
    let myers_file = output_dir.path().join("myers_diff.txt");
    let myers_str = myers_file.to_str().unwrap();
    let args = DiffArgs::parse_from(["diff", "--algorithm", "myers", "--output", myers_str]);
    let myers_result = diff::execute_safe(args, &OutputConfig::default()).await;

    let myers_min_file = output_dir.path().join("myersMinimal_diff.txt");
    let myers_min_str = myers_min_file.to_str().unwrap();
    let args = DiffArgs::parse_from([
        "diff",
        "--algorithm",
        "myersMinimal",
        "--output",
        myers_min_str,
    ]);
    let myers_min_result = diff::execute_safe(args, &OutputConfig::default()).await;

    assert!(
        fs::metadata(&histogram_file).is_ok(),
        "Histogram output file should exist"
    );
    assert!(
        myers_result.is_err(),
        "Myers should fail closed until a real backend is wired"
    );
    assert!(
        myers_min_result.is_err(),
        "MyersMinimal should fail closed until a real backend is wired"
    );
    assert!(
        !myers_file.exists(),
        "unsupported Myers should not write a default diff to the output file"
    );
    assert!(
        !myers_min_file.exists(),
        "unsupported MyersMinimal should not write a default diff to the output file"
    );
}

#[test]
fn test_diff_summary_lists_creates_and_deletes() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // Commit a file we will later delete.
    std::fs::write(p.join("old.txt"), "old\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "old.txt"], p), "add old.txt");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "seed", "--no-verify"], p),
        "commit seed",
    );

    // Stage a created file and a deletion.
    std::fs::write(p.join("new.txt"), "new\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "new.txt"], p), "add new.txt");
    std::fs::remove_file(p.join("old.txt")).unwrap();
    assert_cli_success(&run_libra_command(&["add", "old.txt"], p), "stage deletion");

    let out = run_libra_command(&["diff", "--cached", "--summary"], p);
    assert_cli_success(&out, "diff --cached --summary");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.lines().any(|l| l == " create mode 100644 new.txt"),
        "summary lists the created file in git's format: {s:?}"
    );
    assert!(
        s.lines().any(|l| l == " delete mode 100644 old.txt"),
        "summary lists the deleted file in git's format: {s:?}"
    );
}

#[test]
fn test_diff_shortstat_exit_code_and_no_patch() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("tracked.txt"), "tracked\nupdated line\n").unwrap();

    // --shortstat: just the trailing summary line (no per-file rows).
    let ss = run_libra_command(&["diff", "--shortstat"], p);
    assert!(ss.status.success(), "shortstat exits 0 without --exit-code");
    let s = String::from_utf8_lossy(&ss.stdout);
    assert!(s.contains("1 file changed"), "shortstat summary: {s:?}");
    assert!(
        !s.contains(" | "),
        "shortstat omits the per-file rows: {s:?}"
    );
    assert_eq!(
        s.lines().filter(|l| !l.trim().is_empty()).count(),
        1,
        "shortstat is a single line: {s:?}"
    );

    // --exit-code: still prints the diff, but exits 1 when there are changes.
    let ec = run_libra_command(&["diff", "--exit-code"], p);
    assert_eq!(ec.status.code(), Some(1), "exit-code is 1 when changed");
    assert!(
        !String::from_utf8_lossy(&ec.stdout).trim().is_empty(),
        "--exit-code still prints the diff body"
    );

    // -s / --no-patch: suppress the body; exit 0 without --exit-code.
    let no_patch = run_libra_command(&["diff", "-s"], p);
    assert!(no_patch.status.success(), "--no-patch exits 0 on its own");
    assert!(
        String::from_utf8_lossy(&no_patch.stdout).trim().is_empty(),
        "--no-patch suppresses the diff body"
    );

    // -s --exit-code: no body, exit 1.
    let both = run_libra_command(&["diff", "-s", "--exit-code"], p);
    assert_eq!(both.status.code(), Some(1), "--no-patch + --exit-code = 1");
    assert!(
        String::from_utf8_lossy(&both.stdout).trim().is_empty(),
        "--no-patch still suppresses the body with --exit-code"
    );

    // --exit-code applies in JSON mode too: still emit JSON, but exit 1.
    let json = run_libra_command(&["--json", "diff", "--exit-code"], p);
    assert_eq!(
        json.status.code(),
        Some(1),
        "--json --exit-code exits 1 on changes"
    );
    assert!(
        String::from_utf8_lossy(&json.stdout).contains("\"files_changed\""),
        "--json --exit-code still emits the JSON payload"
    );
}

#[test]
fn test_diff_z_nul_terminates_name_outputs() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("tracked.txt"), "tracked\nchanged\n").unwrap();

    // --name-only -z: each path NUL-terminated, no trailing newline.
    let no = run_libra_command(&["diff", "--name-only", "-z"], p);
    assert!(no.status.success(), "name-only -z ok");
    assert_eq!(
        no.stdout, b"tracked.txt\0",
        "name-only -z framing: {:?}",
        no.stdout
    );

    // --name-status -z: status and path as separate NUL fields.
    let ns = run_libra_command(&["diff", "--name-status", "-z"], p);
    assert!(ns.status.success(), "name-status -z ok");
    assert_eq!(
        ns.stdout, b"M\0tracked.txt\0",
        "name-status -z framing: {:?}",
        ns.stdout
    );

    // Without -z, the same query is newline-terminated (sanity check).
    let plain = run_libra_command(&["diff", "--name-only"], p);
    assert_eq!(
        plain.stdout, b"tracked.txt\n",
        "name-only plain framing: {:?}",
        plain.stdout
    );
}

#[test]
fn test_diff_check_reports_whitespace_errors() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("ws.txt"), "clean\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "ws.txt"], p), "add base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );

    // Stage a change with trailing whitespace (line 2) and space-before-tab (line 3).
    std::fs::write(p.join("ws.txt"), "clean\ntrailing   \n \tindent\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "ws.txt"], p), "stage ws");

    let check = run_libra_command(&["diff", "--cached", "--check"], p);
    assert_eq!(
        check.status.code(),
        Some(2),
        "diff --check exits 2 when problems are found"
    );
    let out = String::from_utf8_lossy(&check.stdout);
    assert!(
        out.contains("ws.txt:2: trailing whitespace"),
        "trailing ws: {out:?}"
    );
    assert!(
        out.contains("ws.txt:3: space before tab in indent"),
        "space-before-tab: {out:?}"
    );

    // A clean staged change reports nothing and exits 0.
    std::fs::write(p.join("ws.txt"), "clean\ntidy line\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "ws.txt"], p), "stage tidy");
    let clean = run_libra_command(&["diff", "--cached", "--check"], p);
    assert_cli_success(&clean, "diff --check (clean) exits 0");
    assert!(
        String::from_utf8_lossy(&clean.stdout).trim().is_empty(),
        "no warnings for a clean diff"
    );
}

#[test]
fn test_diff_reverse_swaps_sides() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("r.txt"), "line1\nline2\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "r.txt"], p), "add base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    std::fs::write(p.join("r.txt"), "line1\nCHANGED\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "r.txt"], p), "stage change");

    // Normal staged diff: line2 removed, CHANGED added.
    let normal = run_libra_command(&["diff", "--cached"], p);
    assert_cli_success(&normal, "diff --cached");
    let n = String::from_utf8_lossy(&normal.stdout);
    assert!(n.contains("-line2"), "normal removes line2: {n:?}");
    assert!(n.contains("+CHANGED"), "normal adds CHANGED: {n:?}");

    // Reverse: the sides swap, so CHANGED is removed and line2 is added.
    let reverse = run_libra_command(&["diff", "--cached", "-R"], p);
    assert_cli_success(&reverse, "diff --cached -R");
    let r = String::from_utf8_lossy(&reverse.stdout);
    assert!(r.contains("-CHANGED"), "reverse removes CHANGED: {r:?}");
    assert!(r.contains("+line2"), "reverse adds line2: {r:?}");
}

#[test]
fn diff_text_flag_forces_content_for_binary() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // Stage a change including a NUL byte (Git/Libra call this "binary").
    std::fs::write(p.join("data.bin"), b"line\x00\x01\x02\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "data.bin"], p),
        "stage data.bin",
    );

    // By default a binary file shows the "Binary files … differ" line, not content.
    let plain = run_libra_command(&["diff", "--cached"], p);
    assert_cli_success(&plain, "diff --cached");
    let plain_out = String::from_utf8_lossy(&plain.stdout);
    assert!(
        plain_out.contains("Binary files") && !plain_out.contains("\n@@ "),
        "a binary file shows 'Binary files … differ', not a content diff: {plain_out:?}"
    );

    // `--text` / `-a` force the content diff (a hunk with the raw bytes).
    for flag in ["--text", "-a"] {
        let out = run_libra_command(&["diff", "--cached", flag], p);
        assert_cli_success(&out, &format!("diff --cached {flag}"));
        let s = String::from_utf8_lossy(&out.stdout);
        assert!(
            s.contains("@@ ") && !s.contains("Binary files"),
            "diff --cached {flag} forces a content diff: {s:?}"
        );
    }

    // `--text` also forces content for a NON-UTF-8 file that git_internal collapsed
    // to a bare `Binary files differ` (a distinguishable lossy-UTF-8 change shows).
    std::fs::write(p.join("nonutf8"), b"\xfflead\nfoo\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "nonutf8"], p), "stage nonutf8");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "nonutf8", "--no-verify"], p),
        "commit nonutf8",
    );
    std::fs::write(p.join("nonutf8"), b"\xfflead\nbar\n").unwrap();
    assert!(
        String::from_utf8_lossy(&run_libra_command(&["diff", "nonutf8"], p).stdout)
            .contains("Binary files"),
        "non-UTF-8 file is binary by default"
    );
    let forced = run_libra_command(&["diff", "--text", "nonutf8"], p);
    let fs = String::from_utf8_lossy(&forced.stdout);
    assert!(
        fs.contains("-foo") && fs.contains("+bar") && !fs.contains("Binary files"),
        "--text forces a (lossy) content diff for a non-UTF-8 file: {fs:?}"
    );
}

#[test]
fn diff_no_ext_diff_flag_is_accepted_noop() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("e.txt"), "x\ny\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "e.txt"], p), "stage e.txt");

    let plain = run_libra_command(&["diff", "--cached"], p);
    assert_cli_success(&plain, "diff --cached");
    // `--no-ext-diff` is accepted and produces identical output: Libra has no
    // external diff drivers, so it always uses the built-in diff engine.
    let out = run_libra_command(&["diff", "--cached", "--no-ext-diff"], p);
    assert_cli_success(&out, "diff --cached --no-ext-diff");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&plain.stdout),
        "diff --no-ext-diff matches plain diff (no-op)"
    );
}

#[test]
fn diff_no_color_moved_flag_is_accepted_noop() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("m.txt"), "a\nb\nc\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "m.txt"], p), "stage m.txt");

    let plain = run_libra_command(&["diff", "--cached"], p);
    assert_cli_success(&plain, "diff --cached");
    // `--no-color-moved` is accepted and a no-op: Libra's diff never colors
    // moved lines, so the output is identical.
    let out = run_libra_command(&["diff", "--cached", "--no-color-moved"], p);
    assert_cli_success(&out, "diff --cached --no-color-moved");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&plain.stdout),
        "diff --no-color-moved matches plain diff (no-op)"
    );
}

#[test]
fn diff_rename_relative_indent_noop_flags_are_accepted() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("n.txt"), "x\ny\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "n.txt"], p), "stage n.txt");

    let plain = run_libra_command(&["diff", "--cached"], p);
    assert_cli_success(&plain, "diff --cached");
    let plain_out = String::from_utf8_lossy(&plain.stdout);
    // These negating flags leave this single-add diff unchanged: there is no
    // rename candidate, paths are already repo-root-relative, and Libra applies
    // no indent heuristic. `--no-relative` overriding `--relative` is covered by
    // `test_diff_relative_filters_and_strips_prefix`.
    for flag in ["--no-renames", "--no-relative", "--no-indent-heuristic"] {
        let out = run_libra_command(&["diff", "--cached", flag], p);
        assert_cli_success(&out, &format!("diff --cached {flag}"));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            plain_out,
            "diff {flag} matches plain diff (no-op)"
        );
    }
}

#[test]
fn diff_no_textconv_flag_is_accepted_noop() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("t.txt"), "x\ny\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "t.txt"], p), "stage t.txt");

    let plain = run_libra_command(&["diff", "--cached"], p);
    assert_cli_success(&plain, "diff --cached");
    // `--no-textconv` is an accepted no-op: Libra's diff has no textconv filters
    // and always diffs raw content, so output is unchanged.
    let out = run_libra_command(&["diff", "--cached", "--no-textconv"], p);
    assert_cli_success(&out, "diff --cached --no-textconv");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&plain.stdout),
        "diff --no-textconv matches plain diff (no-op)"
    );
}

#[test]
fn test_diff_unified_context_controls_surrounding_lines() {
    // `-U<n>` sets the number of context lines around each change in the patch.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let ten: String = (1..=10).map(|i| format!("line{i}\n")).collect();
    std::fs::write(p.join("f.txt"), &ten).unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add f");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "f", "--no-verify"], p),
        "commit f",
    );
    // Change a single line in the middle.
    std::fs::write(p.join("f.txt"), ten.replace("line5\n", "line5-CHANGED\n")).unwrap();

    let ctx_lines = |s: &str| s.lines().filter(|l| l.starts_with(' ')).count();

    // -U0: no surrounding context.
    let s0 = String::from_utf8_lossy(&run_libra_command(&["diff", "-U0", "f.txt"], p).stdout)
        .into_owned();
    assert_eq!(ctx_lines(&s0), 0, "-U0 must have no context lines:\n{s0}");
    assert!(
        s0.contains("-line5") && s0.contains("+line5-CHANGED"),
        "-U0 still shows the change:\n{s0}"
    );

    // -U1: exactly one context line on each side.
    let s1 = String::from_utf8_lossy(&run_libra_command(&["diff", "-U1", "f.txt"], p).stdout)
        .into_owned();
    assert_eq!(
        ctx_lines(&s1),
        2,
        "-U1 must have 1 context line each side:\n{s1}"
    );

    // Default (3 context each side).
    let sd = String::from_utf8_lossy(&run_libra_command(&["diff", "f.txt"], p).stdout).into_owned();
    assert_eq!(
        ctx_lines(&sd),
        6,
        "default must have 3 context each side:\n{sd}"
    );

    // -U5: clamped to the file (4 lines before + 5 after the change).
    let s5 = String::from_utf8_lossy(&run_libra_command(&["diff", "-U5", "f.txt"], p).stdout)
        .into_owned();
    assert_eq!(
        ctx_lines(&s5),
        9,
        "-U5 clamps to file bounds (4 + 5):\n{s5}"
    );

    // `--unified=N` long form is equivalent to `-U N`.
    let s1_long =
        String::from_utf8_lossy(&run_libra_command(&["diff", "--unified=1", "f.txt"], p).stdout)
            .into_owned();
    assert_eq!(s1, s1_long, "--unified=N must equal -U N");
}

#[test]
fn test_diff_unified_zero_context_anchors_pure_insert_delete() {
    // At -U0 there are no context lines, so a pure insert/delete produces a hunk
    // with a zero-count side; its header must anchor at the adjacent line, exactly
    // as Git does (`@@ -k,0 …` / `… +k,0 @@`, and `-0,0` / `+0,0` at start of file).
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let commit_f = |content: &str, msg: &str| {
        std::fs::write(p.join("f.txt"), content).unwrap();
        assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add f");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", msg, "--no-verify"], p),
            "commit f",
        );
    };
    let diff_u0 = || {
        String::from_utf8_lossy(&run_libra_command(&["diff", "-U0", "f.txt"], p).stdout)
            .into_owned()
    };

    // Pure insert in the middle: anchor at old line 2 (the line before the insert).
    commit_f("a\nb\nc\n", "base1");
    std::fs::write(p.join("f.txt"), "a\nb\nX\nc\n").unwrap();
    let s = diff_u0();
    assert!(
        s.contains("@@ -2,0 +3,1 @@"),
        "insert-middle anchors at -2,0:\n{s}"
    );

    // Pure insert at the very start: anchor at -0,0.
    commit_f("a\nb\n", "base2");
    std::fs::write(p.join("f.txt"), "X\na\nb\n").unwrap();
    let s = diff_u0();
    assert!(
        s.contains("@@ -0,0 +1,1 @@"),
        "insert-at-start anchors at -0,0:\n{s}"
    );

    // Pure delete in the middle: anchor the empty new side at new line 1.
    commit_f("a\nb\nc\n", "base3");
    std::fs::write(p.join("f.txt"), "a\nc\n").unwrap();
    let s = diff_u0();
    assert!(
        s.contains("@@ -2,1 +1,0 @@"),
        "delete-middle anchors at +1,0:\n{s}"
    );

    // Whole new file (HEAD vs staged): the empty old side anchors at -0,0.
    std::fs::write(p.join("new.txt"), "X\nY\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "new.txt"], p), "add new");
    let sn = String::from_utf8_lossy(
        &run_libra_command(&["diff", "--staged", "-U0", "new.txt"], p).stdout,
    )
    .into_owned();
    assert!(
        sn.contains("@@ -0,0 +1,2 @@"),
        "new file anchors at -0,0:\n{sn}"
    );

    // Two pure hunks separated by exactly one unchanged line. At -U0 the separator
    // is dropped (not emitted as context), but must still advance the anchor so the
    // second hunk is not stale. old a,b,c -> new b,X,c: delete `a`, keep `b`, then
    // insert `X` after it.
    commit_f("a\nb\nc\n", "base4");
    std::fs::write(p.join("f.txt"), "b\nX\nc\n").unwrap();
    let s = diff_u0();
    assert!(
        s.contains("@@ -1,1 +0,0 @@"),
        "first hunk deletes `a` at -1,1 +0,0:\n{s}"
    );
    assert!(
        s.contains("@@ -2,0 +2,1 @@"),
        "second hunk anchors after the separator line (-2,0 +2,1):\n{s}"
    );
}

#[test]
fn test_diff_ignore_all_space_drops_whitespace_only_and_rediffs() {
    // `-w` ignores whitespace when comparing: a whitespace-only change is not
    // reported (file drops out), context lines come from the new side, and
    // counts/name/numstat all reflect the whitespace-ignored re-diff.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let commit_f = |content: &str, msg: &str| {
        std::fs::write(p.join("f.txt"), content).unwrap();
        assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add f");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", msg, "--no-verify"], p),
            "commit f",
        );
    };
    let out =
        |args: &[&str]| String::from_utf8_lossy(&run_libra_command(args, p).stdout).into_owned();

    // 1) Whitespace-only change → plain diff shows it, `-w` shows nothing.
    commit_f("alpha\nbeta\ngamma\n", "base1");
    std::fs::write(p.join("f.txt"), "alpha   \n  beta\ngamma\t\n").unwrap();
    assert!(
        out(&["diff", "f.txt"]).contains("alpha"),
        "plain diff shows the whitespace change"
    );
    assert!(
        out(&["diff", "-w", "f.txt"]).trim().is_empty(),
        "-w drops a whitespace-only change"
    );
    assert!(
        out(&["diff", "-w", "--name-only", "f.txt"])
            .trim()
            .is_empty(),
        "-w --name-only drops the whitespace-only file"
    );

    // 2) Whitespace-only context line + a real change → shown; context from new side.
    commit_f("a  \nFOO\nc\n", "base2");
    std::fs::write(p.join("f.txt"), "a\nBAR\nc\n").unwrap();
    let w2 = out(&["diff", "-w", "f.txt"]);
    assert!(
        w2.contains("-FOO") && w2.contains("+BAR"),
        "real change shown:\n{w2}"
    );
    assert!(
        w2.contains("\n a\n"),
        "context line shown from the new side (no trailing spaces):\n{w2}"
    );
    assert!(
        !w2.contains("a  "),
        "old-side trailing whitespace not shown:\n{w2}"
    );

    // 3) `-w` honors `-U0` (no context lines).
    let w3 = out(&["diff", "-w", "-U0", "f.txt"]);
    assert!(
        w3.contains("-FOO") && w3.contains("+BAR"),
        "-w -U0 shows the change:\n{w3}"
    );
    assert!(
        !w3.contains("\n a\n") && !w3.contains("\n c\n"),
        "-w -U0 emits no context lines:\n{w3}"
    );

    // 4) `--numstat` under `-w` counts only the real change (1 insertion, 1 deletion).
    assert!(
        out(&["diff", "-w", "--numstat", "f.txt"])
            .lines()
            .any(|l| l == "1\t1\tf.txt"),
        "-w numstat counts only the real change"
    );
}

#[test]
fn test_diff_ignore_space_change_and_ignore_space_at_eol() {
    // `-b` ignores changes in the AMOUNT of whitespace (runs collapse to one
    // space, trailing dropped) but the PRESENCE of whitespace still matters;
    // `--ignore-space-at-eol` ignores only trailing whitespace.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let commit_f = |content: &str, msg: &str| {
        std::fs::write(p.join("f.txt"), content).unwrap();
        assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add f");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", msg, "--no-verify"], p),
            "commit f",
        );
    };
    let out =
        |args: &[&str]| String::from_utf8_lossy(&run_libra_command(args, p).stdout).into_owned();

    // -b: collapsing "foo  bar" -> "foo bar" is a whitespace-amount-only change → ignored.
    commit_f("foo  bar\nbaz\n", "b1");
    std::fs::write(p.join("f.txt"), "foo bar\nbaz\n").unwrap();
    assert!(
        out(&["diff", "f.txt"]).contains("foo"),
        "plain diff shows the change"
    );
    assert!(
        out(&["diff", "-b", "f.txt"]).trim().is_empty(),
        "-b ignores a whitespace-amount-only change"
    );

    // -b: removing internal whitespace entirely IS a real change.
    commit_f("foo bar\n", "b2");
    std::fs::write(p.join("f.txt"), "foobar\n").unwrap();
    let b = out(&["diff", "-b", "f.txt"]);
    assert!(
        b.contains("-foo bar") && b.contains("+foobar"),
        "-b still reports removing internal whitespace:\n{b}"
    );

    // --ignore-space-at-eol: trailing-only change is ignored.
    commit_f("alpha\nbeta\n", "e1");
    std::fs::write(p.join("f.txt"), "alpha   \nbeta\n").unwrap();
    assert!(
        out(&["diff", "--ignore-space-at-eol", "f.txt"])
            .trim()
            .is_empty(),
        "--ignore-space-at-eol ignores a trailing-only change"
    );

    // --ignore-space-at-eol does NOT ignore internal whitespace, but -b does.
    commit_f("a b\n", "e2");
    std::fs::write(p.join("f.txt"), "a  b\n").unwrap();
    assert!(
        out(&["diff", "--ignore-space-at-eol", "f.txt"]).contains("a  b"),
        "--ignore-space-at-eol keeps an internal-whitespace change"
    );
    assert!(
        out(&["diff", "-b", "f.txt"]).trim().is_empty(),
        "-b ignores the same internal-amount change"
    );

    // -b leading whitespace: the amount/kind of a leading run is ignored, but
    // adding leading whitespace where there was none is a real change.
    commit_f("  a\n", "lead1");
    std::fs::write(p.join("f.txt"), " a\n").unwrap();
    assert!(
        out(&["diff", "-b", "f.txt"]).trim().is_empty(),
        "-b ignores a leading-whitespace-amount change"
    );
    commit_f("\ta\n", "lead2");
    std::fs::write(p.join("f.txt"), "    a\n").unwrap();
    assert!(
        out(&["diff", "-b", "f.txt"]).trim().is_empty(),
        "-b treats a leading tab and leading spaces as equal"
    );
    commit_f("a\n", "lead3");
    std::fs::write(p.join("f.txt"), "  a\n").unwrap();
    assert!(
        out(&["diff", "-b", "f.txt"]).contains("+  a"),
        "-b reports adding leading whitespace where there was none"
    );
    assert!(
        out(&["diff", "-w", "f.txt"]).trim().is_empty(),
        "-w ignores adding leading whitespace (presence-insensitive)"
    );

    // Precedence `-w` > `-b` > `--ignore-space-at-eol` for an internal-amount-only
    // change: `--ignore-space-at-eol` alone keeps it; combined with `-w` or `-b`
    // the stronger flag wins and the file drops.
    commit_f("x  y\n", "prec1");
    std::fs::write(p.join("f.txt"), "x y\n").unwrap();
    assert!(
        !out(&["diff", "--ignore-space-at-eol", "f.txt"])
            .trim()
            .is_empty(),
        "--ignore-space-at-eol alone keeps an internal-amount change"
    );
    assert!(
        out(&["diff", "-w", "--ignore-space-at-eol", "f.txt"])
            .trim()
            .is_empty(),
        "-w takes precedence over --ignore-space-at-eol"
    );
    assert!(
        out(&["diff", "-b", "--ignore-space-at-eol", "f.txt"])
            .trim()
            .is_empty(),
        "-b takes precedence over --ignore-space-at-eol"
    );

    // `--check` ignores the whitespace-ignore flags (matches Git): a line that
    // gains trailing whitespace is still flagged with -w/-b/--ignore-space-at-eol,
    // and the whitespace post-pass must NOT drop the file out from under --check.
    commit_f("hello\n", "chk1");
    std::fs::write(p.join("f.txt"), "hello   \n").unwrap();
    for flag in ["--ignore-space-at-eol", "-w", "-b"] {
        let chk = run_libra_command(&["diff", "--check", flag, "f.txt"], p);
        assert_eq!(
            chk.status.code(),
            Some(2),
            "diff --check {flag} must still flag added trailing whitespace (exit 2)"
        );
        assert!(
            String::from_utf8_lossy(&chk.stdout).contains("trailing whitespace"),
            "diff --check {flag} must report the trailing-whitespace error"
        );
    }
}

#[test]
fn test_diff_ignore_blank_lines() {
    // End-to-end `--ignore-blank-lines`, validated against real `git` behavior:
    // a blank far from any real change is suppressed (with the kept hunk's line
    // numbers shifted by it), a blank near a real change rides along, a blank-only
    // change drops the file, and a whitespace-only line is NOT treated as blank.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let commit_f = |content: &str, msg: &str| {
        fs::write(p.join("f.txt"), content).unwrap();
        assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add f");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", msg, "--no-verify"], p),
            "commit f",
        );
    };
    let out =
        |args: &[&str]| String::from_utf8_lossy(&run_libra_command(args, p).stdout).into_owned();

    // Far blank (gap 6 > ctx) is suppressed; only the h->H hunk survives, with the
    // new-side start shifted by the ignored blank (Git: `@@ -5,4 +6,4 @@`).
    commit_f("a\nb\nc\nd\ne\nf\ng\nh\n", "base1");
    fs::write(p.join("f.txt"), "a\n\nb\nc\nd\ne\nf\ng\nH\n").unwrap();
    let far = out(&["diff", "--ignore-blank-lines", "f.txt"]);
    assert!(
        far.contains("@@ -5,4 +6,4 @@"),
        "far blank suppressed, hunk shifted:\n{far}"
    );
    assert!(
        far.contains("-h") && far.contains("+H"),
        "real change shown:\n{far}"
    );
    assert!(
        !far.lines().any(|l| l == "+"),
        "far blank line not emitted:\n{far}"
    );

    // In-window blank (gap 1 < ctx=2) rides along with the a->A change.
    commit_f("a\nb\nc\nd\n", "base2");
    fs::write(p.join("f.txt"), "A\nb\n\nc\nd\n").unwrap();
    let near = out(&["diff", "--ignore-blank-lines", "-U2", "f.txt"]);
    assert!(
        near.contains("@@ -1,4 +1,5 @@"),
        "in-window blank merges:\n{near}"
    );
    assert!(
        near.lines().any(|l| l == "+"),
        "in-window blank is shown:\n{near}"
    );

    // A change that is only an added blank line -> the file drops out.
    commit_f("x\ny\n", "base3");
    fs::write(p.join("f.txt"), "x\n\ny\n").unwrap();
    assert!(
        out(&["diff", "f.txt"]).contains("@@"),
        "plain diff shows the blank add"
    );
    assert!(
        out(&["diff", "--ignore-blank-lines", "f.txt"])
            .trim()
            .is_empty(),
        "blank-only change drops the file"
    );
    assert!(
        out(&["diff", "--ignore-blank-lines", "--name-only", "f.txt"])
            .trim()
            .is_empty(),
        "blank-only change drops from --name-only too"
    );

    // A whitespace-only added line is NOT blank -> still shown.
    commit_f("a\nb\n", "base4");
    fs::write(p.join("f.txt"), "a\n  \nb\n").unwrap();
    assert!(
        !out(&["diff", "--ignore-blank-lines", "f.txt"])
            .trim()
            .is_empty(),
        "whitespace-only line is not ignored"
    );
}

#[test]
fn test_diff_ignore_blank_lines_keeps_added_blank_only_file() {
    // An added file whose entire content is blank lines is still reported as a
    // file-level change (in --name-only and --stat, with zero counts and no hunk),
    // matching `git diff --ignore-blank-lines` — only a modification with no
    // surviving change disappears.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("blankonly.txt"), "\n\n\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "blankonly.txt"], p),
        "add blank file",
    );

    let name_only = String::from_utf8_lossy(
        &run_libra_command(
            &["diff", "--ignore-blank-lines", "--name-only", "--staged"],
            p,
        )
        .stdout,
    )
    .into_owned();
    assert!(
        name_only.lines().any(|l| l == "blankonly.txt"),
        "added blank-only file still appears in --name-only:\n{name_only}"
    );

    let stat = String::from_utf8_lossy(
        &run_libra_command(&["diff", "--ignore-blank-lines", "--stat", "--staged"], p).stdout,
    )
    .into_owned();
    assert!(
        stat.contains("blankonly.txt") && stat.contains("1 file changed"),
        "added blank-only file appears in --stat with zero counts:\n{stat}"
    );
}

#[test]
fn test_diff_ignore_blank_lines_modification_with_header_like_content_is_dropped() {
    // A MODIFICATION whose only surviving change is blank lines must drop out, even
    // when its content contains header-like text such as "new file mode 100644".
    // Guards against classifying it as an add/delete via a raw substring scan.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(
        p.join("m.txt"),
        "new file mode 100644\n--- /dev/null\nbody\n",
    )
    .unwrap();
    assert_cli_success(&run_libra_command(&["add", "m.txt"], p), "add m");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit m",
    );
    // Modify by inserting only blank lines.
    std::fs::write(
        p.join("m.txt"),
        "new file mode 100644\n\n--- /dev/null\n\nbody\n",
    )
    .unwrap();
    let plain =
        String::from_utf8_lossy(&run_libra_command(&["diff", "m.txt"], p).stdout).into_owned();
    assert!(
        plain.contains("@@"),
        "plain diff shows the blank insertions:\n{plain}"
    );
    let ibl = String::from_utf8_lossy(
        &run_libra_command(&["diff", "--ignore-blank-lines", "m.txt"], p).stdout,
    )
    .into_owned();
    assert!(
        ibl.trim().is_empty(),
        "a modification with only blank changes is dropped despite header-like content:\n{ibl}"
    );
    let names = String::from_utf8_lossy(
        &run_libra_command(&["diff", "--ignore-blank-lines", "--name-only", "m.txt"], p).stdout,
    )
    .into_owned();
    assert!(
        names.trim().is_empty(),
        "and does not appear in --name-only:\n{names}"
    );
}

/// `--relative[=<path>]` restricts the diff to a directory and strips that prefix
/// from displayed paths (matching Git), while `--no-relative` keeps full paths.
#[test]
#[serial]
fn test_diff_relative_filters_and_strips_prefix() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::create_dir_all(p.join("sub/deep")).unwrap();
    fs::create_dir_all(p.join("other")).unwrap();
    fs::write(p.join("sub/f.txt"), "a\n").unwrap();
    fs::write(p.join("sub/deep/g.txt"), "x\n").unwrap();
    fs::write(p.join("other/h.txt"), "o\n").unwrap();
    fs::write(p.join("root.txt"), "r\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "."], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );
    fs::write(p.join("sub/f.txt"), "a\nb\n").unwrap();
    fs::write(p.join("sub/deep/g.txt"), "x\ny\n").unwrap();
    fs::write(p.join("other/h.txt"), "o\np\n").unwrap();
    fs::write(p.join("root.txt"), "r\ns\n").unwrap();

    // --relative=sub: only sub/ files, prefix stripped, no `sub/` and no other/root.
    let rel = run_libra_command(&["diff", "--relative=sub"], p);
    assert_cli_success(&rel, "diff --relative=sub");
    let out = String::from_utf8_lossy(&rel.stdout);
    assert!(
        out.contains("diff --git a/f.txt b/f.txt"),
        "stripped f.txt: {out}"
    );
    assert!(
        out.contains("diff --git a/deep/g.txt b/deep/g.txt"),
        "stripped deep/g.txt: {out}"
    );
    assert!(
        !out.contains("sub/"),
        "the sub/ prefix is stripped everywhere: {out}"
    );
    assert!(
        !out.contains("other/h.txt"),
        "files outside sub/ are excluded: {out}"
    );
    assert!(!out.contains("root.txt"), "root files are excluded: {out}");

    // --relative=sub/deep: only the nested file, stripped to its basename.
    let deep = run_libra_command(&["diff", "--relative=sub/deep"], p);
    assert_cli_success(&deep, "diff --relative=sub/deep");
    let deep_out = String::from_utf8_lossy(&deep.stdout);
    assert!(
        deep_out.contains("diff --git a/g.txt b/g.txt"),
        "deep stripped: {deep_out}"
    );
    assert!(
        !deep_out.contains("f.txt"),
        "only sub/deep is included: {deep_out}"
    );

    // --no-relative keeps full repo-root-relative paths.
    let no_rel = run_libra_command(&["diff", "--no-relative"], p);
    assert_cli_success(&no_rel, "diff --no-relative");
    let no_rel_out = String::from_utf8_lossy(&no_rel.stdout);
    assert!(
        no_rel_out.contains("a/sub/f.txt") && no_rel_out.contains("a/other/h.txt"),
        "no-relative shows full paths: {no_rel_out}"
    );

    // --no-relative overrides an earlier --relative (no clap conflict, full paths).
    let both = run_libra_command(&["diff", "--relative=sub", "--no-relative"], p);
    assert_cli_success(&both, "diff --relative=sub --no-relative");
    assert!(
        String::from_utf8_lossy(&both.stdout).contains("a/sub/f.txt"),
        "--no-relative overrides --relative: {}",
        String::from_utf8_lossy(&both.stdout)
    );
}

/// `--relative` strips the prefix from BOTH `a/` and `b/` path positions even when the
/// filename contains a space (exact-path replacement, not a ` b/` split), so the
/// `diff --git`/`---`/`+++` headers stay consistent.
#[test]
#[serial]
fn test_diff_relative_strips_space_containing_path() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::create_dir_all(p.join("sub")).unwrap();
    fs::write(p.join("sub/a b.txt"), "x\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "."], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );
    fs::write(p.join("sub/a b.txt"), "x\ny\n").unwrap();

    let out = run_libra_command(&["diff", "--relative=sub"], p);
    assert_cli_success(&out, "diff --relative=sub (space path)");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("diff --git a/a b.txt b/a b.txt"),
        "both path positions stripped without corruption: {text}"
    );
    assert!(text.contains("--- a/a b.txt"), "--- stripped: {text}");
    assert!(text.contains("+++ b/a b.txt"), "+++ stripped: {text}");
    assert!(!text.contains("sub/"), "no residual sub/ prefix: {text}");
}

/// `--relative` also strips the prefix from the `<LargeFile>` marker emitted for
/// over-large files, so the patch output is consistent with `file.path`/`--stat`.
#[test]
#[serial]
fn test_diff_relative_strips_large_file_marker() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::create_dir_all(p.join("sub")).unwrap();
    // > MAX_DIFF_LINES (10k) across both sides triggers the large-file marker.
    let base: String = (0..7000).map(|i| format!("{i}\n")).collect();
    fs::write(p.join("sub/big.txt"), &base).unwrap();
    assert_cli_success(&run_libra_command(&["add", "."], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );
    fs::write(p.join("sub/big.txt"), format!("{base}EXTRA\n")).unwrap();

    let out = run_libra_command(&["diff", "--relative=sub"], p);
    assert_cli_success(&out, "diff --relative=sub (large file)");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("<LargeFile>big.txt:"),
        "large-file marker prefix stripped: {text}"
    );
    assert!(
        !text.contains("sub/big.txt"),
        "no residual sub/ in marker: {text}"
    );
}

/// `diff --word-diff` re-renders the patch at word granularity: `plain`
/// (default) wraps removed words in `[-…-]` and added in `{+…+}`, `porcelain`
/// emits one token per line, and `none` is a regular line patch.
#[test]
fn test_diff_word_diff_modes() {
    use std::fs;

    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    fs::write(p.join("w.txt"), "alpha beta gamma\n").expect("write w");
    assert_cli_success(&run_libra_command(&["add", "w.txt"], p), "add w");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "w1", "--no-verify"], p),
        "commit w1",
    );
    // A single-word substitution is unambiguous, so it matches git exactly.
    fs::write(p.join("w.txt"), "alpha BETA gamma\n").expect("modify w");

    let body = |args: &[&str]| -> String {
        let mut full = vec!["diff"];
        full.extend_from_slice(args);
        full.push("w.txt");
        let out = run_libra_command(&full, p);
        assert_cli_success(&out, "diff word-diff");
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        // Keep from the first hunk header onward (skip the file headers).
        match s.find("@@") {
            Some(i) => s[i..].to_string(),
            None => s,
        }
    };

    // plain (default): the changed word is bracketed inline; the rest is plain.
    let plain = body(&["--word-diff"]);
    assert!(
        plain.contains("alpha [-beta-]{+BETA+} gamma"),
        "plain word-diff: {plain}"
    );
    // `--word-diff=plain` is identical to the default.
    assert_eq!(plain, body(&["--word-diff=plain"]));

    // Plain mode stays bracketed even when global color is forced. Previously
    // terminal/color state leaked into the renderer and silently changed plain
    // mode into a bracket-less color diff.
    let forced_plain =
        run_libra_command(&["--color=always", "diff", "--word-diff=plain", "w.txt"], p);
    assert_cli_success(&forced_plain, "forced-color plain word-diff");
    let forced_plain = String::from_utf8_lossy(&forced_plain.stdout);
    assert!(
        forced_plain.contains("[-beta-]{+BETA+}"),
        "plain mode remains bracketed: {forced_plain}"
    );
    assert!(
        !forced_plain.contains("\u{1b}["),
        "plain word-diff bypasses line colorization: {forced_plain}"
    );

    // The bare Git-compatible shorthand selects color word diff and forces
    // word colors under the automatic policy even though the test captures
    // (redirects) stdout. Explicit global --color=never remains authoritative.
    let color_words = run_libra_command(&["diff", "--color-words", "w.txt"], p);
    assert_cli_success(&color_words, "diff --color-words");
    let color_words = String::from_utf8_lossy(&color_words.stdout);
    assert!(
        color_words.contains("\u{1b}["),
        "--color-words forces ANSI under auto: {color_words}"
    );
    assert!(
        !color_words.contains("[-") && !color_words.contains("{+"),
        "color shorthand has no plain markers: {color_words}"
    );
    let no_color_words = run_libra_command(&["--color=never", "diff", "--color-words", "w.txt"], p);
    assert_cli_success(&no_color_words, "diff --color=never --color-words");
    assert!(
        !String::from_utf8_lossy(&no_color_words.stdout).contains("\u{1b}["),
        "explicit --color=never suppresses shorthand ANSI"
    );

    // Regex-valued shorthand is deliberately deferred to the
    // --word-diff-regex slice; reject it instead of silently ignoring a value.
    let color_words_regex = run_libra_command(&["diff", "--color-words=\\w+", "w.txt"], p);
    assert_eq!(
        color_words_regex.status.code(),
        Some(129),
        "unsupported regex-valued shorthand must fail: {}",
        String::from_utf8_lossy(&color_words_regex.stderr)
    );

    // porcelain: one token per line with ` `/`-`/`+` prefixes and `~` newlines.
    let porcelain = body(&["--word-diff=porcelain"]);
    assert!(
        porcelain.contains("\n-beta\n"),
        "porcelain removed: {porcelain}"
    );
    assert!(
        porcelain.contains("\n+BETA\n"),
        "porcelain added: {porcelain}"
    );
    assert!(
        porcelain.contains("\n~\n") || porcelain.ends_with("~\n"),
        "porcelain newline marker: {porcelain}"
    );
    assert!(
        !porcelain.contains("[-"),
        "porcelain has no brackets: {porcelain}"
    );

    // none: a regular line patch (no word markers).
    let none = body(&["--word-diff=none"]);
    assert!(
        none.contains("-alpha beta gamma"),
        "none removed line: {none}"
    );
    assert!(
        none.contains("+alpha BETA gamma"),
        "none added line: {none}"
    );
    assert!(!none.contains("[-"), "none has no word markers: {none}");

    // Whitespace is a delimiter: an inserted word keeps the surrounding spaces
    // outside the markers (Git renders `a {+c+} b`, not `a {+c +}b`).
    fs::write(p.join("ws.txt"), "a b\n").expect("write ws");
    assert_cli_success(&run_libra_command(&["add", "ws.txt"], p), "add ws");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ws1", "--no-verify"], p),
        "commit ws1",
    );
    fs::write(p.join("ws.txt"), "a c b\n").expect("modify ws");
    let ws = run_libra_command(&["diff", "--word-diff", "ws.txt"], p);
    assert_cli_success(&ws, "diff --word-diff ws");
    assert!(
        String::from_utf8_lossy(&ws.stdout).contains("a {+c+} b"),
        "delimiter spaces stay outside the marker: {}",
        String::from_utf8_lossy(&ws.stdout)
    );

    // An invalid mode is a usage error.
    let bad = run_libra_command(&["diff", "--word-diff=bogus", "w.txt"], p);
    assert_eq!(
        bad.status.code(),
        Some(129),
        "invalid --word-diff mode: {}",
        String::from_utf8_lossy(&bad.stderr)
    );

    // `--check` ignores `--word-diff` and still scans the real patch for
    // whitespace errors (so word-diff cannot mask a `--check` failure).
    fs::write(p.join("bad.txt"), "ok\n").expect("write bad");
    assert_cli_success(&run_libra_command(&["add", "bad.txt"], p), "add bad");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "b1", "--no-verify"], p),
        "commit b1",
    );
    fs::write(p.join("bad.txt"), "ok\ntrailing   \n").expect("trailing ws");
    assert_cli_success(&run_libra_command(&["add", "bad.txt"], p), "stage bad");
    let check = run_libra_command(
        &["diff", "--check", "--word-diff", "--cached", "bad.txt"],
        p,
    );
    assert_eq!(
        check.status.code(),
        Some(2),
        "--check --word-diff still flags whitespace errors: {} / {}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
}

/// `diff.external` routes each file's patch through the configured driver
/// verbatim (even when its output resembles built-in metadata). The
/// GIT_EXTERNAL_DIFF protocol reports an all-zero worktree hash;
/// `--no-ext-diff` falls back to the internal patch and `--stat` bypasses it.
#[cfg(unix)]
#[test]
fn test_diff_external_driver_replaces_patch() {
    use std::os::unix::fs::PermissionsExt;

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("f.txt"), "l1\nl2\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add f.txt");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    fs::write(p.join("f.txt"), "l1\nCHANGED\n").unwrap();

    let driver = p.join("driver.sh");
    fs::write(
        &driver,
        "#!/bin/sh\nprintf 'diff --git a/f.txt b/f.txt\\n--- a/f.txt\\n+++ b/f.txt\\nnewhex=%s\\n\\n' \"$6\"\n",
    )
    .unwrap();
    fs::set_permissions(&driver, fs::Permissions::from_mode(0o755)).unwrap();
    assert_cli_success(
        &run_libra_command(
            &["config", "set", "diff.external", driver.to_str().unwrap()],
            p,
        ),
        "set diff.external",
    );
    assert_cli_success(
        &run_libra_command(&["config", "diff.srcPrefix", "OLD/"], p),
        "set diff.srcPrefix",
    );
    assert_cli_success(
        &run_libra_command(&["config", "diff.dstPrefix", "NEW/"], p),
        "set diff.dstPrefix",
    );

    // Patch output is produced by the external driver.
    let out = run_libra_command(&["diff", "f.txt"], p);
    assert_cli_success(&out, "external diff");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.as_bytes()
            .starts_with(b"diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n"),
        "external driver metadata-looking lines must remain byte-for-byte verbatim: {s:?}"
    );
    assert!(
        s.contains("newhex=000000"),
        "worktree hash is all-zero: {s:?}"
    );
    assert!(
        s.ends_with("\n\n"),
        "external trailing bytes are verbatim: {s:?}"
    );
    assert!(!s.contains("OLD/") && !s.contains("NEW/"), "{s:?}");

    // `--no-ext-diff` restores the internal patch.
    let internal = run_libra_command(&["diff", "--no-ext-diff", "f.txt"], p);
    assert!(
        String::from_utf8_lossy(&internal.stdout).contains("diff --git OLD/f.txt NEW/f.txt"),
        "--no-ext-diff uses the internal diff"
    );

    // `--stat` bypasses the external driver entirely.
    let stat = run_libra_command(&["diff", "--stat", "f.txt"], p);
    let st = String::from_utf8_lossy(&stat.stdout);
    assert!(
        st.contains("f.txt") && !st.contains("EXTDIFF"),
        "--stat bypasses the external driver: {st}"
    );
}

/// `--json` and `--quiet` bypass the external diff driver (structured/suppressed
/// output uses the internal engine), and a driver that exits non-zero is a fatal
/// error rather than a silent empty diff.
#[cfg(unix)]
#[test]
fn test_diff_external_driver_gating_and_failure() {
    use std::os::unix::fs::PermissionsExt;

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("f.txt"), "l1\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit",
    );
    fs::write(p.join("f.txt"), "l2\n").unwrap();

    let driver = p.join("d.sh");
    fs::write(&driver, "#!/bin/sh\necho EXTDIFF\n").unwrap();
    fs::set_permissions(&driver, fs::Permissions::from_mode(0o755)).unwrap();
    assert_cli_success(
        &run_libra_command(
            &["config", "set", "diff.external", driver.to_str().unwrap()],
            p,
        ),
        "set diff.external",
    );

    // --json keeps the internal structured diff (does not run the driver).
    let json = run_libra_command(&["--json", "diff", "f.txt"], p);
    let s = String::from_utf8_lossy(&json.stdout);
    assert!(
        !s.contains("EXTDIFF"),
        "JSON mode must not run the external driver: {s}"
    );

    // A failing driver is fatal.
    let failing = p.join("fail.sh");
    fs::write(&failing, "#!/bin/sh\necho boom >&2\nexit 3\n").unwrap();
    fs::set_permissions(&failing, fs::Permissions::from_mode(0o755)).unwrap();
    assert_cli_success(
        &run_libra_command(
            &["config", "set", "diff.external", failing.to_str().unwrap()],
            p,
        ),
        "set failing driver",
    );
    let out = run_libra_command(&["diff", "f.txt"], p);
    assert!(
        !out.status.success(),
        "a failing external driver must be fatal"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("external diff driver"),
        "the driver failure is surfaced: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `-M`/`--find-renames` folds a staged delete+add pair into a single rename
/// entry: an inexact rename carries `similarity index N%` + the content diff,
/// and the name-status / numstat / summary surfaces render `R<score>` and the
/// `old => new` path (with Git's directory brace-compaction).
#[test]
fn test_diff_rename_detection_surfaces() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::create_dir_all(p.join("src")).unwrap();
    fs::write(p.join("src/old.txt"), "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "src/old.txt"], p), "add old");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    // Rename + a one-line edit, both staged.
    fs::remove_file(p.join("src/old.txt")).unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "src/old.txt"], p),
        "stage deletion",
    );
    fs::write(p.join("src/new.txt"), "a\nb\nZ\nd\ne\nf\ng\nh\ni\nj\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "src/new.txt"], p), "add new");

    // Patch: rename headers + similarity + the content hunk.
    let patch = run_libra_command(&["diff", "-M", "--cached"], p);
    assert_cli_success(&patch, "diff -M");
    let ps = String::from_utf8_lossy(&patch.stdout);
    assert!(
        ps.contains("diff --git a/src/old.txt b/src/new.txt")
            && ps.contains("similarity index 90%")
            && ps.contains("rename from src/old.txt")
            && ps.contains("rename to src/new.txt")
            && ps.contains("-c\n+Z"),
        "rename patch with content diff: {ps}"
    );

    // name-status: R<score> old new (no brace compaction).
    let ns = run_libra_command(&["diff", "-M", "--cached", "--name-status"], p);
    assert_eq!(
        String::from_utf8_lossy(&ns.stdout).trim_end(),
        "R090\tsrc/old.txt\tsrc/new.txt",
    );

    // numstat + summary use Git's directory brace-compaction.
    let num = run_libra_command(&["diff", "-M", "--cached", "--numstat"], p);
    assert_eq!(
        String::from_utf8_lossy(&num.stdout).trim_end(),
        "1\t1\tsrc/{old.txt => new.txt}",
    );
    let summary = run_libra_command(&["diff", "-M", "--cached", "--summary"], p);
    assert_eq!(
        String::from_utf8_lossy(&summary.stdout).trim_end(),
        " rename src/{old.txt => new.txt} (90%)",
    );

    // JSON serializes a rename as status=renamed + rename_from + similarity.
    let json = run_libra_command(&["--json", "diff", "-M", "--cached"], p);
    let js = String::from_utf8_lossy(&json.stdout);
    assert!(
        js.contains("\"status\": \"renamed\"")
            && js.contains("\"rename_from\": \"src/old.txt\"")
            && js.contains("\"path\": \"src/new.txt\"")
            && js.contains("\"similarity\": 90"),
        "JSON exposes the rename metadata: {js}"
    );
}

/// `-M100` only matches identical content (an edited file is not a rename), and
/// `--no-renames` countermands `-M`, restoring the separate add + delete.
#[test]
fn test_diff_rename_threshold_and_no_renames() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("old.txt"), "a\nb\nc\nd\ne\nf\ng\nh\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "old.txt"], p), "add old");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    fs::remove_file(p.join("old.txt")).unwrap();
    assert_cli_success(&run_libra_command(&["add", "old.txt"], p), "stage deletion");
    fs::write(p.join("new.txt"), "a\nb\nZ\nd\ne\nf\ng\nh\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "new.txt"], p), "add new");

    // `-M100%` is exact-only: 87% < 100% → not a rename. (Note `-M100` without
    // the `%` is 10%, matching Git's `0.<digits>` reading.)
    let strict = run_libra_command(&["diff", "-M100%", "--cached"], p);
    let ss = String::from_utf8_lossy(&strict.stdout);
    assert!(
        ss.contains("deleted file mode") && ss.contains("new file mode") && !ss.contains("rename"),
        "-M100% leaves an edited file as add + delete: {ss}"
    );

    // `-M100` (no `%`) is a 10% threshold → the 87% edit IS a rename.
    let lenient = run_libra_command(&["diff", "-M100", "--cached"], p);
    assert!(
        String::from_utf8_lossy(&lenient.stdout).contains("rename from old.txt"),
        "-M100 is 10%, so an 87% edit is a rename"
    );

    // Invalid score is a usage error, not a silent default.
    let bad = run_libra_command(&["diff", "-Mnope", "--cached"], p);
    assert!(
        !bad.status.success()
            && String::from_utf8_lossy(&bad.stderr).contains("invalid argument to find-renames"),
        "an invalid -M argument is rejected"
    );

    // `-M0` maps to the 50% default (like Git), so the 87% edit is a rename.
    let zero = run_libra_command(&["diff", "-M0", "--cached"], p);
    assert!(
        String::from_utf8_lossy(&zero.stdout).contains("rename from old.txt"),
        "-M0 uses the 50% default, not a 0% match-everything threshold"
    );

    // `--no-renames` after `-M` turns detection back off.
    let off = run_libra_command(&["diff", "-M", "--no-renames", "--cached"], p);
    assert!(
        !String::from_utf8_lossy(&off.stdout).contains("rename"),
        "--no-renames countermands -M"
    );
}

/// Textconv: a file whose `diff=<driver>` attribute names a driver with a
/// `diff.<driver>.textconv` command has each side converted before diffing. It is
/// on by default; `--no-textconv` diffs the raw bytes.
#[cfg(unix)]
#[test]
fn test_diff_textconv() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // `*.dat` files diff through the `upper` driver, which uppercases content.
    fs::write(p.join(".libra_attributes"), "*.dat diff=upper\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["config", "set", "diff.upper.textconv", "tr a-z A-Z <"], p),
        "set textconv",
    );
    fs::write(p.join("f.dat"), "hello\nworld\nfoo\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "f.dat", ".libra_attributes"], p),
        "add",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit",
    );
    fs::write(p.join("f.dat"), "hello\nplanet\nfoo\n").unwrap();

    // Default: the diff is of the uppercased (textconv'd) content.
    let conv = run_libra_command(&["diff", "f.dat"], p);
    let cs = String::from_utf8_lossy(&conv.stdout);
    assert!(
        cs.contains(" HELLO") && cs.contains("-WORLD") && cs.contains("+PLANET"),
        "textconv converts both sides before diffing: {cs}"
    );

    // `--no-textconv`: raw content.
    let raw = run_libra_command(&["diff", "--no-textconv", "f.dat"], p);
    let rs = String::from_utf8_lossy(&raw.stdout);
    assert!(
        rs.contains("-world") && rs.contains("+planet") && !rs.contains("PLANET"),
        "--no-textconv diffs the raw bytes: {rs}"
    );

    // A later `-diff` clears the driver (Git's last-match-wins): `z.dat` is not
    // textconv'd even though `*.dat diff=upper` would otherwise match it.
    fs::write(
        p.join(".libra_attributes"),
        "*.dat diff=upper\nz.dat -diff\n",
    )
    .unwrap();
    fs::write(p.join("z.dat"), "low\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", ".libra_attributes", "z.dat"], p),
        "add z",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "z", "--no-verify"], p),
        "commit z",
    );
    fs::write(p.join("z.dat"), "high\n").unwrap();
    let cleared = run_libra_command(&["diff", "z.dat"], p);
    let zs = String::from_utf8_lossy(&cleared.stdout);
    assert!(
        zs.contains("-low") && zs.contains("+high") && !zs.contains("LOW"),
        "a later -diff clears the textconv driver: {zs}"
    );

    // A detected rename of a textconv'd file converts its content body too.
    fs::write(p.join("r.dat"), "aa\nbb\ncc\ndd\nee\nff\ngg\nhh\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "r.dat"], p), "add r");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "r", "--no-verify"], p),
        "commit r",
    );
    fs::remove_file(p.join("r.dat")).unwrap();
    assert_cli_success(&run_libra_command(&["add", "r.dat"], p), "stage r deletion");
    fs::write(p.join("r2.dat"), "aa\nbb\nXX\ndd\nee\nff\ngg\nhh\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "r2.dat"], p), "add r2");
    let rn = run_libra_command(&["diff", "-M", "--cached", "--", "r.dat", "r2.dat"], p);
    let rns = String::from_utf8_lossy(&rn.stdout);
    assert!(
        rns.contains("rename from r.dat") && rns.contains("-CC") && rns.contains("+XX"),
        "a renamed textconv'd file has its body converted, keeping rename headers: {rns}"
    );

    // A failing textconv command is fatal (like Git's "unable to read files to
    // diff"), not a silent fall-back to raw bytes.
    fs::write(
        p.join(".libra_attributes"),
        "*.dat diff=upper\nb.dat diff=bad\n",
    )
    .unwrap();
    assert_cli_success(
        &run_libra_command(&["config", "set", "diff.bad.textconv", "false"], p),
        "set failing textconv",
    );
    fs::write(p.join("b.dat"), "x\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", ".libra_attributes", "b.dat"], p),
        "add b",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "b", "--no-verify"], p),
        "commit b",
    );
    fs::write(p.join("b.dat"), "y\n").unwrap();
    let failing = run_libra_command(&["diff", "b.dat"], p);
    assert!(
        !failing.status.success()
            && String::from_utf8_lossy(&failing.stderr).contains("textconv filter"),
        "a failing textconv command is fatal: {}",
        String::from_utf8_lossy(&failing.stderr)
    );

    // A rename across drivers resolves each side independently: `*.foo`
    // uppercases, `*.bar` has no driver, so a `old.foo -> new.bar` rename shows
    // the uppercased old side and the raw new side.
    fs::write(p.join(".libra_attributes"), "*.foo diff=upper\n").unwrap();
    fs::write(p.join("old.foo"), "aa\nbb\ncc\ndd\nee\nff\ngg\nhh\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", ".libra_attributes", "old.foo"], p),
        "add foo",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "foo", "--no-verify"], p),
        "commit foo",
    );
    fs::remove_file(p.join("old.foo")).unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "old.foo"], p),
        "stage foo deletion",
    );
    fs::write(p.join("new.bar"), "aa\nbb\nzz\ndd\nee\nff\ngg\nhh\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "new.bar"], p), "add bar");
    let xr = run_libra_command(&["diff", "-M", "--cached", "--", "old.foo", "new.bar"], p);
    let xrs = String::from_utf8_lossy(&xr.stdout);
    assert!(
        xrs.contains("rename from old.foo") && xrs.contains("-AA") && xrs.contains("+zz"),
        "per-side drivers: old side uppercased, new side raw: {xrs}"
    );

    // An EXACT rename (identical raw bytes → no content hunk) across differing
    // drivers must still synthesize a body when the converted sides differ.
    fs::write(p.join("ex.foo"), "kk\nll\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "ex.foo"], p), "add ex.foo");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ex", "--no-verify"], p),
        "commit ex",
    );
    fs::remove_file(p.join("ex.foo")).unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "ex.foo"], p),
        "stage ex deletion",
    );
    fs::write(p.join("ex.bar"), "kk\nll\n").unwrap(); // identical raw content
    assert_cli_success(&run_libra_command(&["add", "ex.bar"], p), "add ex.bar");
    let exact = run_libra_command(&["diff", "-M", "--cached", "--", "ex.foo", "ex.bar"], p);
    let exs = String::from_utf8_lossy(&exact.stdout);
    assert!(
        exs.contains("similarity index 100%") && exs.contains("-KK") && exs.contains("+kk"),
        "exact rename across drivers synthesizes a converted body: {exs}"
    );

    // A textconv whose output is non-empty for EMPTY input must not fabricate
    // content for the missing side of an added file.
    assert_cli_success(
        &run_libra_command(&["config", "set", "diff.pre.textconv", "sed s/^/L:/"], p),
        "set prefix textconv",
    );
    fs::write(p.join(".libra_attributes"), "*.pre diff=pre\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", ".libra_attributes"], p),
        "add attrs",
    );
    fs::write(p.join("n.pre"), "one\ntwo\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "n.pre"], p), "add n.pre");
    let added = run_libra_command(&["diff", "--cached", "n.pre"], p);
    let adds = String::from_utf8_lossy(&added.stdout);
    assert!(
        adds.contains("+L:one") && !adds.lines().any(|l| l.starts_with("-L:")),
        "an added file's absent old side is not textconv'd into fake removals: {adds}"
    );
}

/// A normal textconv'd file whose CONTENT contains a literal `<LargeFile>` line
/// must still be converted — the over-large sentinel is only matched as a line
/// prefix, and hunk content is `+`/`-`/space-prefixed, so it never matches.
#[cfg(unix)]
#[test]
fn test_diff_textconv_literal_largefile_content() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join(".libra_attributes"), "*.dat diff=upper\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["config", "set", "diff.upper.textconv", "tr a-z A-Z <"], p),
        "set textconv",
    );
    fs::write(p.join("x.dat"), "<LargeFile>abc\nkeep\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "x.dat", ".libra_attributes"], p),
        "add",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit",
    );
    fs::write(p.join("x.dat"), "<LargeFile>xyz\nkeep\n").unwrap();
    let out = run_libra_command(&["diff", "x.dat"], p);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("-<LARGEFILE>ABC") && s.contains("+<LARGEFILE>XYZ"),
        "a literal <LargeFile> content line is still textconv'd: {s}"
    );
}

/// A file with a NUL byte is detected as binary: it shows `Binary files … differ`
/// by default, `Bin <old> -> <new> bytes` under `--stat`, `-`/`-` under
/// `--numstat`, and a `GIT binary patch` (full-index header + base85 `literal`
/// chunks) under `--binary`.
#[test]
fn test_diff_binary() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("b.bin"), b"A\x00B\x00C\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.bin"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit",
    );
    fs::write(p.join("b.bin"), b"A\x00X\x00Y\x00Z\n").unwrap();

    // Default: Binary files differ, no content hunk.
    let def = run_libra_command(&["diff", "b.bin"], p);
    let ds = String::from_utf8_lossy(&def.stdout);
    assert!(
        ds.contains("Binary files a/b.bin and b/b.bin differ") && !ds.contains("@@ "),
        "default shows Binary files differ: {ds:?}"
    );

    // --stat: Bin <old> -> <new> bytes.
    let stat = run_libra_command(&["diff", "--stat", "b.bin"], p);
    assert!(
        String::from_utf8_lossy(&stat.stdout).contains("b.bin | Bin 6 -> 8 bytes"),
        "--stat shows Bin sizes: {:?}",
        String::from_utf8_lossy(&stat.stdout)
    );

    // --numstat: dashes for binary.
    let num = run_libra_command(&["diff", "--numstat", "b.bin"], p);
    assert!(
        String::from_utf8_lossy(&num.stdout).contains("-\t-\tb.bin"),
        "--numstat shows -/- for binary: {:?}",
        String::from_utf8_lossy(&num.stdout)
    );

    // --binary: GIT binary patch with a full-index header and forward+reverse
    // literal chunks.
    let bin = run_libra_command(&["diff", "--binary", "b.bin"], p);
    let bs = String::from_utf8_lossy(&bin.stdout);
    assert!(
        bs.contains("GIT binary patch")
            && bs.contains("literal 8")
            && bs.contains("literal 6")
            // full (40-hex sha1) index, not the abbreviated 7-char form
            && bs.lines().any(|l| l.starts_with("index ") && l.len() > 80),
        "--binary emits a GIT binary patch with full index: {bs:?}"
    );
    // The patch must end with the blank-line terminator Git's parser requires
    // (so `git apply` accepts it); the renderer must not trim it.
    assert!(
        bs.ends_with("\n\n"),
        "--binary patch keeps its trailing blank-line terminator: {bs:?}"
    );

    // A non-UTF-8 file with NO NUL is still detected (git_internal collapses it to
    // a bare marker) and reformatted to the full "Binary files a/… and b/…" form.
    fs::write(p.join("u.bin"), b"\xff\xfe\xfd\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "u.bin"], p), "add u");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "u", "--no-verify"], p),
        "commit u",
    );
    fs::write(p.join("u.bin"), b"\xff\xfe\xfc\n").unwrap();
    let nonutf8 = run_libra_command(&["diff", "u.bin"], p);
    assert!(
        String::from_utf8_lossy(&nonutf8.stdout)
            .contains("Binary files a/u.bin and b/u.bin differ"),
        "non-UTF-8 binary uses the full a/… b/… label form: {:?}",
        String::from_utf8_lossy(&nonutf8.stdout)
    );

    // `--binary` is `--full-index` for text files in the same diff too.
    fs::write(p.join("t.txt"), "one\ntwo\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "t.txt"], p), "add t");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "t", "--no-verify"], p),
        "commit t",
    );
    fs::write(p.join("t.txt"), "one\nTWO\n").unwrap();
    let txt = run_libra_command(&["diff", "--binary", "t.txt"], p);
    assert!(
        String::from_utf8_lossy(&txt.stdout)
            .lines()
            .any(|l| l.starts_with("index ") && l.len() > 80),
        "--binary full-indexes text files too: {:?}",
        String::from_utf8_lossy(&txt.stdout)
    );

    // A TEXT file whose content contains a literal `Binary files differ` line must
    // NOT be misdetected as binary (the bare marker is matched exactly, and a
    // context line is `  `/`+`/`-`-prefixed).
    fs::write(p.join("c.txt"), "Binary files differ\nkeep\nold\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "c.txt"], p), "add c");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c", "--no-verify"], p),
        "commit c",
    );
    fs::write(p.join("c.txt"), "Binary files differ\nkeep\nnew\n").unwrap();
    let ctx = run_libra_command(&["diff", "c.txt"], p);
    let cx = String::from_utf8_lossy(&ctx.stdout);
    assert!(
        cx.contains("-old") && cx.contains("+new") && !cx.contains("a/c.txt and b/c.txt differ"),
        "a text file with a 'Binary files differ' context line stays a content diff: {cx:?}"
    );
}

/// A detected rename (`-M`) of a non-UTF-8 binary file (no NUL) is shown as a
/// rename + `Binary files … differ`, not a lossy content diff — the rename body
/// was reconstructed via lossy UTF-8, so detection scans the actual blob bytes.
#[test]
fn test_diff_binary_rename() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // 10 non-UTF-8 lines; rename + change one line → high similarity.
    let mut old = Vec::new();
    for _ in 0..10 {
        old.extend_from_slice(b"\xff\xfe\xfd\n");
    }
    fs::write(p.join("old.bin"), &old).unwrap();
    assert_cli_success(&run_libra_command(&["add", "old.bin"], p), "add old");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    fs::remove_file(p.join("old.bin")).unwrap();
    assert_cli_success(&run_libra_command(&["add", "old.bin"], p), "stage deletion");
    let mut new = Vec::new();
    for _ in 0..9 {
        new.extend_from_slice(b"\xff\xfe\xfd\n");
    }
    new.extend_from_slice(b"\xaa\xbb\xcc\n");
    fs::write(p.join("new.bin"), &new).unwrap();
    assert_cli_success(&run_libra_command(&["add", "new.bin"], p), "add new");

    let out = run_libra_command(&["diff", "-M", "--cached"], p);
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("rename from old.bin")
            && s.contains("Binary files a/old.bin and b/new.bin differ")
            && !s.contains("@@ "),
        "a non-UTF-8 binary rename shows rename headers + Binary files differ: {s:?}"
    );

    // An EXACT binary rename (identical bytes) is header-only — no "Binary files
    // … differ" body (matching Git, which shows similarity 100% + rename headers).
    fs::write(p.join("ex.bin"), b"\xff\xfe\xfd\x00\xfc\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "ex.bin"], p), "add ex");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ex", "--no-verify"], p),
        "commit ex",
    );
    fs::remove_file(p.join("ex.bin")).unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "ex.bin"], p),
        "stage ex deletion",
    );
    fs::write(p.join("ex2.bin"), b"\xff\xfe\xfd\x00\xfc\n").unwrap(); // identical bytes
    assert_cli_success(&run_libra_command(&["add", "ex2.bin"], p), "add ex2");
    let exact = run_libra_command(&["diff", "-M", "--cached", "--", "ex.bin", "ex2.bin"], p);
    let es = String::from_utf8_lossy(&exact.stdout);
    assert!(
        es.contains("similarity index 100%")
            && es.contains("rename from ex.bin")
            && !es.contains("Binary files"),
        "an exact binary rename is header-only, no Binary-files body: {es:?}"
    );
    // It is still binary metadata: `--stat` shows a bare `Bin` (no sizes, since the
    // content is unchanged), `--numstat` shows `-`/`-`.
    let xstat = run_libra_command(
        &[
            "diff", "-M", "--cached", "--stat", "--", "ex.bin", "ex2.bin",
        ],
        p,
    );
    assert!(
        String::from_utf8_lossy(&xstat.stdout).contains("ex.bin => ex2.bin | Bin\n"),
        "exact binary rename --stat is a bare `Bin`: {:?}",
        String::from_utf8_lossy(&xstat.stdout)
    );
    let xnum = run_libra_command(
        &[
            "diff",
            "-M",
            "--cached",
            "--numstat",
            "--",
            "ex.bin",
            "ex2.bin",
        ],
        p,
    );
    assert!(
        String::from_utf8_lossy(&xnum.stdout).contains("-\t-\tex.bin => ex2.bin"),
        "exact binary rename --numstat is `-`/`-`: {:?}",
        String::from_utf8_lossy(&xnum.stdout)
    );
}

/// `--color-moved` colors moved lines (removed in one place, added in another)
/// with a distinct color under `--color=always`; `--color=never`, omitting the
/// flag, or `--no-color-moved` leaves them as normal add/remove colors. An
/// invalid mode is a usage error.
#[test]
fn test_diff_color_moved() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("f.txt"), "keepA\nkeepB\nblock1\nblock2\nblock3\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit",
    );
    // Move keepA/keepB to the end.
    fs::write(p.join("f.txt"), "block1\nblock2\nblock3\nkeepA\nkeepB\n").unwrap();

    // `--color=always --color-moved=plain`: moved lines get the bold magenta
    // (removed, `1;35`) / bold cyan (added, `1;36`) move colors.
    let moved = run_libra_command(
        &["diff", "--color=always", "--color-moved=plain", "f.txt"],
        p,
    );
    let ms = String::from_utf8_lossy(&moved.stdout);
    assert!(
        ms.contains("\u{1b}[1;35m") && ms.contains("\u{1b}[1;36m"),
        "moved lines are bold magenta/cyan: {ms:?}"
    );

    // Without --color-moved, moved lines use the normal red/green.
    let plain = run_libra_command(&["diff", "--color=always", "f.txt"], p);
    let ps = String::from_utf8_lossy(&plain.stdout);
    assert!(
        !ps.contains("\u{1b}[1;35m") && ps.contains("\u{1b}[31m") && ps.contains("\u{1b}[32m"),
        "without --color-moved, normal red/green: {ps:?}"
    );

    // `--color=never` suppresses all color, including move color.
    let never = run_libra_command(
        &["diff", "--color=never", "--color-moved=plain", "f.txt"],
        p,
    );
    assert!(
        !String::from_utf8_lossy(&never.stdout).contains("\u{1b}["),
        "--color=never emits no ANSI"
    );

    // An invalid mode is a usage error.
    let bad = run_libra_command(&["diff", "--color-moved=bogus", "f.txt"], p);
    assert!(
        !bad.status.success()
            && String::from_utf8_lossy(&bad.stderr).contains("invalid argument to color-moved"),
        "invalid --color-moved mode is rejected"
    );

    // Bare `--color-moved` followed by a pathspec must NOT swallow the pathspec as
    // the mode (`require_equals` makes the value `=`-attached only).
    let bare = run_libra_command(&["diff", "--color=always", "--color-moved", "f.txt"], p);
    assert!(
        bare.status.success(),
        "bare --color-moved + pathspec is not a usage error: {}",
        String::from_utf8_lossy(&bare.stderr)
    );

    // A moved line whose content begins with `--` (rendered `---…` for the
    // removal) is still detected/colored, not mistaken for a `--- a/<path>` header.
    fs::write(p.join("g.txt"), "--dashline\nkeepX\nkeepY\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "g.txt"], p), "add g");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "g", "--no-verify"], p),
        "commit g",
    );
    fs::write(p.join("g.txt"), "keepX\nkeepY\n--dashline\n").unwrap();
    let dash = run_libra_command(
        &["diff", "--color=always", "--color-moved=plain", "g.txt"],
        p,
    );
    let ds = String::from_utf8_lossy(&dash.stdout);
    assert!(
        ds.contains("\u{1b}[1;35m---dashline") && ds.contains("\u{1b}[1;36m+--dashline"),
        "a `--`-prefixed moved body line is colored as moved, not skipped: {ds:?}"
    );
}

/// `-M --relative=<dir>` strips the directory prefix from BOTH sides of a rename
/// — the `a/`/`rename from`/`--- ` old-side headers and the `rename_from` field,
/// not just the new path — so no header keeps the stripped prefix.
#[test]
fn test_diff_rename_relative_strips_both_sides() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::create_dir_all(p.join("sub")).unwrap();
    fs::write(p.join("sub/old.txt"), "a\nb\nc\nd\ne\nf\ng\nh\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "sub/old.txt"], p), "add old");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    fs::remove_file(p.join("sub/old.txt")).unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "sub/old.txt"], p),
        "stage deletion",
    );
    fs::write(p.join("sub/new.txt"), "a\nb\nZ\nd\ne\nf\ng\nh\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "sub/new.txt"], p), "add new");

    let patch = run_libra_command(&["diff", "-M", "--relative=sub", "--cached"], p);
    let ps = String::from_utf8_lossy(&patch.stdout);
    assert!(
        ps.contains("diff --git a/old.txt b/new.txt")
            && ps.contains("rename from old.txt")
            && ps.contains("--- a/old.txt")
            && !ps.contains("sub/"),
        "both rename sides are stripped in the patch headers: {ps}"
    );
    let ns = run_libra_command(
        &["diff", "-M", "--relative=sub", "--cached", "--name-status"],
        p,
    );
    assert_eq!(
        String::from_utf8_lossy(&ns.stdout).trim_end(),
        "R087\told.txt\tnew.txt",
    );
}

/// A rename that straddles the `--relative` boundary is NOT folded: the prefix
/// restriction is applied before rename pairing (like Git), so the in-prefix side
/// shows as a plain add or delete.
#[test]
fn test_diff_rename_relative_boundary_is_not_a_rename() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::create_dir_all(p.join("sub")).unwrap();
    // old.txt (outside sub) renamed to sub/new.txt (inside sub).
    fs::write(p.join("old.txt"), "a\nb\nc\nd\ne\nf\ng\nh\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "old.txt"], p), "add old");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    fs::remove_file(p.join("old.txt")).unwrap();
    assert_cli_success(&run_libra_command(&["add", "old.txt"], p), "stage deletion");
    fs::write(p.join("sub/new.txt"), "a\nb\nZ\nd\ne\nf\ng\nh\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "sub/new.txt"], p), "add new");

    // Under --relative=sub only the in-prefix new side survives, as a plain add.
    let ns = run_libra_command(
        &["diff", "-M", "--relative=sub", "--cached", "--name-status"],
        p,
    );
    assert_eq!(String::from_utf8_lossy(&ns.stdout).trim_end(), "A\tnew.txt");
}

/// A reordered-but-same-content rename scores 100% (the chunk multiset is equal),
/// yet the blobs differ: `-M` reports it as a rename WITH a content body (like
/// Git), while `-M100%` is exact-only and leaves it as add + delete.
#[test]
fn test_diff_rename_full_similarity_non_identical() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("old.txt"), "a\nb\nc\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "old.txt"], p), "add old");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    fs::remove_file(p.join("old.txt")).unwrap();
    assert_cli_success(&run_libra_command(&["add", "old.txt"], p), "stage deletion");
    fs::write(p.join("new.txt"), "c\nb\na\n").unwrap(); // reordered: same multiset
    assert_cli_success(&run_libra_command(&["add", "new.txt"], p), "add new");

    // `-M`: a 100%-similar but non-identical rename still shows its body.
    let m = run_libra_command(&["diff", "-M", "--cached"], p);
    let ms = String::from_utf8_lossy(&m.stdout);
    assert!(
        ms.contains("similarity index 100%")
            && ms.contains("rename from old.txt")
            && ms.contains("\n@@ "),
        "a reordered 100% rename still carries a content body: {ms}"
    );

    // `-M100%` is exact-only: reordered content is not byte-identical → add+delete.
    let exact = run_libra_command(&["diff", "-M100%", "--cached"], p);
    let es = String::from_utf8_lossy(&exact.stdout);
    assert!(
        !es.contains("rename") && es.contains("new file mode") && es.contains("deleted file mode"),
        "-M100% does not fold a non-identical pair: {es}"
    );
}

#[test]
fn test_diff_ignore_cr_at_eol() {
    // `--ignore-cr-at-eol` (lore.md §1.4): a CRLF↔LF-only change drops out;
    // trailing-space or mid-line `\r` changes still show; stronger whitespace
    // flags take precedence.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let commit_f = |content: &str, msg: &str| {
        std::fs::write(p.join("f.txt"), content).unwrap();
        assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add f");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", msg, "--no-verify"], p),
            "commit f",
        );
    };
    let out =
        |args: &[&str]| String::from_utf8_lossy(&run_libra_command(args, p).stdout).into_owned();

    // 1) LF → CRLF only: plain diff shows it, the flag drops the file entirely.
    commit_f("alpha\nbeta\ngamma\n", "base lf");
    std::fs::write(p.join("f.txt"), "alpha\r\nbeta\r\ngamma\r\n").unwrap();
    assert!(
        !out(&["diff", "f.txt"]).trim().is_empty(),
        "plain diff shows the CRLF-only change"
    );
    assert!(
        out(&["diff", "--ignore-cr-at-eol", "f.txt"])
            .trim()
            .is_empty(),
        "--ignore-cr-at-eol drops a CRLF-only change"
    );
    assert!(
        out(&["diff", "--ignore-cr-at-eol", "--numstat", "f.txt"])
            .trim()
            .is_empty(),
        "numstat reflects the re-diff (no changes)"
    );
    let exit = run_libra_command(&["diff", "--ignore-cr-at-eol", "--exit-code", "f.txt"], p);
    assert_eq!(
        exit.status.code(),
        Some(0),
        "--exit-code sees no differences under the flag"
    );

    // 2) Trailing-space-only change STILL shows (distinguishes from
    //    --ignore-space-at-eol, which would drop it).
    commit_f("one\ntwo\n", "base spaces");
    std::fs::write(p.join("f.txt"), "one   \ntwo\n").unwrap();
    assert!(
        !out(&["diff", "--ignore-cr-at-eol", "f.txt"])
            .trim()
            .is_empty(),
        "trailing-space change is a real change under --ignore-cr-at-eol"
    );
    assert!(
        out(&["diff", "--ignore-space-at-eol", "f.txt"])
            .trim()
            .is_empty(),
        "sanity: --ignore-space-at-eol drops the same change"
    );

    // 3) Mid-line \r is a real change (only the final \r is stripped).
    commit_f("a\rb\n", "base midcr");
    std::fs::write(p.join("f.txt"), "ab\n").unwrap();
    assert!(
        !out(&["diff", "--ignore-cr-at-eol", "f.txt"])
            .trim()
            .is_empty(),
        "mid-line \\r removal is a real change"
    );

    // 4) Precedence: `-b --ignore-cr-at-eol` behaves as `-b` (a space-amount
    //    change is dropped even though cr-at-eol alone would keep it).
    commit_f("x  y\n", "base b");
    std::fs::write(p.join("f.txt"), "x y\n").unwrap();
    assert!(
        out(&["diff", "-b", "--ignore-cr-at-eol", "f.txt"])
            .trim()
            .is_empty(),
        "-b wins over --ignore-cr-at-eol"
    );

    // 5) Double-CR ending: `a\r\r\n` vs `a\r\n` matches Git (equal) and —
    //    with strip-all normalization — behaves identically on the plain and
    //    the --ignore-blank-lines composition paths.
    commit_f("a\r\r\ntail\n", "base doublecr");
    std::fs::write(p.join("f.txt"), "a\r\ntail\n").unwrap();
    assert!(
        out(&["diff", "--ignore-cr-at-eol", "f.txt"])
            .trim()
            .is_empty(),
        "double-CR vs single-CR ending is ignored (matches Git)"
    );
    assert!(
        out(&[
            "diff",
            "--ignore-cr-at-eol",
            "--ignore-blank-lines",
            "f.txt"
        ])
        .trim()
        .is_empty(),
        "the ignore-blank composition path agrees (consistent record semantics)"
    );
}

#[test]
fn test_diff_ignore_cr_at_eol_composes_with_ignore_blank_lines() {
    // Composition (Git's xdl_blankline): under ANY whitespace flag an
    // all-whitespace line counts as blank — so with --ignore-cr-at-eol both a
    // "\r"-terminated blank line AND a space-only line count as blank, and a
    // change adding only such lines drops out.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("f.txt"), "top\nbottom\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add f");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit f",
    );
    // Insert a CRLF blank line and a space-only line between top and bottom.
    std::fs::write(p.join("f.txt"), "top\n\r\n   \nbottom\n").unwrap();
    let out =
        |args: &[&str]| String::from_utf8_lossy(&run_libra_command(args, p).stdout).into_owned();
    assert!(
        !out(&["diff", "--ignore-blank-lines", "f.txt"])
            .trim()
            .is_empty(),
        "without a whitespace flag a \\r-only line is NOT blank (Git parity)"
    );
    assert!(
        out(&[
            "diff",
            "--ignore-blank-lines",
            "--ignore-cr-at-eol",
            "f.txt"
        ])
        .trim()
        .is_empty(),
        "with --ignore-cr-at-eol both the CRLF blank and the space-only line count blank"
    );
}

// ---------------------------------------------------------------------------
// Positional revisions + `--` disambiguation (lore.md §1.4): Git's
// `diff [<revision>...] [--] [<path>...]` grammar.
// ---------------------------------------------------------------------------

/// Two commits diverging on f.txt; returns (repo, c1, c2) with a dirty worktree
/// change on top.
fn positional_diff_repo() -> (tempfile::TempDir, String, String) {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let rev = |spec: &str| {
        let out = run_libra_command(&["rev-parse", spec], p);
        assert_cli_success(&out, "rev-parse");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    fs::write(p.join("f.txt"), "one\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add f1");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    let c1 = rev("HEAD");
    fs::write(p.join("f.txt"), "two\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add f2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );
    let c2 = rev("HEAD");
    fs::write(p.join("f.txt"), "three\n").unwrap(); // dirty worktree
    (repo, c1, c2)
}

#[test]
fn test_diff_positional_single_rev() {
    let (repo, c1, _c2) = positional_diff_repo();
    let p = repo.path();
    let positional = run_libra_command(&["diff", &c1], p);
    assert_cli_success(&positional, "diff <rev>");
    let flagged = run_libra_command(&["diff", "--old", &c1], p);
    assert_eq!(
        String::from_utf8_lossy(&positional.stdout),
        String::from_utf8_lossy(&flagged.stdout),
        "diff <rev> equals diff --old <rev> (rev vs worktree)"
    );
    assert!(
        String::from_utf8_lossy(&positional.stdout).contains("+three"),
        "compares against the dirty worktree"
    );
}

#[test]
fn test_diff_positional_two_revs_equals_two_dot() {
    let (repo, c1, c2) = positional_diff_repo();
    let p = repo.path();
    let spaced = run_libra_command(&["diff", &c1, &c2], p);
    assert_cli_success(&spaced, "diff A B");
    let glued = run_libra_command(&["diff", &format!("{c1}..{c2}")], p);
    assert_eq!(
        String::from_utf8_lossy(&spaced.stdout),
        String::from_utf8_lossy(&glued.stdout),
        "diff A B is byte-identical to diff A..B"
    );
    // Order defines old/new: B A is the inverse.
    let reversed = run_libra_command(&["diff", &c2, &c1], p);
    let rs = String::from_utf8_lossy(&reversed.stdout);
    assert!(
        rs.contains("+one") && rs.contains("-two"),
        "diff B A inverts the sides: {rs}"
    );
    // A pathspec after the two revisions filters the diff.
    let filtered = run_libra_command(&["diff", &c1, &c2, "f.txt"], p);
    assert!(
        String::from_utf8_lossy(&filtered.stdout).contains("-one"),
        "trailing pathspec filters the two-rev diff"
    );
    let missed = run_libra_command(&["diff", &c1, &c2, "--", "nosuch.txt"], p);
    assert!(
        String::from_utf8_lossy(&missed.stdout).trim().is_empty(),
        "post-'--' pathspec needs no existence check and filters to nothing"
    );
}

#[test]
fn test_diff_positional_staged_rev() {
    let (repo, c1, c2) = positional_diff_repo();
    let p = repo.path();
    let positional = run_libra_command(&["diff", "--cached", &c1], p);
    assert_cli_success(&positional, "diff --cached <rev>");
    let flagged = run_libra_command(&["diff", "--old", &c1, "--staged"], p);
    assert_eq!(
        String::from_utf8_lossy(&positional.stdout),
        String::from_utf8_lossy(&flagged.stdout),
        "diff --cached <rev> equals diff --old <rev> --staged"
    );
    // Two revisions (or a range) with --staged is an error.
    let two = run_libra_command(&["diff", "--staged", &c1, &c2], p);
    assert_eq!(
        two.status.code(),
        Some(129),
        "--staged rejects two revs (LBR-CLI-002)"
    );
    let range = run_libra_command(&["diff", "--staged", &format!("{c1}..{c2}")], p);
    assert_eq!(
        range.status.code(),
        Some(129),
        "--staged rejects a range (LBR-CLI-002)"
    );
}

#[test]
fn test_diff_ambiguous_and_unknown_arguments() {
    let (repo, _c1, _c2) = positional_diff_repo();
    let p = repo.path();
    // A name that is BOTH a branch and a file.
    assert_cli_success(&run_libra_command(&["branch", "x"], p), "branch x");
    fs::write(p.join("x"), "content\n").unwrap();
    let amb = run_libra_command(&["diff", "x"], p);
    // Libra CLI-category errors exit 129 (LBR-CLI-002), vs Git's 128 die() —
    // consistent with the existing invalid-revision path; documented divergence.
    assert_eq!(amb.status.code(), Some(129), "ambiguous argument errors");
    assert!(
        String::from_utf8_lossy(&amb.stderr).contains("ambiguous argument 'x'"),
        "names the token: {}",
        String::from_utf8_lossy(&amb.stderr)
    );
    // `--` forces the path reading; a rev before `--` forces the revision one.
    let as_path = run_libra_command(&["diff", "--", "x"], p);
    assert_cli_success(&as_path, "diff -- x path-filters");
    let as_rev = run_libra_command(&["diff", "x", "--", "f.txt"], p);
    assert_cli_success(&as_rev, "diff x -- f.txt uses x as a revision");
    // Unknown token: neither revision nor path.
    let unk = run_libra_command(&["diff", "no-such-thing"], p);
    assert_eq!(unk.status.code(), Some(129));
    assert!(
        String::from_utf8_lossy(&unk.stderr)
            .contains("unknown revision or path not in the working tree"),
        "{}",
        String::from_utf8_lossy(&unk.stderr)
    );
    // More than two revisions is a declined surface (no combined diff).
    let many = run_libra_command(&["diff", "HEAD", "HEAD", "HEAD"], p);
    assert_eq!(many.status.code(), Some(129), "three revisions rejected");
    assert!(
        String::from_utf8_lossy(&many.stderr).contains("more than two revisions"),
        "{}",
        String::from_utf8_lossy(&many.stderr)
    );
}

#[test]
fn test_diff_three_dot_positional_and_no_merge_base() {
    // Fork: base -> (main: c-main) and (feature: c-feat).
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("f.txt"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    let rev = |spec: &str| {
        let out = run_libra_command(&["rev-parse", spec], p);
        assert_cli_success(&out, "rev-parse");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let base = rev("HEAD");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(&run_libra_command(&["checkout", "feature"], p), "co feat");
    fs::write(p.join("f.txt"), "feature\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add feat");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "feat", "--no-verify"], p),
        "commit feat",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    fs::write(p.join("f.txt"), "main\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add main");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main", "--no-verify"], p),
        "commit main",
    );

    // A...B diffs merge-base(A,B) vs B.
    let three = run_libra_command(&["diff", "main...feature"], p);
    assert_cli_success(&three, "three-dot");
    let explicit = run_libra_command(&["diff", "--old", &base, "--new", "feature"], p);
    assert_eq!(
        String::from_utf8_lossy(&three.stdout),
        String::from_utf8_lossy(&explicit.stdout),
        "A...B equals --old <merge-base> --new B"
    );
}
