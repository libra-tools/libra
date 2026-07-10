//! Integration tests for the grep command.

use std::fs;

use libra::{
    command::{
        add::{self, AddArgs},
        branch::{self, BranchArgs},
        commit::{self, CommitArgs},
    },
    utils::{output::OutputConfig, test},
};
use serde_json::Value;
use serial_test::serial;
use tempfile::tempdir;

use super::{assert_cli_success, parse_cli_error_stderr, parse_json_stdout, run_libra_command};

async fn add_and_commit(message: &str, pathspec: Vec<String>) {
    add::execute_safe(
        AddArgs {
            pathspec,
            all: false,
            update: false,
            refresh: false,
            verbose: false,
            force: false,
            dry_run: false,
            ignore_errors: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
        },
        &OutputConfig::default(),
    )
    .await
    .expect("failed to add files");

    commit::execute_safe(
        CommitArgs {
            message: Some(message.to_string()),
            allow_empty: false,
            conventional: false,
            amend: false,
            no_edit: false,
            signoff: false,
            disable_pre: false,
            all: false,
            no_verify: true,
            author: None,
            file: None,
            ..Default::default()
        },
        &OutputConfig::default(),
    )
    .await
    .expect("failed to commit files");
}

#[tokio::test]
#[serial]
async fn test_grep_working_tree_searches_only_tracked_files() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("tracked.txt", "needle in tracked file\n").expect("failed to write tracked file");
    add_and_commit("add tracked file", vec!["tracked.txt".to_string()]).await;

    fs::write("tracked.txt", "needle in tracked file\nupdated needle\n")
        .expect("failed to update tracked file");
    fs::write("untracked.txt", "needle in untracked file\n")
        .expect("failed to write untracked file");

    let output = run_libra_command(&["--json=compact", "grep", "needle"], repo.path());
    assert_cli_success(&output, "grep should succeed for tracked files only");

    let json = parse_json_stdout(&output);
    let matches = json["data"]["matches"]
        .as_array()
        .expect("expected grep matches array");
    let paths: Vec<&str> = matches
        .iter()
        .map(|entry| entry["path"].as_str().expect("expected match path"))
        .collect();

    assert!(paths.iter().all(|path| *path == "tracked.txt"));
    assert!(
        matches
            .iter()
            .any(|entry| entry["line"] == "updated needle")
    );
    assert!(!paths.contains(&"untracked.txt"));
}

#[tokio::test]
#[serial]
async fn test_grep_untracked_searches_untracked_non_ignored_files() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("tracked.txt", "needle in tracked file\n").expect("write tracked");
    add_and_commit("add tracked file", vec!["tracked.txt".to_string()]).await;
    fs::write("untracked.txt", "needle in untracked file\n").expect("write untracked");
    fs::write(".libraignore", "ignored.txt\n").expect("write ignore");
    fs::write("ignored.txt", "needle in ignored file\n").expect("write ignored");

    let output = run_libra_command(
        &["--json=compact", "grep", "--untracked", "needle"],
        repo.path(),
    );
    assert_cli_success(&output, "grep --untracked should succeed");
    let json = parse_json_stdout(&output);
    let paths: Vec<String> = json["data"]["matches"]
        .as_array()
        .expect("matches array")
        .iter()
        .map(|entry| entry["path"].as_str().expect("path").to_string())
        .collect();

    // Tracked AND untracked (non-ignored) are searched; the ignored file is not.
    assert!(
        paths.contains(&"tracked.txt".to_string()),
        "tracked searched: {paths:?}"
    );
    assert!(
        paths.contains(&"untracked.txt".to_string()),
        "untracked non-ignored searched: {paths:?}"
    );
    assert!(
        !paths.contains(&"ignored.txt".to_string()),
        "ignored file is not searched: {paths:?}"
    );

    // --untracked conflicts with --cached (clap, exit 129).
    let conflict = run_libra_command(&["grep", "--untracked", "--cached", "needle"], repo.path());
    assert_eq!(conflict.status.code(), Some(129));

    // --untracked with a --tree revision is a command-level grep error.
    let with_tree = run_libra_command(
        &["grep", "--untracked", "--tree", "HEAD", "needle"],
        repo.path(),
    );
    assert_eq!(with_tree.status.code(), Some(2));
    let (_stderr, report) = parse_cli_error_stderr(&with_tree.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
}

