//! Integration tests for reflog command with filtering functionality.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use clap::Parser;
use libra::{
    command::{commit, reflog},
    internal::{db::get_db_conn_instance, reflog::Reflog},
    utils::output::OutputConfig,
};
use serial_test::serial;
use tempfile::tempdir;

use super::*;

/// Helper: Get the number of reflog entries for HEAD
async fn count_reflog_entries() -> usize {
    let db = get_db_conn_instance().await;
    Reflog::find_all(&db, "HEAD")
        .await
        .map(|logs| logs.len())
        .unwrap_or(0)
}

#[test]
fn test_reflog_show_json_outputs_entries() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "reflog", "show", "-n", "1"], repo.path());
    assert_cli_success(&output, "json reflog show");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "reflog.show");
    assert_eq!(json["data"]["ref_name"], "HEAD");
    assert_eq!(json["data"]["count"], 1);
    assert!(json["data"]["total_count"].as_u64().unwrap_or_default() >= 1);

    let entries = json["data"]["entries"].as_array().expect("entries array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["selector"], "HEAD@{0}");
    assert_eq!(entries[0]["index"], 0);
    assert_eq!(entries[0]["ref_name"], "HEAD");
    assert!(entries[0]["new_oid"].as_str().unwrap_or_default().len() >= 7);
    assert_eq!(
        entries[0]["short_new_oid"]
            .as_str()
            .unwrap_or_default()
            .len(),
        7
    );
    assert!(
        entries[0]["commit"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("base")
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn test_reflog_bare_defaults_to_show() {
    let repo = create_committed_repo_via_cli();

    // No subcommand should behave like `reflog show HEAD`.
    let output = run_libra_command(&["--json", "reflog"], repo.path());
    assert_cli_success(&output, "bare json reflog");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "reflog.show");
    assert_eq!(json["data"]["ref_name"], "HEAD");
    let entries = json["data"]["entries"].as_array().expect("entries array");
    assert!(!entries.is_empty(), "bare reflog should list HEAD entries");
    assert_eq!(entries[0]["selector"], "HEAD@{0}");
}

#[test]
fn test_reflog_show_json_invalid_date_reports_invalid_arguments() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(
        &["--json", "reflog", "show", "--since", "not-a-date"],
        repo.path(),
    );

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let (_human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(report.message.contains("invalid --since date"));
}

#[test]
fn test_reflog_exists_machine_outputs_single_json_line() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--machine", "reflog", "exists", "HEAD"], repo.path());
    assert_cli_success(&output, "machine reflog exists");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().count(),
        1,
        "expected one JSON line, got: {stdout}"
    );

    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("expected JSON");
    assert_eq!(json["command"], "reflog.exists");
    assert_eq!(json["data"]["ref_name"], "HEAD");
    assert_eq!(json["data"]["exists"], true);
    assert!(output.stderr.is_empty());
}

#[test]
fn test_reflog_exists_json_missing_ref_reports_invalid_target() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(
        &["--json", "reflog", "exists", "refs/heads/missing"],
        repo.path(),
    );

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let (_human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        report
            .message
            .contains("reflog entry for 'refs/heads/missing' not found")
    );
}

#[test]
fn test_reflog_exists_json_expands_bare_branch_name() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "reflog", "exists", "main"], repo.path());
    assert_cli_success(&output, "json reflog exists main");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "reflog.exists");
    assert_eq!(json["data"]["ref_name"], "refs/heads/main");
    assert_eq!(json["data"]["exists"], true);
}

#[test]
fn test_reflog_delete_json_reports_deleted_count() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "reflog", "delete", "HEAD@{0}"], repo.path());
    assert_cli_success(&output, "json reflog delete");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "reflog.delete");
    assert_eq!(json["data"]["deleted_count"], 1);
    assert_eq!(json["data"]["selectors"][0], "HEAD@{0}");
    assert!(output.stderr.is_empty());
}

#[test]
fn test_reflog_delete_json_expands_bare_branch_selector() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "reflog", "delete", "main@{0}"], repo.path());
    assert_cli_success(&output, "json reflog delete main@{0}");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "reflog.delete");
    assert_eq!(json["data"]["deleted_count"], 1);
    assert_eq!(json["data"]["selectors"][0], "main@{0}");
}

#[test]
fn test_reflog_delete_json_missing_selector_reports_invalid_target() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "reflog", "delete", "HEAD@{99}"], repo.path());

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let (_human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(report.message.contains("HEAD@{99}"));
}