#[test]
#[serial]
fn test_grep_no_index_searches_filesystem_without_repository() {
    // `--no-index` works with no `.libra` present and greps the filesystem recursively.
    let dir = tempdir().expect("failed to create temp dir");
    let p = dir.path();
    fs::write(p.join("a.txt"), "needle x\n").expect("write a");
    fs::create_dir(p.join("d")).expect("mkdir d");
    fs::write(p.join("d").join("b.txt"), "needle y\n").expect("write b");

    let output = run_libra_command(&["--json=compact", "grep", "--no-index", "needle"], p);
    assert_cli_success(&output, "grep --no-index without a repository");
    let json = parse_json_stdout(&output);
    let paths: Vec<String> = json["data"]["matches"]
        .as_array()
        .expect("matches array")
        .iter()
        .map(|entry| entry["path"].as_str().expect("path").to_string())
        .collect();
    assert!(
        paths.iter().any(|path| path == "a.txt"),
        "a.txt searched: {paths:?}"
    );
    assert!(
        paths.iter().any(|path| path == "d/b.txt"),
        "nested d/b.txt searched recursively: {paths:?}"
    );

    // A path argument restricts the walk to that subtree.
    let scoped = run_libra_command(&["--json=compact", "grep", "--no-index", "needle", "d"], p);
    assert_cli_success(&scoped, "grep --no-index d");
    let scoped_paths: Vec<String> = parse_json_stdout(&scoped)["data"]["matches"]
        .as_array()
        .expect("matches array")
        .iter()
        .map(|entry| entry["path"].as_str().expect("path").to_string())
        .collect();
    assert_eq!(
        scoped_paths,
        vec!["d/b.txt".to_string()],
        "scoped to d/: {scoped_paths:?}"
    );

    // --no-index conflicts with --cached (clap, exit 129).
    let conflict = run_libra_command(&["grep", "--no-index", "--cached", "needle"], p);
    assert_eq!(conflict.status.code(), Some(129));
}

#[tokio::test]
#[serial]
async fn test_grep_no_index_searches_ignored_files_inside_repo() {
    // Inside a repository, `--no-index` searches every file, including ignored ones
    // (it does not honor ignore rules), unlike a normal grep.
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("tracked.txt", "needle tracked\n").expect("write tracked");
    add_and_commit("add tracked", vec!["tracked.txt".to_string()]).await;
    fs::write(".libraignore", "ignored.txt\n").expect("write ignore");
    fs::write("ignored.txt", "needle ignored\n").expect("write ignored");

    let output = run_libra_command(
        &["--json=compact", "grep", "--no-index", "needle"],
        repo.path(),
    );
    assert_cli_success(&output, "grep --no-index inside repo");
    let paths: Vec<String> = parse_json_stdout(&output)["data"]["matches"]
        .as_array()
        .expect("matches array")
        .iter()
        .map(|entry| entry["path"].as_str().expect("path").to_string())
        .collect();
    assert!(
        paths.iter().any(|path| path == "tracked.txt"),
        "tracked searched: {paths:?}"
    );
    assert!(
        paths.iter().any(|path| path == "ignored.txt"),
        "ignored file IS searched under --no-index: {paths:?}"
    );
}

#[tokio::test]
#[serial]
async fn test_grep_after_context_marks_context_lines() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("f.txt", "alpha\nNEEDLE\nbeta\ngamma\n").expect("failed to write file");
    add_and_commit("add f", vec!["f.txt".to_string()]).await;

    let output = run_libra_command(
        &["--json=compact", "grep", "-A", "1", "NEEDLE"],
        repo.path(),
    );
    assert_cli_success(&output, "grep -A 1 should succeed");

    let json = parse_json_stdout(&output);
    let matches = json["data"]["matches"]
        .as_array()
        .expect("expected grep matches array");
    // The match line plus one trailing context line.
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0]["line"], "NEEDLE");
    assert_eq!(matches[0]["line_number"], 2);
    // Real match lines omit the is_context field (serde skip when false).
    assert!(matches[0].get("is_context").is_none_or(|v| v == false));
    assert_eq!(matches[1]["line"], "beta");
    assert_eq!(matches[1]["line_number"], 3);
    assert_eq!(matches[1]["is_context"], true);
    // total_matches counts only real matches, not context lines.
    assert_eq!(json["data"]["total_matches"], 1);
}

#[tokio::test]
#[serial]
async fn test_grep_perl_regexp_is_rejected() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("f.txt", "needle\n").expect("failed to write file");
    add_and_commit("add f", vec!["f.txt".to_string()]).await;

    let output = run_libra_command(&["grep", "-P", "needle"], repo.path());
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("perl-regexp is not supported"),
        "unexpected stderr: {stderr}"
    );
}

#[tokio::test]
#[serial]
async fn test_grep_tree_head_searches_committed_content() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("history.txt", "present in head\n").expect("failed to write file");
    add_and_commit("add history file", vec!["history.txt".to_string()]).await;

    fs::write("history.txt", "working tree only\n").expect("failed to update file");

    let output = run_libra_command(
        &["--json=compact", "grep", "--tree", "HEAD", "present"],
        repo.path(),
    );
    assert_cli_success(&output, "grep --tree HEAD should succeed");

    let json = parse_json_stdout(&output);
    let matches = json["data"]["matches"]
        .as_array()
        .expect("expected grep matches array");

    assert_eq!(
        json["data"]["context"],
        Value::String("tree:HEAD".to_string())
    );
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["path"], Value::String("history.txt".to_string()));
    assert_eq!(
        matches[0]["line"],
        Value::String("present in head".to_string())
    );
}

#[tokio::test]
#[serial]
async fn test_grep_tree_accepts_branch_revisions() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("branch.txt", "main branch text\n").expect("failed to write file");
    add_and_commit("base commit", vec!["branch.txt".to_string()]).await;

    branch::execute_safe(
        BranchArgs {
            subcommand: None,
            format: None,
            no_column: false,
            new_branch: Some("feature/grep-tree".to_string()),
            commit_hash: None,
            list: false,
            delete: None,
            delete_safe: None,
            set_upstream_to: None,
            unset_upstream: None,
            edit_description: None,
            show_current: false,
            rename: Vec::new(),
            copy: Vec::new(),
            copy_force: Vec::new(),
            remotes: false,
            all: false,
            contains: Vec::new(),
            no_contains: Vec::new(),
            points_at: None,
            merged: None,
            no_merged: None,
            sort: None,
            ignore_case: false,
            column: None,
            verbose: 0,
        },
        &OutputConfig::default(),
    )
    .await
    .expect("failed to create branch");

    fs::write("branch.txt", "feature branch text\n").expect("failed to update file");
    add_and_commit("update current branch", vec!["branch.txt".to_string()]).await;

    let output = run_libra_command(
        &[
            "--json=compact",
            "grep",
            "--tree",
            "feature/grep-tree",
            "main",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "grep should resolve branch revisions");

    let json = parse_json_stdout(&output);
    let matches = json["data"]["matches"]
        .as_array()
        .expect("expected grep matches array");

    assert_eq!(matches.len(), 1);
    assert_eq!(
        matches[0]["line"],
        Value::String("main branch text".to_string())
    );
}

#[tokio::test]
#[serial]
async fn test_grep_word_regexp_preserves_regex_semantics() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("words.txt", "foo\nbar\nfoobar\nbarista\n").expect("failed to write file");
    add_and_commit("add words file", vec!["words.txt".to_string()]).await;

    let output = run_libra_command(&["--json=compact", "grep", "-w", "foo|bar"], repo.path());
    assert_cli_success(&output, "-w should preserve regex alternation semantics");

    let json = parse_json_stdout(&output);
    let matches = json["data"]["matches"]
        .as_array()
        .expect("expected grep matches array");
    let lines: Vec<&str> = matches
        .iter()
        .map(|entry| entry["line"].as_str().expect("expected matched line"))
        .collect();
    assert_eq!(lines, vec!["foo", "bar"]);
}

#[tokio::test]
#[serial]
async fn test_grep_tree_reports_invalid_revision() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("tracked.txt", "tracked\n").expect("failed to write file");
    add_and_commit("add tracked file", vec!["tracked.txt".to_string()]).await;

    let output = run_libra_command(
        &["--json=compact", "grep", "--tree", "missing-ref", "tracked"],
        repo.path(),
    );
    assert!(
        !output.status.success(),
        "grep with invalid revision should fail"
    );

    let (_human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(report.message.contains("invalid revision: missing-ref"));
}

#[tokio::test]
#[serial]
async fn test_grep_byte_offset_reports_zero_based_match_offset() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("offset.txt", "alpha\nfoo bar\n").expect("failed to write file");
    add_and_commit("add offset file", vec!["offset.txt".to_string()]).await;

    let output = run_libra_command(&["--json=compact", "grep", "-b", "bar"], repo.path());
    assert_cli_success(&output, "grep -b should succeed");

    let json = parse_json_stdout(&output);
    let matches = json["data"]["matches"]
        .as_array()
        .expect("expected grep matches array");

    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["path"], Value::String("offset.txt".to_string()));
    assert_eq!(matches[0]["byte_offset"], Value::from(4));
}

#[tokio::test]
#[serial]
async fn test_grep_tree_skips_large_blob_files() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    let large_content = "needle\n".repeat(90_000);
    fs::write("large.txt", large_content).expect("failed to write large file");
    add_and_commit("add large file", vec!["large.txt".to_string()]).await;

    let output = run_libra_command(
        &["--json=compact", "grep", "--tree", "HEAD", "needle"],
        repo.path(),
    );
    assert_eq!(output.status.code(), Some(1));
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["total_matches"], Value::from(0));
    assert_eq!(json["data"]["total_files"], Value::from(0));
    assert_eq!(json["data"]["matches"], Value::Array(Vec::new()));
}

#[tokio::test]
#[serial]
async fn test_grep_reports_total_files_as_number_of_matched_files() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("a.txt", "needle once\n").expect("failed to write a.txt");
    fs::write("b.txt", "needle twice\nneedle again\n").expect("failed to write b.txt");
    fs::write("c.txt", "no match here\n").expect("failed to write c.txt");
    add_and_commit(
        "add multiple files",
        vec![
            "a.txt".to_string(),
            "b.txt".to_string(),
            "c.txt".to_string(),
        ],
    )
    .await;

    let output = run_libra_command(&["--json=compact", "grep", "needle"], repo.path());
    assert_cli_success(&output, "grep should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["total_files"], Value::from(2));
    assert_eq!(json["data"]["total_matches"], Value::from(3));
}