#[tokio::test]
#[serial(cwd)]
async fn test_reflog_show_with_filters() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Create some commits to generate reflog entries
    for i in 1..=3 {
        let commit_args = commit::CommitArgs {
            message: Some(format!("Test commit {}", i)),
            file: None,
            allow_empty: true,
            conventional: false,
            amend: false,
            no_edit: false,
            signoff: false,
            disable_pre: true,
            all: false,
            no_verify: false,
            author: None,
            ..Default::default()
        };
        commit::execute(commit_args).await;
    }

    // Verify: 3 reflog entries should exist (init + 3 commits = 4 entries total)
    let total_entries = count_reflog_entries().await;
    assert!(
        total_entries >= 3,
        "Expected at least 3 reflog entries, found {}",
        total_entries
    );

    // Test basic reflog show - should not panic
    let args = reflog::ReflogArgs::parse_from(["reflog", "show"]);
    reflog::execute(args).await;

    // Test with --grep filter - should not panic and filter works
    let args = reflog::ReflogArgs::parse_from(["reflog", "show", "--grep", "commit"]);
    reflog::execute(args).await;
    // All our commits have "commit" in the message, so this should work

    // Test with --since filter (relative date) - should not panic
    let args = reflog::ReflogArgs::parse_from(["reflog", "show", "--since", "1 hour ago"]);
    reflog::execute(args).await;

    // Test with combined filters - should not panic
    let args = reflog::ReflogArgs::parse_from([
        "reflog",
        "show",
        "--grep",
        "Test",
        "--since",
        "1 day ago",
    ]);
    reflog::execute(args).await;

    // Test with --pretty flag - should not panic
    let args = reflog::ReflogArgs::parse_from([
        "reflog",
        "show",
        "--since",
        "1 day ago",
        "--pretty",
        "oneline",
    ]);
    reflog::execute(args).await;
}