#[tokio::test]
#[serial]
async fn test_grep_count_reports_matching_lines_per_file() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("count.txt", "needle needle\nneedle again\nnone\n")
        .expect("failed to write count file");
    add_and_commit("add count file", vec!["count.txt".to_string()]).await;

    let output = run_libra_command(&["--json=compact", "grep", "-c", "needle"], repo.path());
    assert_cli_success(&output, "grep -c should succeed");

    let json = parse_json_stdout(&output);
    let counts = json["data"]["counts"]
        .as_array()
        .expect("expected grep counts array");

    assert_eq!(json["data"]["total_files"], Value::from(1));
    assert_eq!(json["data"]["total_matches"], Value::from(2));
    assert_eq!(counts.len(), 1);
    assert_eq!(counts[0]["path"], Value::String("count.txt".to_string()));
    assert_eq!(counts[0]["count"], Value::from(2));
}

#[tokio::test]
#[serial]
async fn test_grep_files_without_matches_uses_plural_json_field() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("match.txt", "needle here\n").expect("failed to write match file");
    fs::write("miss.txt", "nothing here\n").expect("failed to write miss file");
    add_and_commit(
        "add match and miss files",
        vec!["match.txt".to_string(), "miss.txt".to_string()],
    )
    .await;

    let output = run_libra_command(&["--json=compact", "grep", "-L", "needle"], repo.path());
    assert_cli_success(&output, "grep -L should succeed");

    let json = parse_json_stdout(&output);
    let misses = json["data"]["files_without_matches"]
        .as_array()
        .expect("expected files_without_matches array");

    assert_eq!(json["data"]["total_files"], Value::from(1));
    assert_eq!(misses.len(), 1);
    assert_eq!(misses[0], Value::String("miss.txt".to_string()));
    assert_eq!(json["data"]["files_without_match"], Value::Null);
    assert_eq!(json["data"]["files_with_matches"], Value::Null);
}

#[tokio::test]
#[serial]
async fn test_grep_multiple_regexp_patterns_match_any() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("multi.txt", "alpha\nbeta\ngamma\n").expect("failed to write file");
    add_and_commit("add multi file", vec!["multi.txt".to_string()]).await;

    let output = run_libra_command(
        &["--json=compact", "grep", "-e", "alpha", "-e", "gamma"],
        repo.path(),
    );
    assert_cli_success(&output, "grep with multiple -e patterns should succeed");

    let json = parse_json_stdout(&output);
    let matches = json["data"]["matches"]
        .as_array()
        .expect("expected grep matches array");
    assert_eq!(matches.len(), 2);
}

#[tokio::test]
#[serial]
async fn test_grep_reads_patterns_from_file() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("target.txt", "alpha\nbeta\ngamma\n").expect("failed to write file");
    fs::write("patterns.txt", "beta\ngamma\n").expect("failed to write pattern file");
    add_and_commit("add target file", vec!["target.txt".to_string()]).await;

    let output = run_libra_command(
        &["--json=compact", "grep", "-f", "patterns.txt"],
        repo.path(),
    );
    assert_cli_success(&output, "grep -f should succeed");

    let json = parse_json_stdout(&output);
    let matches = json["data"]["matches"]
        .as_array()
        .expect("expected grep matches array");
    assert_eq!(matches.len(), 2);
}

#[tokio::test]
#[serial]
async fn test_grep_invalid_pattern_file_returns_structured_error() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    let output = run_libra_command(
        &["--json=compact", "grep", "-f", "missing.txt"],
        repo.path(),
    );
    assert!(
        !output.status.success(),
        "grep with missing pattern file should fail"
    );

    let (_human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-IO-001");
    assert!(
        report
            .message
            .contains("failed to read pattern file 'missing.txt'")
    );
}

#[tokio::test]
#[serial]
async fn test_grep_tree_large_blob_emits_warning_in_json() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    let large_content = "needle\n".repeat(90_000);
    fs::write("large-warning.txt", large_content).expect("failed to write large file");
    add_and_commit(
        "add large warning file",
        vec!["large-warning.txt".to_string()],
    )
    .await;

    let output = run_libra_command(
        &["--json=compact", "grep", "--tree", "HEAD", "needle"],
        repo.path(),
    );
    assert_eq!(output.status.code(), Some(1));

    let json = parse_json_stdout(&output);
    let warnings = json["data"]["warnings"]
        .as_array()
        .expect("expected warnings array");
    assert!(!warnings.is_empty());
    assert!(warnings[0]["path"] == "large-warning.txt");
}

#[cfg(unix)]
#[tokio::test]
#[serial]
async fn test_grep_working_tree_symlink_emits_warning_and_skips_target() {
    use std::os::unix::fs::symlink;

    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("real.txt", "needle\n").expect("failed to write real file");
    fs::write("link.txt", "needle\n").expect("failed to write tracked file");
    add_and_commit(
        "add target and tracked path",
        vec!["real.txt".to_string(), "link.txt".to_string()],
    )
    .await;
    fs::remove_file("link.txt").expect("failed to remove tracked file");
    symlink("real.txt", "link.txt").expect("failed to create symlink");

    let output = run_libra_command(&["--json=compact", "grep", "needle"], repo.path());
    assert_cli_success(&output, "grep should succeed while skipping symlink");

    let json = parse_json_stdout(&output);
    let warnings = json["data"]["warnings"]
        .as_array()
        .expect("expected warnings array");
    assert!(warnings.iter().any(|warning| warning["path"] == "link.txt"));
}

#[tokio::test]
#[serial]
async fn test_grep_returns_nonzero_when_no_matches_found() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("nomatch.txt", "alpha\nbeta\n").expect("failed to write file");
    add_and_commit("add no-match file", vec!["nomatch.txt".to_string()]).await;

    let output = run_libra_command(&["grep", "needle"], repo.path());
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stderr.is_empty(),
        "no-match grep should be a silent status signal: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
#[serial]
async fn test_grep_all_match_requires_all_patterns_in_same_file() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("all-match.txt", "alpha\nonly-one\nbeta\n").expect("failed to write file");
    fs::write("partial.txt", "alpha only\n").expect("failed to write file");
    add_and_commit(
        "add all-match files",
        vec!["all-match.txt".to_string(), "partial.txt".to_string()],
    )
    .await;

    let output = run_libra_command(
        &[
            "--json=compact",
            "grep",
            "--all-match",
            "-e",
            "alpha",
            "-e",
            "beta",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "grep --all-match should succeed");

    let json = parse_json_stdout(&output);
    let matches = json["data"]["matches"]
        .as_array()
        .expect("expected grep matches array");
    let paths: Vec<&str> = matches
        .iter()
        .map(|entry| entry["path"].as_str().expect("expected match path"))
        .collect();
    assert!(paths.iter().all(|path| *path == "all-match.txt"));
}

#[tokio::test]
#[serial]
async fn test_grep_all_match_is_based_on_positive_pattern_presence_even_with_invert() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("both.txt", "alpha\nkeep\nbeta\n").expect("failed to write file");
    fs::write("only-alpha.txt", "alpha\nkeep\n").expect("failed to write file");
    add_and_commit(
        "add invert all-match files",
        vec!["both.txt".to_string(), "only-alpha.txt".to_string()],
    )
    .await;

    let output = run_libra_command(
        &[
            "--json=compact",
            "grep",
            "-v",
            "--all-match",
            "-e",
            "alpha",
            "-e",
            "beta",
        ],
        repo.path(),
    );
    assert_cli_success(&output, "grep -v --all-match should succeed");

    let json = parse_json_stdout(&output);
    let matches = json["data"]["matches"]
        .as_array()
        .expect("expected grep matches array");
    let paths: Vec<&str> = matches
        .iter()
        .map(|entry| entry["path"].as_str().expect("expected match path"))
        .collect();
    assert!(paths.iter().all(|path| *path == "both.txt"));
}

/// `libra grep --help` surfaces the EXAMPLES banner so users see the
/// canonical invocations (regex vs literal, multi-pattern, --cached,
/// --tree REV, count, filename listing, --json) without reading the
/// design doc. Cross-cutting `--help` EXAMPLES rollout per
/// `docs/development/commands/_general.md` item B.
#[test]
fn test_grep_help_lists_examples_banner() {
    let repo = tempdir().expect("tempdir for grep --help");
    let output = run_libra_command(&["grep", "--help"], repo.path());
    assert!(
        output.status.success(),
        "grep --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "grep --help should include EXAMPLES banner, stdout: {stdout}"
    );
    for invocation in [
        "libra grep 'TODO'",
        "libra grep -F 'fn foo()'",
        "libra grep -i 'panic'",
        "libra grep -n 'TODO' src/",
        "libra grep -c 'unsafe' src/",
        "libra grep -l 'unwrap()' src/",
        "libra grep -e 'TODO' -e 'FIXME'",
        "libra grep --cached 'TODO'",
        "libra grep --tree HEAD~5 'TODO'",
        "libra grep --json 'TODO'",
    ] {
        assert!(
            stdout.contains(invocation),
            "grep --help should include `{invocation}`, stdout: {stdout}"
        );
    }
}

#[tokio::test]
#[serial]
async fn test_grep_default_output_is_unchanged() {
    // Regression guard: with none of the new grouping flags, output is exactly
    // the historical `path:[lineno:]content` form, sorted by path.
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("a.txt", "alpha foo\nbeta\ngamma foo\n").expect("failed to write a.txt");
    fs::write("b.txt", "delta foo\n").expect("failed to write b.txt");
    add_and_commit("add files", vec!["a.txt".to_string(), "b.txt".to_string()]).await;

    let output = run_libra_command(&["grep", "-n", "foo"], repo.path());
    assert_cli_success(&output, "default grep should succeed");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "a.txt:1:alpha foo\na.txt:3:gamma foo\nb.txt:1:delta foo\n"
    );
}