#[tokio::test]
#[serial(cwd)]
async fn test_reflog_show_invalid_date() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Create a commit
    let commit_args = commit::CommitArgs {
        message: Some("Test commit".to_string()),
        file: None,
        allow_empty: true,
        conventional: false,
        amend: false,
        no_edit: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    };
    commit::execute(commit_args).await;

    // Test with invalid date format - should return error, not panic
    let args = reflog::ReflogArgs::parse_from(["reflog", "show", "--since", "invalid-date-format"]);
    let result = reflog::execute_safe(args, &OutputConfig::default()).await;
    assert!(
        result.is_err(),
        "invalid --since date should return an error"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_reflog_show_with_author_filter() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Create commits with different authors
    for i in 1..=3 {
        let commit_args = commit::CommitArgs {
            message: Some(format!("Test commit {}", i)),
            file: None,
            allow_empty: true,
            conventional: false,
            amend: false,
            no_edit: false,
            signoff: false,
            disable_pre: true,
            all: false,
            no_verify: false,
            author: None,
            ..Default::default()
        };
        commit::execute(commit_args).await;
    }

    // Verify: reflog entries should exist
    let total_entries = count_reflog_entries().await;
    assert!(
        total_entries >= 3,
        "Expected at least 3 reflog entries, found {}",
        total_entries
    );

    // Test with --author filter (should work without error)
    let args = reflog::ReflogArgs::parse_from(["reflog", "show", "--author", "test"]);
    reflog::execute(args).await; // Should filter entries by author

    // Test with case-insensitive author filter
    let args = reflog::ReflogArgs::parse_from(["reflog", "show", "--author", "TEST"]);
    reflog::execute(args).await; // Should work the same as lowercase

    // Verify: author filter doesn't delete entries
    let entries_after = count_reflog_entries().await;
    assert_eq!(
        total_entries, entries_after,
        "Author filter should not delete reflog entries"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_reflog_show_with_number_limit() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Create 5 commits
    for i in 1..=5 {
        let commit_args = commit::CommitArgs {
            message: Some(format!("Test commit {}", i)),
            file: None,
            allow_empty: true,
            conventional: false,
            amend: false,
            no_edit: false,
            signoff: false,
            disable_pre: true,
            all: false,
            no_verify: false,
            author: None,
            ..Default::default()
        };
        commit::execute(commit_args).await;
    }

    // Verify: At least 5 reflog entries should exist
    let total_entries = count_reflog_entries().await;
    assert!(
        total_entries >= 5,
        "Expected at least 5 reflog entries, found {}",
        total_entries
    );

    // Test with -n limit (should work without error)
    let args = reflog::ReflogArgs::parse_from(["reflog", "show", "-n", "3"]);
    reflog::execute(args).await; // Should display only 3 most recent entries

    // Test with --number limit
    let args = reflog::ReflogArgs::parse_from(["reflog", "show", "--number", "2"]);
    reflog::execute(args).await; // Should display only 2 most recent entries

    // Test that number limit doesn't affect total reflog count
    let entries_after = count_reflog_entries().await;
    assert_eq!(
        total_entries, entries_after,
        "Number limit should not delete reflog entries"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_reflog_show_with_combined_filters() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Create commits
    for i in 1..=3 {
        let commit_args = commit::CommitArgs {
            message: Some(format!("Test commit {}", i)),
            file: None,
            allow_empty: true,
            conventional: false,
            amend: false,
            no_edit: false,
            signoff: false,
            disable_pre: true,
            all: false,
            no_verify: false,
            author: None,
            ..Default::default()
        };
        commit::execute(commit_args).await;
    }

    // Verify: reflog entries should exist
    let total_entries = count_reflog_entries().await;
    assert!(
        total_entries >= 3,
        "Expected at least 3 reflog entries, found {}",
        total_entries
    );

    // Test with combined filters: author + number
    let args = reflog::ReflogArgs::parse_from(["reflog", "show", "--author", "test", "-n", "2"]);
    reflog::execute(args).await; // Should filter by author AND limit to 2 entries

    // Test with combined filters: since + author + number
    let args = reflog::ReflogArgs::parse_from([
        "reflog",
        "show",
        "--since",
        "1 day ago",
        "--author",
        "test",
        "--number",
        "5",
    ]);
    reflog::execute(args).await; // Should apply all three filters together

    // Verify: combined filters don't delete entries
    let entries_after = count_reflog_entries().await;
    assert_eq!(
        total_entries, entries_after,
        "Combined filters should not delete reflog entries"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_reflog_show_with_patch() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Create commits with file changes
    for i in 1..=3 {
        let commit_args = commit::CommitArgs {
            message: Some(format!("Test commit {}", i)),
            file: None,
            allow_empty: true,
            conventional: false,
            amend: false,
            no_edit: false,
            signoff: false,
            disable_pre: true,
            all: false,
            no_verify: false,
            author: None,
            ..Default::default()
        };
        commit::execute(commit_args).await;
    }

    // Test with --patch flag (should work without error)
    let args = reflog::ReflogArgs::parse_from(["reflog", "show", "--patch"]);
    reflog::execute(args).await; // Should complete without panic

    // Test with -p shorthand
    let args = reflog::ReflogArgs::parse_from(["reflog", "show", "-p"]);
    reflog::execute(args).await; // Should complete without panic

    // Test with patch and number limit
    let args = reflog::ReflogArgs::parse_from(["reflog", "show", "--patch", "-n", "2"]);
    reflog::execute(args).await; // Should complete without panic
}

#[tokio::test]
#[serial(cwd)]
async fn test_reflog_show_with_stat() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Create commits
    for i in 1..=3 {
        let commit_args = commit::CommitArgs {
            message: Some(format!("Test commit {}", i)),
            file: None,
            allow_empty: true,
            conventional: false,
            amend: false,
            no_edit: false,
            signoff: false,
            disable_pre: true,
            all: false,
            no_verify: false,
            author: None,
            ..Default::default()
        };
        commit::execute(commit_args).await;
    }

    // Test with --stat flag (should work without error)
    let args = reflog::ReflogArgs::parse_from(["reflog", "show", "--stat"]);
    reflog::execute(args).await; // Should complete without panic

    // Test with stat and number limit
    let args = reflog::ReflogArgs::parse_from(["reflog", "show", "--stat", "-n", "2"]);
    reflog::execute(args).await; // Should complete without panic
}

/// `libra reflog --help` surfaces the EXAMPLES banner so users see the
/// three sub-commands (`show`, `delete`, `exists`) plus a filtered show,
/// a HEAD@{N} delete selector, and the JSON variant for agents. Cross-cutting
/// `--help` EXAMPLES rollout per `docs/development/commands/_general.md` item B.
#[test]
fn test_reflog_help_lists_examples_banner() {
    let repo = tempdir().expect("tempdir for reflog --help");
    let output = run_libra_command(&["reflog", "--help"], repo.path());
    assert!(
        output.status.success(),
        "reflog --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "reflog --help should include EXAMPLES banner, stdout: {stdout}"
    );
    for invocation in [
        "libra reflog show",
        "libra reflog show main --number 20",
        "libra reflog exists refs/heads/feature-x",
        "libra reflog delete HEAD@{2}",
        "libra reflog expire --all --dry-run",
        "libra reflog expire --expire=now --all",
        "libra reflog --json show HEAD",
    ] {
        assert!(
            stdout.contains(invocation),
            "reflog --help should include `{invocation}`, stdout: {stdout}"
        );
    }
}

// ── reflog expire ─────────────────────────────────────────────────────────

#[test]
fn test_reflog_expire_no_refs_exits_128() {
    // Intentional difference from Git's silent no-op: an explicit error.
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["reflog", "expire"], repo.path());
    assert_eq!(output.status.code(), Some(128));
    let (stderr, _report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        stderr.contains("no reflog specified"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn test_reflog_expire_invalid_date_reports_cli_002() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(
        &["reflog", "expire", "--all", "--expire=garbage"],
        repo.path(),
    );
    assert_eq!(output.status.code(), Some(129));
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(stderr.contains("invalid expire time"), "stderr: {stderr}");
}

#[test]
fn test_reflog_expire_all_dry_run_no_change() {
    let repo = create_committed_repo_via_cli();

    let before = run_libra_command(&["--json", "reflog", "show", "HEAD"], repo.path());
    let before_count = parse_json_stdout(&before)["data"]["total_count"]
        .as_u64()
        .unwrap_or(0);
    assert!(before_count >= 1, "fixture should have reflog entries");

    // --expire=all marks everything, but --dry-run must not delete.
    let dry = run_libra_command(
        &["reflog", "expire", "--all", "--dry-run", "--expire=all"],
        repo.path(),
    );
    assert_cli_success(&dry, "reflog expire --all --dry-run");

    let after = run_libra_command(&["--json", "reflog", "show", "HEAD"], repo.path());
    let after_count = parse_json_stdout(&after)["data"]["total_count"]
        .as_u64()
        .unwrap_or(0);
    assert_eq!(before_count, after_count, "dry-run must not prune entries");
}

#[test]
fn test_reflog_expire_json_envelope_and_prunes() {
    let repo = create_committed_repo_via_cli();

    // Dry-run JSON envelope: each ref reports pruned > 0 under --expire=all.
    let dry = run_libra_command(
        &[
            "--json",
            "reflog",
            "expire",
            "--all",
            "--dry-run",
            "--expire=all",
        ],
        repo.path(),
    );
    assert_cli_success(&dry, "json reflog expire dry-run");
    let json = parse_json_stdout(&dry);
    assert_eq!(json["command"], "reflog.expire");
    let data = json["data"].as_array().expect("expire data array");
    assert!(!data.is_empty(), "expire should report at least one ref");
    assert!(data.iter().all(|entry| entry["ref_name"].is_string()));
    assert!(
        data.iter()
            .any(|entry| entry["pruned"].as_u64().unwrap_or(0) > 0),
        "under --expire=all some ref should report pruned entries: {json}"
    );

    // Real prune, then a follow-up dry-run should find nothing left to prune.
    let real = run_libra_command(&["reflog", "expire", "--all", "--expire=all"], repo.path());
    assert_cli_success(&real, "reflog expire --all --expire=all");

    let again = run_libra_command(
        &[
            "--json",
            "reflog",
            "expire",
            "--all",
            "--dry-run",
            "--expire=all",
        ],
        repo.path(),
    );
    assert_cli_success(&again, "second dry-run");
    let again_data = parse_json_stdout(&again)["data"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        again_data
            .iter()
            .all(|entry| entry["pruned"].as_u64().unwrap_or(0) == 0),
        "after a real prune nothing should remain to prune"
    );
}

#[test]
fn test_reflog_no_abbrev_shows_full_hash() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // Default reflog abbreviates the object name to 7 chars.
    let abbrev = run_libra_command(&["reflog"], p);
    assert_cli_success(&abbrev, "reflog");
    let ab_out = String::from_utf8_lossy(&abbrev.stdout);
    let ab_hash = ab_out.split_whitespace().next().expect("abbreviated hash");
    assert_eq!(ab_hash.len(), 7, "default abbreviates to 7: {ab_out:?}");

    // --no-abbrev prints the full object name (40 hex for sha1, 64 for sha256).
    let full = run_libra_command(&["reflog", "show", "--no-abbrev"], p);
    assert_cli_success(&full, "reflog --no-abbrev");
    let full_out = String::from_utf8_lossy(&full.stdout);
    let full_hash = full_out.split_whitespace().next().expect("full hash");
    assert!(
        full_hash.len() == 40 || full_hash.len() == 64,
        "--no-abbrev shows the full object name (sha1=40/sha256=64): {full_out:?}"
    );
    assert!(
        full_hash.len() > ab_hash.len(),
        "full hash is longer than the abbreviation"
    );
    assert!(full_hash.starts_with(ab_hash), "abbrev is a prefix of full");
}