#[tokio::test]
#[serial]
async fn test_grep_heading_groups_matches_under_file_name() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("a.txt", "alpha foo\nbeta\ngamma foo\n").expect("failed to write a.txt");
    fs::write("b.txt", "delta foo\n").expect("failed to write b.txt");
    add_and_commit("add files", vec!["a.txt".to_string(), "b.txt".to_string()]).await;

    // --heading: file name on its own line; match lines drop the prefix.
    let heading = "a.txt\n1:alpha foo\n3:gamma foo\nb.txt\n1:delta foo\n";
    let output = run_libra_command(&["grep", "--heading", "-n", "foo"], repo.path());
    assert_cli_success(&output, "grep --heading should succeed");
    assert_eq!(String::from_utf8_lossy(&output.stdout), heading);

    // Last-one-wins: `--no-heading --heading` keeps headings on.
    let output = run_libra_command(
        &["grep", "--no-heading", "--heading", "-n", "foo"],
        repo.path(),
    );
    assert_cli_success(&output, "grep --no-heading --heading should succeed");
    assert_eq!(String::from_utf8_lossy(&output.stdout), heading);

    // ...and `--heading --no-heading` falls back to the default prefixed form.
    let output = run_libra_command(
        &["grep", "--heading", "--no-heading", "-n", "foo"],
        repo.path(),
    );
    assert_cli_success(&output, "grep --heading --no-heading should succeed");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "a.txt:1:alpha foo\na.txt:3:gamma foo\nb.txt:1:delta foo\n"
    );
}

#[tokio::test]
#[serial]
async fn test_grep_break_inserts_blank_line_between_files() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("a.txt", "alpha foo\ngamma foo\n").expect("failed to write a.txt");
    fs::write("b.txt", "delta foo\n").expect("failed to write b.txt");
    add_and_commit("add files", vec!["a.txt".to_string(), "b.txt".to_string()]).await;

    // One blank line between file groups; per-line prefix preserved.
    let expected = "a.txt:1:alpha foo\na.txt:2:gamma foo\n\nb.txt:1:delta foo\n";
    let output = run_libra_command(&["grep", "--break", "-n", "foo"], repo.path());
    assert_cli_success(&output, "grep --break should succeed");
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);

    // Last-one-wins for the negated pair.
    let output = run_libra_command(&["grep", "--no-break", "--break", "-n", "foo"], repo.path());
    assert_cli_success(&output, "grep --no-break --break should succeed");
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);

    // A single matching file gets no leading or trailing blank line.
    let output = run_libra_command(&["grep", "--break", "-n", "foo", "a.txt"], repo.path());
    assert_cli_success(&output, "grep --break single file should succeed");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "a.txt:1:alpha foo\na.txt:2:gamma foo\n"
    );
}

#[tokio::test]
#[serial]
async fn test_grep_heading_and_break_combine() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("a.txt", "alpha foo\nbeta\ngamma foo\n").expect("failed to write a.txt");
    fs::write("b.txt", "delta foo\n").expect("failed to write b.txt");
    add_and_commit("add files", vec!["a.txt".to_string(), "b.txt".to_string()]).await;

    // Each new file emits a blank line (--break) then a heading (--heading).
    let output = run_libra_command(&["grep", "--heading", "--break", "-n", "foo"], repo.path());
    assert_cli_success(&output, "grep --heading --break should succeed");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "a.txt\n1:alpha foo\n3:gamma foo\n\nb.txt\n1:delta foo\n"
    );
}

#[tokio::test]
#[serial]
async fn test_grep_heading_with_context_keeps_group_separator() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("d.txt", "m foo\nx\ny\nm2 foo\n").expect("failed to write d.txt");
    add_and_commit("add d", vec!["d.txt".to_string()]).await;

    // Heading drops the prefix; context lines use '-'; non-adjacent groups keep '--'.
    let output = run_libra_command(&["grep", "--heading", "-A", "1", "-n", "foo"], repo.path());
    assert_cli_success(&output, "grep --heading -A1 should succeed");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "d.txt\n1:m foo\n2-x\n--\n4:m2 foo\n"
    );
}

#[tokio::test]
#[serial]
async fn test_grep_null_separates_fields_with_nul_byte() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("a.txt", "alpha foo\ngamma foo\n").expect("failed to write a.txt");
    fs::write("b.txt", "delta foo\n").expect("failed to write b.txt");
    fs::write("c.txt", "no match here\n").expect("failed to write c.txt");
    add_and_commit(
        "add files",
        vec![
            "a.txt".to_string(),
            "b.txt".to_string(),
            "c.txt".to_string(),
        ],
    )
    .await;

    // -z -n: every field separator becomes NUL; lines stay newline-terminated.
    let output = run_libra_command(&["grep", "-z", "-n", "foo"], repo.path());
    assert_cli_success(&output, "grep -z should succeed");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "a.txt\u{0}1\u{0}alpha foo\na.txt\u{0}2\u{0}gamma foo\nb.txt\u{0}1\u{0}delta foo\n"
    );

    // -lz: NUL-terminated file names, no trailing newline.
    let output = run_libra_command(&["grep", "-lz", "foo"], repo.path());
    assert_cli_success(&output, "grep -lz should succeed");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "a.txt\u{0}b.txt\u{0}"
    );

    // -cz: `path\0count`, newline-terminated record (zero-count files omitted).
    let output = run_libra_command(&["grep", "-cz", "foo"], repo.path());
    assert_cli_success(&output, "grep -cz should succeed");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "a.txt\u{0}2\nb.txt\u{0}1\n"
    );

    // -Lz: files without a match, NUL-terminated.
    let output = run_libra_command(&["grep", "-Lz", "foo"], repo.path());
    assert_cli_success(&output, "grep -Lz should succeed");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "c.txt\u{0}");
}

#[tokio::test]
#[serial]
async fn test_grep_null_with_context_uses_nul_and_literal_separator() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("d.txt", "m foo\nx\ny\nm2 foo\n").expect("failed to write d.txt");
    add_and_commit("add d", vec!["d.txt".to_string()]).await;

    // Context lines use NUL after the file name too; the group separator stays "--".
    let output = run_libra_command(&["grep", "-z", "-A", "1", "foo"], repo.path());
    assert_cli_success(&output, "grep -z -A1 should succeed");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "d.txt\u{0}m foo\nd.txt\u{0}x\n--\nd.txt\u{0}m2 foo\n"
    );
}

#[tokio::test]
#[serial]
async fn test_grep_no_match_emits_no_output() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("a.txt", "alpha foo\n").expect("failed to write a.txt");
    add_and_commit("add a", vec!["a.txt".to_string()]).await;

    // No match: stdout is empty regardless of the grouping flags, and the
    // command reports failure (Git-style exit code).
    let output = run_libra_command(
        &["grep", "--heading", "--break", "-z", "-n", "zzz"],
        repo.path(),
    );
    assert!(
        output.stdout.is_empty(),
        "no-match output should be empty, stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        !output.status.success(),
        "no-match grep should exit non-zero"
    );
}

#[tokio::test]
#[serial]
async fn test_grep_max_count_and_only_matching() {
    let repo = tempdir().expect("repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write("f.txt", "foo foo\nbar foo\nfoo end\nplain line\n").expect("write");
    add_and_commit("add f", vec!["f.txt".to_string()]).await;

    // -m 2: stop after 2 matching lines in the file.
    let capped = run_libra_command(&["--json=compact", "grep", "-m", "2", "foo"], repo.path());
    assert_cli_success(&capped, "grep -m 2");
    let cj = parse_json_stdout(&capped);
    let cm = cj["data"]["matches"].as_array().expect("matches array");
    assert_eq!(cm.len(), 2, "-m 2 caps at 2 matching lines: {cm:?}");

    // -o: emit each matched substring (line 1 has two "foo"s) -> 2+1+1 = 4.
    let only = run_libra_command(&["--json=compact", "grep", "-o", "foo"], repo.path());
    assert_cli_success(&only, "grep -o");
    let oj = parse_json_stdout(&only);
    let om = oj["data"]["matches"].as_array().expect("matches array");
    assert_eq!(
        om.len(),
        4,
        "-o emits one entry per match occurrence: {om:?}"
    );
    assert!(
        om.iter().all(|m| m["line"] == "foo"),
        "-o emits only the matched substring: {om:?}"
    );

    // -m 1 -o: cap at the first matching line, then expand its matches (2).
    let both = run_libra_command(
        &["--json=compact", "grep", "-m", "1", "-o", "foo"],
        repo.path(),
    );
    assert_cli_success(&both, "grep -m 1 -o");
    let bj = parse_json_stdout(&both);
    let bm = bj["data"]["matches"].as_array().expect("matches array");
    assert_eq!(
        bm.len(),
        2,
        "-m 1 keeps line 1 only, -o expands its 2 matches: {bm:?}"
    );

    // -o -b: each match reports its OWN within-line byte offset.
    // Matches in order: line1 "foo foo" -> 0, 4; line2 "bar foo" -> 4; line3 "foo end" -> 0.
    let ob = run_libra_command(&["--json=compact", "grep", "-o", "-b", "foo"], repo.path());
    assert_cli_success(&ob, "grep -o -b");
    let obj = parse_json_stdout(&ob);
    let obm = obj["data"]["matches"].as_array().expect("matches array");
    assert_eq!(obm[0]["byte_offset"], 0, "line1 match1 offset 0: {obm:?}");
    assert_eq!(obm[1]["byte_offset"], 4, "line1 match2 offset 4: {obm:?}");
    // line2 "bar foo": the match's within-line offset is 4. The earlier buggy
    // `byte_off + m.start()` would have reported 8 here (byte_off=4, m.start()=4).
    assert_eq!(
        obm[2]["byte_offset"], 4,
        "line2 match offset 4 (not 8): {obm:?}"
    );
}

/// `--max-depth` limits how many directory levels below each pathspec (or below
/// the search root when no pathspec is given) `grep` descends, matching Git: a
/// file directly inside the pathspec directory is depth 0.
#[tokio::test]
#[serial]
async fn test_grep_max_depth_limits_directory_descent() {
    let repo = tempdir().expect("failed to create repo dir");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::create_dir_all("a/b/c").expect("failed to create nested dirs");
    fs::write("top.txt", "match\n").expect("write top");
    fs::write("a/m1.txt", "match\n").expect("write a/m1");
    fs::write("a/b/m2.txt", "match\n").expect("write a/b/m2");
    fs::write("a/b/c/m3.txt", "match\n").expect("write a/b/c/m3");
    add_and_commit(
        "add nested files",
        vec![
            "top.txt".to_string(),
            "a/m1.txt".to_string(),
            "a/b/m2.txt".to_string(),
            "a/b/c/m3.txt".to_string(),
        ],
    )
    .await;

    // Names with matches, sorted, for the given args.
    let names = |args: &[&str]| -> Vec<String> {
        let mut full = vec!["grep", "-l", "match"];
        full.extend_from_slice(args);
        let out = run_libra_command(&full, repo.path());
        assert_cli_success(&out, "grep -l --max-depth");
        let mut paths: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.to_string())
            .collect();
        paths.sort();
        paths
    };

    // No pathspec: depth 0 is top-level files only; depth 1 adds files one
    // directory down; a high depth (or negative) reaches everything.
    assert_eq!(names(&["--max-depth", "0"]), vec!["top.txt"]);
    assert_eq!(names(&["--max-depth", "1"]), vec!["a/m1.txt", "top.txt"]);
    assert_eq!(
        names(&["--max-depth", "2"]),
        vec!["a/b/m2.txt", "a/m1.txt", "top.txt"]
    );
    assert_eq!(
        names(&["--max-depth", "-1"]),
        vec!["a/b/c/m3.txt", "a/b/m2.txt", "a/m1.txt", "top.txt"]
    );

    // With a pathspec, depth is measured relative to that directory: depth 0
    // keeps only the files directly inside it.
    assert_eq!(names(&["--max-depth", "0", "a"]), vec!["a/m1.txt"]);
    assert_eq!(names(&["--max-depth", "0", "a/b"]), vec!["a/b/m2.txt"]);
    assert_eq!(
        names(&["--max-depth", "1", "a"]),
        vec!["a/b/m2.txt", "a/m1.txt"]
    );

    // A pathspec naming a file exactly is always within depth 0 (no directory
    // descent), and multiple pathspecs union their in-depth matches.
    assert_eq!(
        names(&["--max-depth", "0", "a/b/c/m3.txt"]),
        vec!["a/b/c/m3.txt"]
    );
    assert_eq!(
        names(&["--max-depth", "0", "a", "a/b"]),
        vec!["a/b/m2.txt", "a/m1.txt"]
    );

    // A `.` / `./` pathspec is the search root (depth 0 = top-level files),
    // matching Git — the `CurDir` component must not be counted toward depth.
    assert_eq!(names(&["--max-depth", "0", "."]), vec!["top.txt"]);
    assert_eq!(names(&["--max-depth", "0", "./"]), vec!["top.txt"]);

    // Run from a subdirectory. `libra grep` searches the whole worktree with
    // worktree-relative paths regardless of cwd, so with NO pathspec depth is
    // measured from the worktree root (a deliberate Libra/Git difference: Git
    // would scope to the current directory). With a pathspec, depth is measured
    // relative to that pathspec — matching Git's file selection.
    {
        let subdir = repo.path().join("a");
        let _guard = test::ChangeDirGuard::new(&subdir);
        let names_from_sub = |args: &[&str]| -> Vec<String> {
            let mut full = vec!["grep", "-l", "match"];
            full.extend_from_slice(args);
            let out = run_libra_command(&full, &subdir);
            assert_cli_success(&out, "grep -l --max-depth from subdir");
            let mut paths: Vec<String> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|l| l.to_string())
                .collect();
            paths.sort();
            paths
        };
        // No pathspec: worktree-root-relative depth (top.txt is depth 0).
        assert_eq!(names_from_sub(&["--max-depth", "0"]), vec!["top.txt"]);
        // Pathspec `b` (relative to cwd `a/`): depth 0 keeps `a/b/m2.txt`,
        // exactly the file Git would select (Git shows it as `b/m2.txt`).
        assert_eq!(
            names_from_sub(&["--max-depth", "0", "b"]),
            vec!["a/b/m2.txt"]
        );
    }
}
