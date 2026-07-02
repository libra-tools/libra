//! Tests log command output ordering and formatting of commit history.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::{cmp::min, str::FromStr};

use clap::Parser;
use git_internal::{
    Diff,
    hash::ObjectHash,
    internal::object::{blob::Blob, commit::Commit, tree::Tree},
};
use libra::{
    internal::{db::get_db_conn_instance, model::reference},
    utils::{object_ext::TreeExt, output::OutputConfig, pager::LIBRA_PAGER_ENV, util},
};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serial_test::serial;

use super::*;

#[test]
fn test_log_cli_outside_repository_returns_fatal_128() {
    let temp = tempdir().unwrap();

    let output = run_libra_command(&["log", "--oneline"], temp.path());
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal: not a libra repository"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn test_log_cli_empty_repository_returns_fatal_128() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["log", "--oneline"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-003");
    assert_eq!(
        report.message,
        "your current branch 'main' does not have any commits yet"
    );
    assert!(
        report
            .hints
            .iter()
            .any(|hint| hint == "create a commit first before running 'libra log'."),
        "missing log hint: {:?}",
        report.hints
    );
    assert_eq!(
        stderr,
        "fatal: your current branch 'main' does not have any commits yet\nError-Code: LBR-REPO-003\n\nHint: create a commit first before running 'libra log'."
    );
}

#[test]
fn test_log_first_parent_skips_merged_branch_commits() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();

    // main: a unique commit.
    std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], repo.path()), "add a");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "MAIN_ONE", "--no-verify"], repo.path()),
        "commit MAIN_ONE",
    );

    // feature: a side-branch commit with a distinct subject + file.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], repo.path()),
        "switch -c feature",
    );
    std::fs::write(repo.path().join("b.txt"), "b\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.txt"], repo.path()), "add b");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "SIDE_BRANCH", "--no-verify"], repo.path()),
        "commit SIDE_BRANCH",
    );

    // main diverges, then merges feature -> a two-parent merge commit.
    assert_cli_success(
        &run_libra_command(&["switch", "main"], repo.path()),
        "switch main",
    );
    std::fs::write(repo.path().join("a.txt"), "a2\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], repo.path()), "add a2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "MAIN_TWO", "--no-verify"], repo.path()),
        "commit MAIN_TWO",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "feature"], repo.path()),
        "merge feature",
    );

    // Plain log includes the merged side-branch commit; --first-parent omits it.
    let plain = run_libra_command(&["log", "--oneline"], repo.path());
    assert_cli_success(&plain, "log --oneline");
    let plain_out = String::from_utf8_lossy(&plain.stdout);
    assert!(
        plain_out.contains("SIDE_BRANCH"),
        "plain log should include the side-branch commit:\n{plain_out}"
    );

    let fp = run_libra_command(&["log", "--first-parent", "--oneline"], repo.path());
    assert_cli_success(&fp, "log --first-parent --oneline");
    let fp_out = String::from_utf8_lossy(&fp.stdout);
    assert!(
        !fp_out.contains("SIDE_BRANCH"),
        "--first-parent should omit the merged side-branch commit:\n{fp_out}"
    );
    assert!(
        fp_out.contains("MAIN_TWO"),
        "--first-parent should keep first-parent history:\n{fp_out}"
    );
}

#[test]
fn test_log_pickaxe_s_finds_commit_that_changes_string_count() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();

    // A commit that does NOT touch the needle string.
    std::fs::write(repo.path().join("plain.txt"), "nothing special here\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "plain.txt"], repo.path()),
        "add plain.txt",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "NEEDLE_ABSENT", "--no-verify"],
            repo.path(),
        ),
        "commit NEEDLE_ABSENT",
    );

    // A commit that introduces the needle string.
    std::fs::write(repo.path().join("target.txt"), "line with FINDME token\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "target.txt"], repo.path()),
        "add target.txt",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "NEEDLE_ADDED", "--no-verify"],
            repo.path(),
        ),
        "commit NEEDLE_ADDED",
    );

    let out = run_libra_command(&["log", "-S", "FINDME", "--oneline"], repo.path());
    assert_cli_success(&out, "log -S FINDME");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("NEEDLE_ADDED"),
        "pickaxe -S should include the commit that added the string:\n{stdout}"
    );
    assert!(
        !stdout.contains("NEEDLE_ABSENT"),
        "pickaxe -S should skip commits that don't change the string count:\n{stdout}"
    );
}

#[test]
fn test_log_pickaxe_g_matches_diff_line_regex() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();

    // A commit whose added line does NOT match the regex.
    std::fs::write(repo.path().join("a.txt"), "plain content\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "a.txt"], repo.path()),
        "add a.txt",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "GNOMATCH", "--no-verify"], repo.path()),
        "commit GNOMATCH",
    );

    // A commit whose added line matches the regex.
    std::fs::write(repo.path().join("b.txt"), "fn handler_v2() {}\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "b.txt"], repo.path()),
        "add b.txt",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "GMATCH", "--no-verify"], repo.path()),
        "commit GMATCH",
    );

    let out = run_libra_command(&["log", "-G", "handler_v[0-9]", "--oneline"], repo.path());
    assert_cli_success(&out, "log -G handler_v[0-9]");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("GMATCH"),
        "-G should match the commit whose diff line matches the regex:\n{stdout}"
    );
    assert!(
        !stdout.contains("GNOMATCH"),
        "-G should skip commits with no matching added/removed line:\n{stdout}"
    );
}

#[test]
fn test_log_skip_omits_leading_commits() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    for tag in ["SK_A", "SK_B", "SK_C"] {
        let file = format!("{tag}.txt");
        std::fs::write(repo.path().join(&file), "x\n").unwrap();
        assert_cli_success(
            &run_libra_command(&["add", file.as_str()], repo.path()),
            "add skip file",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", tag, "--no-verify"], repo.path()),
            "commit skip",
        );
    }

    // Newest is SK_C; --skip 1 -n 1 should show only the 2nd-newest (SK_B).
    let out = run_libra_command(&["log", "--skip", "1", "-n", "1", "--oneline"], repo.path());
    assert_cli_success(&out, "log --skip 1 -n 1");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("SK_B"),
        "--skip 1 -n 1 should show the 2nd-newest commit:\n{stdout}"
    );
    assert!(
        !stdout.contains("SK_C"),
        "--skip 1 should omit the newest commit:\n{stdout}"
    );
    assert!(
        !stdout.contains("SK_A"),
        "-n 1 should not reach older commits:\n{stdout}"
    );
}

#[tokio::test]
#[serial]
async fn test_log_corrupt_head_reference_returns_repo_corrupt() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    let db = get_db_conn_instance().await;
    let head = reference::Entity::find()
        .filter(reference::Column::Kind.eq(reference::ConfigKind::Head))
        .filter(reference::Column::Remote.is_null())
        .one(&db)
        .await
        .unwrap()
        .expect("expected HEAD row");
    let mut head: reference::ActiveModel = head.into();
    head.name = Set(None);
    head.commit = Set(Some("not-a-valid-hash".to_string()));
    head.update(&db).await.unwrap();

    let output = run_libra_command(&["log", "--oneline"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert!(
        stderr.contains("failed to resolve HEAD"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        stderr.contains("invalid detached HEAD commit hash"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn test_log_json_output_includes_commit_list() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "log", "-n", "1"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "log");
    assert_eq!(json["data"]["commits"][0]["subject"], "base");
    assert!(json["data"]["commits"][0]["files"].as_array().is_some());
}

#[tokio::test]
#[serial]
async fn test_log_quiet_does_not_initialize_pager() {
    if cfg!(windows) {
        return;
    }

    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());
    let missing_bin_dir = tempdir().unwrap();
    let _path = test::ScopedEnvVar::set("PATH", missing_bin_dir.path());
    let _pager = test::ScopedEnvVar::set(LIBRA_PAGER_ENV, "always");

    let args = LogArgs::try_parse_from(["libra", "--oneline"]).unwrap();
    let output = OutputConfig {
        quiet: true,
        ..OutputConfig::default()
    };

    let result = libra::command::log::execute_safe(args, &output).await;
    assert!(
        result.is_ok(),
        "quiet log should not initialize pager: {result:?}"
    );
}

#[test]
fn test_log_invalid_since_uses_command_usage_error() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["log", "--since", "not-a-date"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert!(stderr.starts_with("error: "));
    assert!(stderr.contains("supported formats: YYYY-MM-DD"));
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert_eq!(report.category, "cli");
    assert_eq!(report.exit_code, 129);
    assert_eq!(report.severity, "error");
}

#[test]
fn test_log_invalid_decorate_uses_command_usage_error() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "log", "--decorate=bogus"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert!(
        output.stdout.is_empty(),
        "unexpected stdout: {:?}",
        output.stdout
    );
    assert!(stderr.is_empty(), "unexpected human stderr: {stderr}");
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert_eq!(report.category, "cli");
    assert_eq!(report.exit_code, 129);
    assert_eq!(report.severity, "error");
    assert_eq!(report.message, "invalid --decorate option: bogus");
    assert_eq!(report.hints, vec!["valid options: no, short, full, auto"]);
}

#[tokio::test]
#[serial]
async fn test_log_decorate_no_skips_corrupt_reference_map() {
    let repo = create_committed_repo_via_cli();

    let create_branch = run_libra_command(&["branch", "topic"], repo.path());
    assert!(
        create_branch.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create_branch.stderr)
    );

    let _guard = ChangeDirGuard::new(repo.path());
    let db = get_db_conn_instance().await;
    let topic = reference::Entity::find()
        .filter(reference::Column::Kind.eq(reference::ConfigKind::Branch))
        .filter(reference::Column::Name.eq("topic"))
        .filter(reference::Column::Remote.is_null())
        .one(&db)
        .await
        .unwrap()
        .expect("expected topic branch row");
    let mut topic: reference::ActiveModel = topic.into();
    topic.commit = Set(Some("not-a-valid-hash".to_string()));
    topic.update(&db).await.unwrap();

    let output = run_libra_command(&["log", "--decorate=no", "--oneline"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("base"),
        "expected log output to remain available, got: {stdout}"
    );
}

#[tokio::test]
#[serial]
async fn test_log_patch_fails_when_commit_blob_is_missing() {
    let repo = create_committed_repo_via_cli();

    let tracked_blob = {
        let _guard = ChangeDirGuard::new(repo.path());
        let head = Head::current_commit().await.expect("expected HEAD commit");
        let commit: Commit = load_object(&head).expect("expected HEAD commit object");
        let tree: Tree = load_object(&commit.tree_id).expect("expected HEAD tree");
        tree.get_plain_items()
            .into_iter()
            .find(|(path, _)| path == &std::path::PathBuf::from("tracked.txt"))
            .map(|(_, hash)| hash.to_string())
            .expect("expected tracked.txt blob in HEAD tree")
    };
    std::fs::remove_file(loose_object_path(repo.path(), &tracked_blob))
        .expect("failed to delete committed blob");
    std::fs::write(
        repo.path().join("tracked.txt"),
        "mutated worktree fallback\n",
    )
    .expect("failed to mutate worktree file");

    let output = run_libra_command(&["log", "-n", "1", "--patch"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert!(
        stderr.contains("failed to load blob object"),
        "expected repo corruption error, got: {stderr}"
    );
}

#[tokio::test]
#[serial]
async fn test_log_quiet_patch_fails_when_commit_blob_is_missing() {
    let repo = create_committed_repo_via_cli();

    let tracked_blob = {
        let _guard = ChangeDirGuard::new(repo.path());
        let head = Head::current_commit().await.expect("expected HEAD commit");
        let commit: Commit = load_object(&head).expect("expected HEAD commit object");
        let tree: Tree = load_object(&commit.tree_id).expect("expected HEAD tree");
        tree.get_plain_items()
            .into_iter()
            .find(|(path, _)| path == &std::path::PathBuf::from("tracked.txt"))
            .map(|(_, hash)| hash.to_string())
            .expect("expected tracked.txt blob in HEAD tree")
    };
    std::fs::remove_file(loose_object_path(repo.path(), &tracked_blob))
        .expect("failed to delete committed blob");
    std::fs::write(
        repo.path().join("tracked.txt"),
        "mutated worktree fallback\n",
    )
    .expect("failed to mutate worktree file");

    let output = run_libra_command(&["--quiet", "log", "-n", "1", "--patch"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert!(
        stderr.contains("failed to load blob object"),
        "expected repo corruption error, got: {stderr}"
    );
}

#[tokio::test]
#[serial]
async fn test_log_quiet_stat_respects_selected_history_range() {
    let repo = create_committed_repo_via_cli();

    std::fs::write(repo.path().join("tracked.txt"), "tracked\nsecond\n").unwrap();
    let add_second = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert!(
        add_second.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&add_second.stderr)
    );
    let commit_second = run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path());
    assert!(
        commit_second.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&commit_second.stderr)
    );

    std::fs::write(repo.path().join("tracked.txt"), "tracked\nthird\n").unwrap();
    let add_third = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert!(
        add_third.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&add_third.stderr)
    );
    let commit_third = run_libra_command(&["commit", "-m", "third", "--no-verify"], repo.path());
    assert!(
        commit_third.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&commit_third.stderr)
    );

    let oldest_blob = {
        let _guard = ChangeDirGuard::new(repo.path());
        let head = Head::current_commit().await.expect("expected HEAD commit");
        let latest: Commit = load_object(&head).expect("expected latest commit");
        let middle_id = latest.parent_commit_ids[0];
        let middle: Commit = load_object(&middle_id).expect("expected middle commit");
        let oldest_id = middle.parent_commit_ids[0];
        let oldest: Commit = load_object(&oldest_id).expect("expected oldest commit");
        let tree: Tree = load_object(&oldest.tree_id).expect("expected oldest tree");
        tree.get_plain_items()
            .into_iter()
            .find(|(path, _)| path == &std::path::PathBuf::from("tracked.txt"))
            .map(|(_, hash)| hash.to_string())
            .expect("expected tracked.txt blob in oldest tree")
    };
    std::fs::remove_file(loose_object_path(repo.path(), &oldest_blob))
        .expect("failed to delete oldest committed blob");
    std::fs::write(
        repo.path().join("tracked.txt"),
        "mutated worktree fallback\n",
    )
    .expect("failed to mutate worktree file");

    let top_only = run_libra_command(&["--quiet", "log", "-n", "1", "--stat"], repo.path());
    assert!(
        top_only.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&top_only.stderr)
    );

    let output = run_libra_command(&["--quiet", "log", "-n", "2", "--stat"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert!(
        stderr.contains("failed to load blob object"),
        "expected repo corruption error, got: {stderr}"
    );

    // `--shortstat` reads the same per-commit stats, so quiet mode must validate
    // (and fail on) the missing blob just like `--stat`.
    let short_top = run_libra_command(&["--quiet", "log", "-n", "1", "--shortstat"], repo.path());
    assert!(
        short_top.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&short_top.stderr)
    );
    let short_output =
        run_libra_command(&["--quiet", "log", "-n", "2", "--shortstat"], repo.path());
    let (short_stderr, short_report) = parse_cli_error_stderr(&short_output.stderr);
    assert_eq!(short_output.status.code(), Some(128));
    assert_eq!(short_report.error_code, "LBR-REPO-002");
    assert!(
        short_stderr.contains("failed to load blob object"),
        "expected --shortstat to fail like --stat, got: {short_stderr}"
    );
}

#[test]
fn test_log_json_total_reflects_filtered_scope() {
    let repo = create_committed_repo_via_cli();

    let name_output = run_libra_command(&["config", "user.name", "Other User"], repo.path());
    assert!(
        name_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&name_output.stderr)
    );
    let email_output =
        run_libra_command(&["config", "user.email", "other@example.com"], repo.path());
    assert!(
        email_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&email_output.stderr)
    );

    std::fs::write(
        repo.path().join("tracked.txt"),
        "tracked\nupdated by other\n",
    )
    .expect("failed to update tracked.txt");
    let add_output = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert!(
        add_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&add_output.stderr)
    );
    let commit_output = run_libra_command(
        &["commit", "-m", "other update", "--no-verify"],
        repo.path(),
    );
    assert!(
        commit_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&commit_output.stderr)
    );

    let output = run_libra_command(&["--json", "log", "--author", "Other User"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "log");
    assert_eq!(json["data"]["total"], 1);
    let commits = json["data"]["commits"]
        .as_array()
        .expect("commits should be an array");
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0]["author_name"], "Other User");
    assert_eq!(commits[0]["subject"], "other update");
}

#[tokio::test]
#[serial]
/// Tests retrieval of commits reachable from a specific commit hash
async fn test_get_reachable_commits() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let commit_id = create_test_commit_tree().await;

    let reachable_commits = get_reachable_commits(commit_id, None).await.unwrap();
    assert_eq!(reachable_commits.len(), 6);
}

#[tokio::test]
#[serial]
/// Tests log command execution functionality
async fn test_execute_log() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    let _ = create_test_commit_tree().await;

    // let args = LogArgs { number: Some(1) };
    // execute(args).await;
    let head = Head::current().await;
    // check if the current branch has any commits
    if let Head::Branch(branch_name) = head.to_owned() {
        // Migrated from `Branch::find_branch` (lossy wrapper) to
        // `Branch::find_branch_result` per `docs/development/commands/branch.md` —
        // storage errors no longer silently degrade to "no commits yet".
        match Branch::find_branch_result(&branch_name, None).await {
            Ok(Some(_)) => {}
            Ok(None) => {
                panic!("fatal: your current branch '{branch_name}' does not have any commits yet ")
            }
            Err(err) => {
                panic!("fatal: failed to query branch '{branch_name}': {err:?}")
            }
        }
    }

    let commit_hash = Head::current_commit().await.unwrap().to_string();

    let mut reachable_commits = get_reachable_commits(commit_hash.clone(), None)
        .await
        .unwrap();
    // newest first
    reachable_commits.sort_by_key(|c| std::cmp::Reverse(c.committer.timestamp));

    //the last seven commits
    let max_output_number = min(6, reachable_commits.len());
    let expected_msgs = [
        "Commit_6", "Commit_5", "Commit_4", "Commit_3", "Commit_2", "Commit_1",
    ];
    for (i, commit) in reachable_commits.iter().take(max_output_number).enumerate() {
        let msg = commit.message.trim_start_matches('\n');
        assert_eq!(msg, expected_msgs[i]);
    }
}

/// create a test commit tree structure as graph and create branch (master) head to commit 6
/// return a commit hash of commit 6
///            3 --  6
///          /      /
///    1 -- 2  --  5
//           \   /   \
///            4     7
async fn create_test_commit_tree() -> String {
    let mut commit_1 = Commit::from_tree_id(
        ObjectHash::new(&[1; 20]),
        vec![],
        &format_commit_msg("Commit_1", None),
    );
    commit_1.committer.timestamp = 1;
    // save_object(&commit_1);
    save_object(&commit_1, &commit_1.id).unwrap();

    let mut commit_2 = Commit::from_tree_id(
        ObjectHash::new(&[2; 20]),
        vec![commit_1.id],
        &format_commit_msg("Commit_2", None),
    );
    commit_2.committer.timestamp = 2;
    save_object(&commit_2, &commit_2.id).unwrap();

    let mut commit_3 = Commit::from_tree_id(
        ObjectHash::new(&[3; 20]),
        vec![commit_2.id],
        &format_commit_msg("Commit_3", None),
    );
    commit_3.committer.timestamp = 3;
    save_object(&commit_3, &commit_3.id).unwrap();

    let mut commit_4 = Commit::from_tree_id(
        ObjectHash::new(&[4; 20]),
        vec![commit_2.id],
        &format_commit_msg("Commit_4", None),
    );
    commit_4.committer.timestamp = 4;
    save_object(&commit_4, &commit_4.id).unwrap();

    let mut commit_5 = Commit::from_tree_id(
        ObjectHash::new(&[5; 20]),
        vec![commit_2.id, commit_4.id],
        &format_commit_msg("Commit_5", None),
    );
    commit_5.committer.timestamp = 5;
    save_object(&commit_5, &commit_5.id).unwrap();

    let mut commit_6 = Commit::from_tree_id(
        ObjectHash::new(&[6; 20]),
        vec![commit_3.id, commit_5.id],
        &format_commit_msg("Commit_6", None),
    );
    commit_6.committer.timestamp = 6;
    save_object(&commit_6, &commit_6.id).unwrap();

    let mut commit_7 = Commit::from_tree_id(
        ObjectHash::new(&[7; 20]),
        vec![commit_5.id],
        &format_commit_msg("Commit_7", None),
    );
    commit_7.committer.timestamp = 7;
    save_object(&commit_7, &commit_7.id).unwrap();

    // set current branch head to commit 6
    let head = Head::current().await;
    let branch_name = match head {
        Head::Branch(name) => name,
        _ => panic!("should be branch"),
    };

    Branch::update_branch(&branch_name, &commit_6.id.to_string(), None)
        .await
        .unwrap();

    commit_6.id.to_string()
}

#[tokio::test]
#[serial]
/// Tests log command with --oneline parameter
async fn test_log_oneline() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create test commits
    let commit_id = create_test_commit_tree().await;
    let reachable_commits = get_reachable_commits(commit_id, None).await.unwrap();

    // Test oneline format
    let args = LogArgs::try_parse_from(["libra", "--number", "3", "--oneline"]);

    // Since execute function writes to stdout, we'll test the logic directly
    let mut sorted_commits = reachable_commits.clone();
    sorted_commits.sort_by_key(|c| std::cmp::Reverse(c.committer.timestamp));

    let max_commits = std::cmp::min(
        args.unwrap().number.unwrap_or(usize::MAX),
        sorted_commits.len(),
    );

    let expected_msgs = ["Commit_6", "Commit_5", "Commit_4"];
    for (i, commit) in sorted_commits.iter().take(max_commits).enumerate() {
        // Test short hash format (should be 7 characters)
        let short_hash = &commit.id.to_string()[..7];
        assert_eq!(short_hash.len(), 7);

        // Test that commit message parsing works
        let (msg, _) = libra::common_utils::parse_commit_msg(&commit.message);
        assert!(!msg.is_empty());

        // For our test commits, verify the expected format
        assert_eq!(msg.trim(), expected_msgs[i]);
    }
}

#[tokio::test]
#[serial]
/// Tests log -p (patch) without pathspec: create A -> commit -> create B -> commit -> assert diffs contain both A and B contents
async fn test_log_patch_no_pathspec() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create file A and commit
    test::ensure_file("A.txt", Some("Content A\n"));
    add::execute(AddArgs {
        pathspec: vec![String::from("A.txt")],
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
        message: Some("Add A".to_string()),
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

    // Create file B and commit
    test::ensure_file("B.txt", Some("Content B\n"));
    add::execute(AddArgs {
        pathspec: vec![String::from("B.txt")],
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
        message: Some("Add B".to_string()),
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

    let bin_dir = temp_path.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let out_file = temp_path.path().join("less_out.txt");

    // On Windows we inline diff generation to avoid relying on spawned pager
    if cfg!(windows) {
        let diffs = collect_combined_diff_for_commits(2, Vec::new()).await;
        assert!(
            diffs.contains("Content A"),
            "patch should contain A content, got: {}",
            diffs
        );
        assert!(
            diffs.contains("Content B"),
            "patch should contain B content, got: {}",
            diffs
        );
    } else {
        // Unix: create shell script that writes stdin to file
        let less_path = bin_dir.join("less");
        let script = format!("#!/bin/sh\ncat - > \"{}\"\n", out_file.display());
        std::fs::write(&less_path, script.as_bytes()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&less_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // Set PATH and run
        let old_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", bin_dir.display(), old_path);
        let _path = test::ScopedEnvVar::set("PATH", &new_path);
        let _pager = test::ScopedEnvVar::set(LIBRA_PAGER_ENV, "always");

        let args = LogArgs::try_parse_from(["libra", "--number", "2", "-p"]).unwrap();
        libra::command::log::execute(args).await;

        let combined_out = std::fs::read_to_string(&out_file).unwrap_or_default();
        assert!(
            combined_out.contains("Content A"),
            "patch should contain A content, got: {}",
            combined_out
        );
        assert!(
            combined_out.contains("Content B"),
            "patch should contain B content, got: {}",
            combined_out
        );
    }
}

#[tokio::test]
#[serial]
/// Tests log -p with a specific pathspec: commit contains A and B, but log -p A should only include A
async fn test_log_patch_with_pathspec() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create files A and B and commit both in one commit
    test::ensure_file("A.txt", Some("Content A\n"));
    test::ensure_file("B.txt", Some("Content B\n"));

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
        message: Some("Add A and B".to_string()),
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

    let bin_dir = temp_path.path().join("bin2");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let out_file = temp_path.path().join("less_out_pathspec.txt");

    if cfg!(windows) {
        let paths = vec![util::to_workdir_path("A.txt")];
        let diffs = collect_combined_diff_for_commits(1, paths).await;
        assert!(
            diffs.contains("Content A"),
            "patch should contain A content, got: {}",
            diffs
        );
        assert!(
            !diffs.contains("Content B"),
            "patch should not contain B content when pathspec is A, got: {}",
            diffs
        );
    } else {
        let less_path = bin_dir.join("less");
        let script = format!("#!/bin/sh\ncat - > \"{}\"\n", out_file.display());
        std::fs::write(&less_path, script.as_bytes()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&less_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let old_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", bin_dir.display(), old_path);
        let _path = test::ScopedEnvVar::set("PATH", &new_path);
        let _pager = test::ScopedEnvVar::set(LIBRA_PAGER_ENV, "always");

        let args = LogArgs::try_parse_from(["libra", "-p", "A.txt"]).unwrap();
        libra::command::log::execute(args).await;

        let out = std::fs::read_to_string(out_file).unwrap_or_default();
        assert!(
            out.contains("Content A"),
            "patch should contain A content, got: {}",
            out
        );
        assert!(
            !out.contains("Content B"),
            "patch should not contain B content when pathspec is A, got: {}",
            out
        );
    }
}

async fn collect_combined_diff_for_commits(count: usize, paths: Vec<std::path::PathBuf>) -> String {
    // Get head commit and reachable commits
    let commit_hash = Head::current_commit().await.unwrap().to_string();
    let reachable_commits = get_reachable_commits(commit_hash, None).await.unwrap();

    let max_output_number = std::cmp::min(count, reachable_commits.len());
    let mut out = String::new();
    for commit in reachable_commits.into_iter().take(max_output_number) {
        let tree = load_object::<Tree>(&commit.tree_id).unwrap();
        let new_blobs: Vec<(std::path::PathBuf, ObjectHash)> = tree.get_plain_items();

        let old_blobs: Vec<(std::path::PathBuf, ObjectHash)> =
            if !commit.parent_commit_ids.is_empty() {
                let parent = &commit.parent_commit_ids[0];
                let parent_hash = ObjectHash::from_str(&parent.to_string()).unwrap();
                let parent_commit = load_object::<Commit>(&parent_hash).unwrap();
                let parent_tree = load_object::<Tree>(&parent_commit.tree_id).unwrap();
                parent_tree.get_plain_items()
            } else {
                Vec::new()
            };

        let read_content =
            |file: &std::path::PathBuf, hash: &ObjectHash| match load_object::<Blob>(hash) {
                Ok(blob) => blob.data,
                Err(_) => {
                    let file = util::to_workdir_path(file);
                    std::fs::read(&file).unwrap()
                }
            };

        let diffs = Diff::diff(
            old_blobs,
            new_blobs,
            paths.clone().into_iter().collect(),
            read_content,
        );
        for d in diffs {
            out.push_str(&d.data);
        }
    }
    out
}

#[tokio::test]
#[serial]
async fn test_log_stat() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    test::ensure_file("file1.txt", Some("line1\nline2\nline3\n"));
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
        message: Some("Add file1".to_string()),
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

    test::ensure_file("file2.txt", Some("content A\ncontent B\n"));
    add::execute(AddArgs {
        pathspec: vec![String::from("file2.txt")],
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
        message: Some("Add file2".to_string()),
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

    let commit_hash = Head::current_commit().await.unwrap().to_string();
    let commit_id = ObjectHash::from_str(&commit_hash).unwrap();
    let commit = load_object::<Commit>(&commit_id).unwrap();

    let stats = libra::command::log::compute_commit_stat(&commit, Vec::new())
        .await
        .unwrap();

    assert!(!stats.is_empty());
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].path, "file2.txt");
    assert_eq!(stats[0].insertions, 2);
    assert_eq!(stats[0].deletions, 0);

    let stat_output = libra::command::log::format_stat_output(&stats);
    assert!(stat_output.contains("file2.txt"));
    assert!(stat_output.contains("2"));
    assert!(stat_output.contains("1 file"));
    assert!(stat_output.contains("2 insertion"));
}

#[tokio::test]
#[serial]
async fn test_log_patch_with_stat_shows_diffstat_before_patch() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    test::ensure_file("file1.txt", Some("line1\nline2\n"));
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
        message: Some("add file1".to_string()),
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

    test::ensure_file("file1.txt", Some("line1\nline2\nline3\n"));
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
        message: Some("extend file1".to_string()),
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

    // `--patch-with-stat` emits the diffstat block, then the patch.
    let out = run_libra_command(&["log", "--patch-with-stat", "-1"], temp_path.path());
    assert_cli_success(&out, "log --patch-with-stat");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stat_pos = stdout
        .find("1 file changed")
        .expect("diffstat summary present");
    let patch_pos = stdout.find("diff --git").expect("patch body present");
    assert!(
        stat_pos < patch_pos,
        "diffstat must precede the patch body:\n{stdout}"
    );
    assert!(
        stdout.contains("+line3"),
        "patch shows the added line:\n{stdout}"
    );

    // `-p --stat` is the same thing and likewise shows both, in stat-then-patch order.
    let combo = run_libra_command(&["log", "-p", "--stat", "-1"], temp_path.path());
    assert_cli_success(&combo, "log -p --stat");
    let combo_out = String::from_utf8_lossy(&combo.stdout);
    let combo_stat = combo_out.find("1 file changed").expect("stat present");
    let combo_patch = combo_out.find("diff --git").expect("patch present");
    assert!(
        combo_stat < combo_patch,
        "-p --stat: stat before patch:\n{combo_out}"
    );

    // Plain `-p` still shows the patch with no diffstat summary line.
    let patch_only = run_libra_command(&["log", "-p", "-1"], temp_path.path());
    assert_cli_success(&patch_only, "log -p");
    let patch_only_out = String::from_utf8_lossy(&patch_only.stdout);
    assert!(patch_only_out.contains("diff --git"));
    assert!(
        !patch_only_out.contains("1 file changed"),
        "plain -p has no diffstat summary:\n{patch_only_out}"
    );
}

#[tokio::test]
#[serial]
async fn test_log_stat_with_modifications() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    test::ensure_file("test.txt", Some("line1\nline2\nline3\n"));
    add::execute(AddArgs {
        pathspec: vec![String::from("test.txt")],
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

    test::ensure_file("test.txt", Some("line1\nline2 modified\nline3\nline4\n"));
    add::execute(AddArgs {
        pathspec: vec![String::from("test.txt")],
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
        message: Some("Modify test.txt".to_string()),
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

    let commit_hash = Head::current_commit().await.unwrap().to_string();
    let commit_id = ObjectHash::from_str(&commit_hash).unwrap();
    let commit = load_object::<Commit>(&commit_id).unwrap();

    let stats = libra::command::log::compute_commit_stat(&commit, Vec::new())
        .await
        .unwrap();

    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].path, "test.txt");
    assert_eq!(stats[0].insertions, 2);
    assert_eq!(stats[0].deletions, 1);
}

#[tokio::test]
#[serial]
/// Tests log command with commit hash abbreviation parameters
async fn test_log_abbrev_params() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create test commits
    let commit_id = create_test_commit_tree().await;
    let reachable_commits = get_reachable_commits(commit_id, None).await.unwrap();

    // Get the minimum unique hash length calculated by the log command
    let len = libra::utils::util::get_min_unique_hash_length(&reachable_commits);

    // Test with a single commit for consistency
    let commit = reachable_commits.first().unwrap();
    let commit_str = commit.id.to_string();
    let full_hash = commit_str.clone();
    // Extract the full hash length for subsequent oversized-abbreviation boundary tests
    let full_hash_len = full_hash.len();
    // Define an abbreviation length much larger than the hash (e.g., +1000) to simulate an extreme edge case
    let oversized_abbrev = full_hash_len + 1000;

    // Helper function to run log command and get the output
    let run_log_command = |args: &[&str]| -> String {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
            .arg("log")
            .args(args)
            .output()
            .expect("Failed to execute log command");
        assert!(
            output.status.success(),
            "Log command failed with stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("Failed to parse log output")
    };

    // Helper function to extract the commit hash from log output
    let extract_commit_hash = |output: &str, oneline: bool| -> String {
        if oneline {
            // Oneline format: "hash message"
            output.split_whitespace().next().unwrap().to_string()
        } else {
            // Non-oneline format: "commit hash"
            output
                .lines()
                .find(|line| line.starts_with("commit "))
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap()
                .to_string()
        }
    };

    let oneline_abbrev_over_len = format!("--abbrev={}", full_hash_len + 1);
    let oneline_abbrev_oversized = format!("--abbrev={}", oversized_abbrev);

    let non_oneline_abbrev_over_len = format!("--abbrev={}", full_hash_len + 1);
    let non_oneline_abbrev_oversized = format!("--abbrev={}", oversized_abbrev);

    // Test cases for oneline format
    let oneline_test_cases = vec![
        // (args, expected_hash_length)
        (vec!["--oneline"], len), // Default oneline uses min unique length
        (vec!["--oneline", "--abbrev=0"], 7), // oneline with abbrev=0 uses default 7
        (vec!["--oneline", "--abbrev=5"], 5), // oneline with abbrev=5 uses 5 characters
        (vec!["--oneline", "--no-abbrev-commit"], full_hash_len), // oneline with no_abbrev_commit uses full hash
        (vec!["--oneline", &oneline_abbrev_over_len], full_hash_len),
        (vec!["--oneline", &oneline_abbrev_oversized], full_hash_len),
    ];

    // Test oneline format cases
    for (args, expected_len) in oneline_test_cases {
        let output = run_log_command(&args);
        let hash = extract_commit_hash(&output, true);
        assert_eq!(
            hash.len(),
            expected_len,
            "Failed oneline test with args: {:?}, got hash: '{}' (length: {}), expected length: {}",
            args,
            hash,
            hash.len(),
            expected_len
        );
        // Also verify it's a prefix of the full hash
        assert!(
            commit_str.starts_with(&hash),
            "Hash '{}' is not a prefix of full hash '{}'",
            hash,
            commit_str
        );
    }

    // Test cases for non-oneline format
    let non_oneline_test_cases = vec![
        // (args, expected_hash_length)
        (vec![], full_hash_len),        // Default non-oneline uses full hash
        (vec!["--abbrev-commit"], len), // non-oneline with abbrev_commit uses min unique length
        (vec!["--abbrev-commit", "--abbrev=3"], 3), // non-oneline with abbrev_commit and abbrev=3 uses 3 characters
        (vec!["--abbrev-commit", "--no-abbrev-commit"], full_hash_len), // non-oneline with both uses full hash
        (
            vec!["--abbrev-commit", &non_oneline_abbrev_over_len],
            full_hash_len,
        ),
        (
            vec!["--abbrev-commit", &non_oneline_abbrev_oversized],
            full_hash_len,
        ),
    ];

    // Test non-oneline format cases
    for (args, expected_len) in non_oneline_test_cases {
        let output = run_log_command(&args);
        let hash = extract_commit_hash(&output, false);
        assert_eq!(
            hash.len(),
            expected_len,
            "Failed non-oneline test with args: {:?}, got hash: '{}' (length: {}), expected length: {}",
            args,
            hash,
            hash.len(),
            expected_len
        );
        // Also verify it's a prefix of the full hash
        assert!(
            commit_str.starts_with(&hash),
            "Hash '{}' is not a prefix of full hash '{}'",
            hash,
            commit_str
        );
    }
}

#[tokio::test]
#[serial]
async fn test_log_graph() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    let commit_id = create_test_commit_tree().await;

    let args = LogArgs::try_parse_from(["libra", "--number", "6", "--graph"]).unwrap();
    assert!(args.graph);

    let mut graph_state = libra::command::log::GraphState::new();

    let commit_hash = ObjectHash::from_str(&commit_id).unwrap();
    let commit = load_object::<Commit>(&commit_hash).unwrap();

    let prefix = graph_state.render(&commit);
    assert!(!prefix.is_empty());
    assert!(prefix.contains('*'));
}

#[tokio::test]
#[serial]
async fn test_log_graph_simple_chain() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    test::ensure_file("file1.txt", Some("content1\n"));
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

    test::ensure_file("file2.txt", Some("content2\n"));
    add::execute(AddArgs {
        pathspec: vec![String::from("file2.txt")],
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

    let commit_hash = Head::current_commit().await.unwrap().to_string();
    let reachable_commits = get_reachable_commits(commit_hash, None).await.unwrap();

    let mut graph_state = libra::command::log::GraphState::new();

    for commit in reachable_commits.iter().take(2) {
        let prefix = graph_state.render(commit);
        assert!(prefix.starts_with("* ") || prefix.contains("* "));
    }
}

#[tokio::test]
#[serial]
async fn test_log_stat_and_graph_combined() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    test::ensure_file("combo.txt", Some("line1\nline2\n"));
    add::execute(AddArgs {
        pathspec: vec![String::from("combo.txt")],
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
        message: Some("Add combo file".to_string()),
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

    let args = LogArgs::try_parse_from(["libra", "--graph", "--stat"]).unwrap();
    assert!(args.graph);
    assert!(args.stat);

    let commit_hash = Head::current_commit().await.unwrap().to_string();
    let commit_id = ObjectHash::from_str(&commit_hash).unwrap();
    let commit = load_object::<Commit>(&commit_id).unwrap();

    let stats = libra::command::log::compute_commit_stat(&commit, Vec::new())
        .await
        .unwrap();
    assert_eq!(stats.len(), 1);

    let mut graph_state = libra::command::log::GraphState::new();
    let prefix = graph_state.render(&commit);
    assert!(!prefix.is_empty());
}

fn run_log_cmd(args: &[&str], cwd: &std::path::Path) -> (std::process::ExitStatus, String, String) {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(cwd)
        .arg("log")
        .args(args)
        .output()
        .expect("Failed to execute log command");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (output.status, stdout, stderr)
}

fn run_libra_cmd(
    args: &[&str],
    cwd: &std::path::Path,
) -> (std::process::ExitStatus, String, String) {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("Failed to execute libra command");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (output.status, stdout, stderr)
}

fn count_commit_lines(output: &str) -> usize {
    output.lines().filter(|l| l.starts_with("commit ")).count()
}

#[tokio::test]
#[serial]
async fn test_log_short_number_flag_equivalent_to_number() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    let _ = create_test_commit_tree().await;

    let (status_short, out_short, err_short) = run_log_cmd(&["-2"], temp_path.path());
    assert!(status_short.success(), "log -2 failed: {err_short}");

    let (status_long, out_long, err_long) = run_log_cmd(&["-n", "2"], temp_path.path());
    assert!(status_long.success(), "log -n 2 failed: {err_long}");

    let short_count = count_commit_lines(&out_short);
    let long_count = count_commit_lines(&out_long);

    assert_eq!(short_count, 2);
    assert_eq!(long_count, 2);
    assert_eq!(short_count, long_count);
}

#[tokio::test]
#[serial]
async fn test_log_short_number_flag_multi_digit() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    let _ = create_test_commit_tree().await;

    let (status_long, out_long, err_long) = run_log_cmd(&["-n", "10"], temp_path.path());
    assert!(status_long.success(), "log -n 10 failed: {err_long}");

    let expected_count = count_commit_lines(&out_long);

    let (status_short, out_short, err_short) = run_log_cmd(&["-10"], temp_path.path());
    assert!(status_short.success(), "log -10 failed: {err_short}");

    let short_count = count_commit_lines(&out_short);
    assert_eq!(short_count, expected_count);
}

#[tokio::test]
#[serial]
/// Ensure `log -- -2` treats `-2` as a pathspec, not as `-n 2`.
async fn test_log_double_dash_disables_short_number_rewrite() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Commit a normal file first.
    test::ensure_file("a.txt", Some("A\n"));
    add::execute(AddArgs {
        pathspec: vec![String::from("a.txt")],
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
        message: Some("Add a".to_string()),
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

    // Commit a file named "-2" to validate pathspec handling.
    test::ensure_file("-2", Some("dash\n"));
    add::execute(AddArgs {
        pathspec: vec![String::from("-2")],
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
        message: Some("Add dash".to_string()),
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

    let (status, out, err) = run_log_cmd(&["--", "-2"], temp_path.path());
    assert!(status.success(), "log -- -2 failed: {err}");

    // Only the commit touching "-2" should be listed.
    assert_eq!(count_commit_lines(&out), 1);
    assert!(out.contains("Add dash"));
}

#[tokio::test]
#[serial]
/// Ensure `log` rewrite does not trigger when `log` is a positional path for another subcommand.
async fn test_add_with_log_path_does_not_trigger_log_rewrite() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create files named "log" and "-2".
    test::ensure_file("log", Some("logfile\n"));
    test::ensure_file("-2", Some("dashfile\n"));

    let (status_add, _out_add, err_add) =
        run_libra_cmd(&["add", "log", "--", "-2"], temp_path.path());
    assert!(status_add.success(), "add failed: {err_add}");

    let (status_status, out_status, err_status) =
        run_libra_cmd(&["status", "--porcelain"], temp_path.path());
    assert!(
        status_status.success(),
        "status --porcelain failed: {err_status}"
    );

    // Both files should be staged (porcelain v1 uses "A  <path>").
    assert!(out_status.lines().any(|l| l == "A  log"));
    assert!(out_status.lines().any(|l| l == "A  -2"));
}

#[tokio::test]
#[serial]
/// Ensure `libra -- log -2` treats `log` as the subcommand and rewrites `-2` correctly.
async fn test_log_short_number_flag_with_double_dash_before_subcommand() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    let _ = create_test_commit_tree().await;

    let (status, out, err) = run_libra_cmd(&["--", "log", "-2"], temp_path.path());
    assert!(status.success(), "libra -- log -2 failed: {err}");
    assert_eq!(count_commit_lines(&out), 2);
}

#[test]
fn test_log_machine_output_is_single_line_json() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--machine", "log", "-n", "1"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let non_empty_lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        non_empty_lines.len(),
        1,
        "machine output should be exactly one non-empty line, got: {stdout}"
    );

    let parsed: serde_json::Value =
        serde_json::from_str(non_empty_lines[0]).expect("machine output should be valid JSON");
    assert_eq!(parsed["command"], "log");
    assert!(parsed["data"]["commits"].as_array().is_some());
}

#[test]
fn test_log_json_root_commit_has_empty_parents_and_added_files() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "log", "-n", "1"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    let commit = &json["data"]["commits"][0];

    // Root commit has no parents.
    let parents = commit["parents"]
        .as_array()
        .expect("parents should be an array");
    assert!(parents.is_empty(), "root commit should have no parents");

    // Root commit files should all be "added".
    let files = commit["files"]
        .as_array()
        .expect("files should be an array");
    assert!(
        !files.is_empty(),
        "root commit should have at least one file"
    );
    for file in files {
        assert_eq!(
            file["status"], "added",
            "root commit files should all be 'added', got: {}",
            file["status"]
        );
    }
}

#[test]
fn test_log_json_since_filter_restricts_results() {
    let repo = create_committed_repo_via_cli();

    // The committed repo has one commit. Querying with --since far in the future
    // should return zero commits.
    let output = run_libra_command(&["--json", "log", "--since", "2099-01-01"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    let commits = json["data"]["commits"]
        .as_array()
        .expect("commits should be an array");
    assert!(
        commits.is_empty(),
        "no commits should match a future --since date"
    );
}

#[test]
fn test_log_json_oneline_flag_does_not_alter_schema() {
    let repo = create_committed_repo_via_cli();

    let plain = run_libra_command(&["--json", "log", "-n", "1"], repo.path());
    let with_oneline = run_libra_command(&["--json", "log", "-n", "1", "--oneline"], repo.path());
    assert!(plain.status.success());
    assert!(with_oneline.status.success());

    let plain_json = parse_json_stdout(&plain);
    let oneline_json = parse_json_stdout(&with_oneline);

    // JSON schema should be identical regardless of --oneline.
    assert_eq!(
        plain_json["data"]["commits"][0]["hash"],
        oneline_json["data"]["commits"][0]["hash"]
    );
    assert_eq!(
        plain_json["data"]["commits"][0]["subject"],
        oneline_json["data"]["commits"][0]["subject"]
    );
    assert_eq!(
        plain_json["data"]["commits"][0]["author_name"],
        oneline_json["data"]["commits"][0]["author_name"]
    );
}

// ============================================================================
// --grep 参数测试
// ============================================================================

// Test grep parameter parsing
#[test]
fn test_log_args_grep() {
    let args = LogArgs::parse_from(["libra", "--grep", "fix"]);
    assert_eq!(args.grep, Some("fix".to_string()));

    let args = LogArgs::parse_from(["libra"]);
    assert_eq!(args.grep, None);
}

// Test grep combined with other arguments
#[test]
fn test_grep_with_other_args() {
    let args = LogArgs::parse_from(["libra", "--grep", "feature", "--oneline", "-n", "5"]);
    assert_eq!(args.grep, Some("feature".to_string()));
    assert!(args.oneline);
    assert_eq!(args.number, Some(5));
}

// Test case-sensitive matching
#[test]
fn test_grep_case_sensitive() {
    let args = LogArgs::parse_from(["libra", "--grep", "FIX"]);
    assert_eq!(args.grep, Some("FIX".to_string()));
}

// Test empty string grep
#[test]
fn test_grep_empty_string() {
    let args = LogArgs::parse_from(["libra", "--grep", ""]);
    assert_eq!(args.grep, Some("".to_string()));
}

// Test graph with grep combination
#[test]
fn test_graph_with_grep() {
    let args = LogArgs::parse_from(["libra", "--graph", "--grep", "fix"]);
    assert!(args.graph);
    assert_eq!(args.grep, Some("fix".to_string()));
}

// Integration test: verify actual filtering behavior
#[tokio::test]
#[serial]
async fn test_log_grep_filtering() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create first commit: fix message
    test::ensure_file("file1.txt", Some("content1\n"));
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
        message: Some("fix: bug fix".to_string()),
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

    // Create second commit: feat message
    test::ensure_file("file2.txt", Some("content2\n"));
    add::execute(AddArgs {
        pathspec: vec![String::from("file2.txt")],
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
        message: Some("feat: new feature".to_string()),
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

    // Create third commit: docs message
    test::ensure_file("file3.txt", Some("content3\n"));
    add::execute(AddArgs {
        pathspec: vec![String::from("file3.txt")],
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
        message: Some("docs: update readme".to_string()),
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

    // Test grep "fix" - should only show the fix commit
    let (status, stdout, stderr) = run_log_cmd(&["--grep", "fix"], temp_path.path());
    assert!(status.success(), "log --grep failed: {stderr}");
    assert!(stdout.contains("fix: bug fix"));
    assert!(!stdout.contains("feat: new feature"));
    assert!(!stdout.contains("docs: update readme"));

    // Test grep "feat" - should only show the feat commit
    let (status, stdout, stderr) = run_log_cmd(&["--grep", "feat"], temp_path.path());
    assert!(status.success(), "log --grep failed: {stderr}");
    assert!(stdout.contains("feat: new feature"));
    assert!(!stdout.contains("fix: bug fix"));
    assert!(!stdout.contains("docs: update readme"));

    // Test grep "nonexistent" - should show no commits
    let (status, stdout, stderr) = run_log_cmd(&["--grep", "nonexistent"], temp_path.path());
    assert!(status.success(), "log --grep failed: {stderr}");
    assert!(!stdout.contains("fix: bug fix"));
    assert!(!stdout.contains("feat: new feature"));
    assert!(!stdout.contains("docs: update readme"));
    // With no matches, stdout should be empty
    assert!(stdout.is_empty());

    // Test empty grep pattern - should show all commits
    let (status, stdout, stderr) = run_log_cmd(&["--grep", ""], temp_path.path());
    assert!(status.success(), "log --grep failed: {stderr}");
    assert!(stdout.contains("fix: bug fix"));
    assert!(stdout.contains("feat: new feature"));
    assert!(stdout.contains("docs: update readme"));

    // Test case-sensitive matching - "Fix" should not match "fix"
    let (status, stdout, stderr) = run_log_cmd(&["--grep", "Fix"], temp_path.path());
    assert!(status.success(), "log --grep failed: {stderr}");
    assert!(!stdout.contains("fix: bug fix"));
    assert!(!stdout.contains("feat: new feature"));
    assert!(!stdout.contains("docs: update readme"));
    assert!(stdout.is_empty());

    // Test case-insensitive should not work (we document case-sensitive)
    // but that's the intended behavior

    // Test grep with -n limit
    let (status, stdout, stderr) = run_log_cmd(&["--grep", "fix", "-n", "1"], temp_path.path());
    assert!(status.success(), "log --grep failed: {stderr}");
    // Should show at most 1 commit with "fix"
    let commit_count = count_commit_lines(&stdout);
    assert_eq!(commit_count, 1);
}

#[tokio::test]
#[serial]
async fn test_log_reverse_outputs_oldest_first() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    test::ensure_file("a.txt", Some("a\n"));
    add::execute(AddArgs {
        pathspec: vec!["a.txt".into()],
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
        message: Some("first".to_string()),
        no_verify: true,
        ..Default::default()
    })
    .await;

    test::ensure_file("b.txt", Some("b\n"));
    add::execute(AddArgs {
        pathspec: vec!["b.txt".into()],
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
        message: Some("second".to_string()),
        no_verify: true,
        ..Default::default()
    })
    .await;

    let (_, out, _) = run_log_cmd(&["--oneline", "--reverse"], temp_path.path());
    let first_idx = out.find("first").expect("first commit should appear");
    let second_idx = out.find("second").expect("second commit should appear");
    assert!(
        first_idx < second_idx,
        "--reverse should list oldest first: {out}"
    );
}

#[tokio::test]
#[serial]
async fn test_log_range_excludes_start_commit() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    test::ensure_file("a.txt", Some("a\n"));
    add::execute(AddArgs {
        pathspec: vec!["a.txt".into()],
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
        message: Some("base".to_string()),
        no_verify: true,
        ..Default::default()
    })
    .await;
    let base = Head::current_commit().await.unwrap();

    test::ensure_file("b.txt", Some("b\n"));
    add::execute(AddArgs {
        pathspec: vec!["b.txt".into()],
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
        message: Some("tip".to_string()),
        no_verify: true,
        ..Default::default()
    })
    .await;

    let (_, out, _) = run_log_cmd(
        &["--oneline", "--range", &format!("{}..HEAD", base)],
        temp_path.path(),
    );
    assert!(out.contains("tip"), "range should include tip: {out}");
    assert!(!out.contains("base"), "range should exclude base: {out}");
}

#[tokio::test]
#[serial]
async fn test_log_all_includes_branches() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    test::ensure_file("a.txt", Some("a\n"));
    add::execute(AddArgs {
        pathspec: vec!["a.txt".into()],
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
        message: Some("main only".to_string()),
        no_verify: true,
        ..Default::default()
    })
    .await;

    execute(BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: Some("side".to_string()),
        commit_hash: None,
        list: false,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
        unset_upstream: None,
        edit_description: None,
        show_current: false,
        rename: vec![],
        copy: vec![],
        copy_force: vec![],
        remotes: false,
        all: false,
        contains: vec![],
        no_contains: vec![],
        points_at: None,
        merged: None,
        no_merged: None,
        sort: None,
        ignore_case: false,
        column: None,
        verbose: 0,
    })
    .await;

    test::ensure_file("side.txt", Some("side\n"));
    add::execute(AddArgs {
        pathspec: vec!["side.txt".into()],
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
        message: Some("side only".to_string()),
        no_verify: true,
        ..Default::default()
    })
    .await;

    let (_, out, _) = run_log_cmd(&["--oneline", "--all"], temp_path.path());
    assert!(
        out.contains("side only"),
        "--all should include side branch: {out}"
    );
    assert!(
        out.contains("main only"),
        "--all should include main branch: {out}"
    );
}

#[tokio::test]
#[serial]
async fn test_log_follow_detects_rename() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    test::ensure_file("old.txt", Some("content\n"));
    add::execute(AddArgs {
        pathspec: vec!["old.txt".into()],
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
        message: Some("add old".to_string()),
        no_verify: true,
        ..Default::default()
    })
    .await;

    std::fs::remove_file(temp_path.path().join("old.txt")).unwrap();
    test::ensure_file("new.txt", Some("content\n"));
    add::execute(AddArgs {
        pathspec: vec!["old.txt".into(), "new.txt".into()],
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
        message: Some("rename".to_string()),
        no_verify: true,
        ..Default::default()
    })
    .await;

    let (_, out, _) = run_log_cmd(&["--oneline", "--follow", "new.txt"], temp_path.path());
    // The follow filter is best-effort; assert the command succeeds and
    // includes the rename commit at minimum.
    assert!(
        out.contains("rename"),
        "--follow should include rename commit: {out}"
    );
}

#[tokio::test]
#[serial]
async fn test_log_line_range_flag_accepted() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    test::ensure_file("a.txt", Some("line1\nline2\n"));
    add::execute(AddArgs {
        pathspec: vec!["a.txt".into()],
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
        message: Some("add a".to_string()),
        no_verify: true,
        ..Default::default()
    })
    .await;

    let (status, out, err) = run_log_cmd(&["--oneline", "-L1,2:a.txt"], temp_path.path());
    assert!(status.success(), "-L flag should be accepted: {err}");
    // Line-range tracking is best-effort; just ensure the flag parses.
    assert!(
        out.contains("add a") || out.is_empty(),
        "-L should not produce unexpected output: {out}"
    );
}

#[test]
fn log_shortstat_shows_summary_line_without_per_file_bars() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // Modify the tracked file and add a new one, then commit.
    std::fs::write(p.join("tracked.txt"), "a\nb\nc\n").unwrap();
    std::fs::write(p.join("new.txt"), "x\ny\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt", "new.txt"], p),
        "add changes",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "changes", "--no-verify"], p),
        "commit changes",
    );

    let out = run_libra_command(&["log", "--shortstat", "-1"], p);
    assert_cli_success(&out, "log --shortstat -1");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();

    // The diffstat summary line is present...
    assert!(
        s.contains("files changed") || s.contains("file changed"),
        "shortstat summary line present: {s:?}"
    );
    assert!(s.contains("insertion"), "insertions reported: {s:?}");
    // ...but the per-file bar lines (` <path> | N +++`) are NOT.
    assert!(
        !s.contains(" | "),
        "shortstat must omit per-file bars: {s:?}"
    );
}

#[test]
fn log_format_is_alias_for_pretty() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // `--format=<x>` renders identically to `--pretty=<x>`.
    let fmt = run_libra_command(&["log", "--format=%s", "-1"], p);
    assert_cli_success(&fmt, "log --format=%s");
    let pretty = run_libra_command(&["log", "--pretty=%s", "-1"], p);
    assert_cli_success(&pretty, "log --pretty=%s");
    assert_eq!(
        String::from_utf8_lossy(&fmt.stdout),
        String::from_utf8_lossy(&pretty.stdout),
        "--format must alias --pretty"
    );
    assert!(
        String::from_utf8_lossy(&fmt.stdout).contains("base"),
        "subject placeholder rendered: {}",
        String::from_utf8_lossy(&fmt.stdout)
    );

    // `--format` and `--pretty` are mutually exclusive.
    let both = run_libra_command(&["log", "--format=%s", "--pretty=%s", "-1"], p);
    assert!(!both.status.success(), "--format conflicts with --pretty");
}

#[test]
fn log_parents_and_children_annotate_commit_lines() {
    use std::fs;

    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let head = |s: &str| {
        let out = run_libra_command(&["rev-parse", s], p);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let short = |full: &str| full.chars().take(7).collect::<String>();

    let c0 = head("HEAD");
    fs::write(p.join("a.txt"), "a\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add a");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "c1",
    );
    let c1 = head("HEAD");
    fs::write(p.join("b.txt"), "b\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.txt"], p), "add b");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "c2",
    );
    let c2 = head("HEAD");

    // --parents: c1's line carries c0 (its parent); c2's carries c1.
    let parents = run_libra_command(&["log", "--oneline", "--parents"], p);
    assert_cli_success(&parents, "log --oneline --parents");
    let pj = String::from_utf8_lossy(&parents.stdout).into_owned();
    let c1_line = pj
        .lines()
        .find(|l| l.starts_with(&short(&c1)))
        .expect("c1 line");
    assert!(
        c1_line.contains(&short(&c0)),
        "c1 line must show parent c0: {c1_line:?}"
    );

    // --children: c0's line carries c1 (its child in range); c1's carries c2.
    let children = run_libra_command(&["log", "--oneline", "--children"], p);
    assert_cli_success(&children, "log --oneline --children");
    let cj = String::from_utf8_lossy(&children.stdout).into_owned();
    let c0_line = cj
        .lines()
        .find(|l| l.starts_with(&short(&c0)))
        .expect("c0 line");
    assert!(
        c0_line.contains(&short(&c1)),
        "c0 line must show child c1: {c0_line:?}"
    );
    let c1_cline = cj
        .lines()
        .find(|l| l.starts_with(&short(&c1)))
        .expect("c1 line");
    assert!(
        c1_cline.contains(&short(&c2)),
        "c1 line must show child c2: {c1_cline:?}"
    );

    // --parents and --children are mutually exclusive.
    let conflict = run_libra_command(&["log", "--parents", "--children"], p);
    assert!(
        !conflict.status.success(),
        "--parents conflicts with --children"
    );
}

#[test]
fn log_grep_ignore_case_and_invert_grep() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    for msg in ["Fix alpha", "add beta", "FIX gamma"] {
        assert_cli_success(
            &run_libra_command(&["commit", "--allow-empty", "-m", msg, "--no-verify"], p),
            "commit",
        );
    }
    let subjects = |args: &[&str]| -> String {
        let out = run_libra_command(args, p);
        assert!(out.status.success(), "log ok: {args:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    // Case-sensitive --grep matches only "Fix alpha".
    let cs = subjects(&["log", "--oneline", "--grep", "Fix"]);
    assert!(cs.contains("Fix alpha"), "cs: {cs:?}");
    assert!(!cs.contains("FIX gamma"), "cs excludes other case: {cs:?}");
    assert!(!cs.contains("add beta"), "cs excludes non-match: {cs:?}");

    // -i makes it case-insensitive: both Fix/FIX match.
    let ci = subjects(&["log", "--oneline", "--grep", "Fix", "-i"]);
    assert!(
        ci.contains("Fix alpha") && ci.contains("FIX gamma"),
        "ci: {ci:?}"
    );
    assert!(
        !ci.contains("add beta"),
        "ci still excludes non-match: {ci:?}"
    );

    // --invert-grep keeps the non-matching commits (case-sensitive).
    let inv = subjects(&["log", "--oneline", "--grep", "Fix", "--invert-grep"]);
    assert!(
        inv.contains("add beta") && inv.contains("FIX gamma"),
        "inv: {inv:?}"
    );
    assert!(
        !inv.contains("Fix alpha"),
        "inv excludes the match: {inv:?}"
    );

    // -i + --invert-grep: exclude both Fix and FIX.
    let both = subjects(&["log", "--oneline", "--grep", "Fix", "-i", "--invert-grep"]);
    assert!(both.contains("add beta"), "both keeps non-match: {both:?}");
    assert!(
        !both.contains("Fix alpha") && !both.contains("FIX gamma"),
        "both excludes all case-folded matches: {both:?}"
    );
}

#[test]
fn test_log_author_date_order_lists_all_commits() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    for i in 1..=3 {
        std::fs::write(p.join(format!("f{i}.txt")), format!("{i}\n")).unwrap();
        assert_cli_success(&run_libra_command(&["add", &format!("f{i}.txt")], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", &format!("c{i}"), "--no-verify"], p),
            "commit",
        );
    }

    // `--author-date-order` is accepted and lists every commit. For commits
    // whose author and committer dates match (the common case), the order is
    // the same as the default committer-date order.
    let ado = run_libra_command(&["log", "--author-date-order", "--oneline"], p);
    assert_cli_success(&ado, "log --author-date-order");
    let default = run_libra_command(&["log", "--oneline"], p);
    assert_cli_success(&default, "log --oneline");
    assert_eq!(
        ado.stdout, default.stdout,
        "author-date-order matches default when author == committer date"
    );
    assert!(
        String::from_utf8_lossy(&ado.stdout).contains("c3"),
        "lists the newest commit"
    );
}

#[test]
fn test_log_date_order_selects_default_and_conflicts() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    for i in 1..=3 {
        std::fs::write(p.join(format!("g{i}.txt")), format!("{i}\n")).unwrap();
        assert_cli_success(&run_libra_command(&["add", &format!("g{i}.txt")], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", &format!("d{i}"), "--no-verify"], p),
            "commit",
        );
    }

    // `--date-order` selects Libra's default committer-date order (parity no-op).
    let date_order = run_libra_command(&["log", "--date-order", "--oneline"], p);
    assert_cli_success(&date_order, "log --date-order");
    let default = run_libra_command(&["log", "--oneline"], p);
    assert_cli_success(&default, "log --oneline");
    assert_eq!(
        date_order.stdout, default.stdout,
        "--date-order matches the default committer-date order"
    );

    // `--date-order` conflicts with `--author-date-order`.
    let conflict = run_libra_command(&["log", "--date-order", "--author-date-order"], p);
    assert!(
        !conflict.status.success(),
        "--date-order and --author-date-order conflict"
    );
}

#[test]
fn log_no_expand_tabs_flag_is_accepted_noop() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("t.txt"), "x\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "t.txt"], p), "stage t.txt");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "msg\twith\ttab", "--no-verify"], p),
        "commit with tabs",
    );

    let plain = run_libra_command(&["log"], p);
    assert_cli_success(&plain, "log");
    // `--no-expand-tabs` is accepted and a no-op: Libra never expands tabs in
    // commit messages, so the output is identical.
    let out = run_libra_command(&["log", "--no-expand-tabs"], p);
    assert_cli_success(&out, "log --no-expand-tabs");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&plain.stdout),
        "log --no-expand-tabs matches plain log (no-op)"
    );
}

#[test]
fn log_no_notes_flag_is_accepted_noop() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // Attach a note; Libra's log never displays notes inline, so the flag is a
    // no-op whether or not a note exists.
    assert!(
        run_libra_command(&["notes", "add", "-m", "a note", "HEAD"], p)
            .status
            .success()
    );

    let plain = run_libra_command(&["log"], p);
    assert_cli_success(&plain, "log");
    let out = run_libra_command(&["log", "--no-notes"], p);
    assert_cli_success(&out, "log --no-notes");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&plain.stdout),
        "log --no-notes matches plain log (no-op)"
    );
    // Sanity: the note is not shown by plain log either.
    assert!(
        !String::from_utf8_lossy(&plain.stdout).contains("a note"),
        "Libra log does not display notes inline"
    );
}

#[test]
fn log_no_mailmap_flag_is_accepted_noop() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let plain = run_libra_command(&["log"], p);
    assert_cli_success(&plain, "log");
    // `--no-mailmap` is accepted and a no-op: Libra's log never applies a
    // mailmap, so the output is unchanged.
    let out = run_libra_command(&["log", "--no-mailmap"], p);
    assert_cli_success(&out, "log --no-mailmap");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&plain.stdout),
        "log --no-mailmap matches plain log (no-op)"
    );
}

#[test]
fn log_no_show_signature_flag_is_accepted_noop() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let plain = run_libra_command(&["log"], p);
    assert_cli_success(&plain, "log");
    // `--no-show-signature` is accepted and a no-op: Libra's log never displays
    // commit signatures inline, so the output is unchanged.
    let out = run_libra_command(&["log", "--no-show-signature"], p);
    assert_cli_success(&out, "log --no-show-signature");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&plain.stdout),
        "log --no-show-signature matches plain log (no-op)"
    );
}

#[test]
fn test_log_pretty_named_presets() {
    use std::fs;

    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // Add a commit with a subject and a body to exercise body display.
    fs::write(p.join("x.txt"), "x").expect("write x");
    assert_cli_success(&run_libra_command(&["add", "x.txt"], p), "add x");
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "-m",
                "feat: the subject\n\nbody text here",
                "--no-verify",
            ],
            p,
        ),
        "commit with body",
    );

    let run = |preset: &str| -> String {
        let out = run_libra_command(&["log", &format!("--pretty={preset}"), "-1"], p);
        assert_cli_success(&out, preset);
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    // short: commit + Author, NO Date / NO Commit, subject only (no body).
    let short = run("short");
    assert!(short.contains("commit "), "short header: {short}");
    assert!(short.contains("Author: "), "short Author: {short}");
    assert!(!short.contains("Date:"), "short has no Date: {short}");
    assert!(!short.contains("Commit:"), "short has no Commit: {short}");
    assert!(
        short.contains("    feat: the subject"),
        "short subject: {short}"
    );
    assert!(
        !short.contains("body text here"),
        "short omits the body: {short}"
    );

    // full: Author + Commit (no Date), subject + body.
    let full = run("full");
    assert!(full.contains("Author: "), "full Author: {full}");
    assert!(full.contains("Commit: "), "full Commit: {full}");
    assert!(!full.contains("Date:"), "full has no Date: {full}");
    assert!(
        full.contains("    body text here"),
        "full shows body: {full}"
    );

    // fuller: Author/AuthorDate/Commit/CommitDate (labels aligned), subject + body.
    let fuller = run("fuller");
    assert!(
        fuller.contains("Author:     "),
        "fuller Author pad: {fuller}"
    );
    assert!(
        fuller.contains("AuthorDate: "),
        "fuller AuthorDate: {fuller}"
    );
    assert!(
        fuller.contains("Commit:     "),
        "fuller Commit pad: {fuller}"
    );
    assert!(
        fuller.contains("CommitDate: "),
        "fuller CommitDate: {fuller}"
    );
    assert!(
        fuller.contains("    body text here"),
        "fuller body: {fuller}"
    );

    // reference: one-line `<hash> (<subject>, <date>)`, no header block.
    let reference = run("reference");
    assert!(
        reference.contains("(feat: the subject, "),
        "reference one-liner: {reference}"
    );
    assert!(
        !reference.contains("Author:"),
        "reference is compact: {reference}"
    );
    assert!(
        !reference.contains("commit "),
        "reference is compact: {reference}"
    );

    // raw: object headers (tree/author/committer) + indented message.
    let raw = run("raw");
    assert!(raw.contains("commit "), "raw header: {raw}");
    assert!(raw.contains("\ntree "), "raw tree line: {raw}");
    assert!(raw.contains("\nauthor "), "raw author line: {raw}");
    assert!(raw.contains("\ncommitter "), "raw committer line: {raw}");
    assert!(raw.contains("    feat: the subject"), "raw message: {raw}");

    // medium is Git's default format: commit + Author + Date + subject + body.
    let medium = run("medium");
    assert!(medium.contains("Author: "), "medium Author: {medium}");
    assert!(medium.contains("Date:   "), "medium Date line: {medium}");
    assert!(
        !medium.contains("Commit:"),
        "medium has no Commit line: {medium}"
    );
    assert!(
        medium.contains("    body text here"),
        "medium shows body: {medium}"
    );
}

/// Helper: stage a file and commit it, returning the new HEAD commit hash.
async fn commit_file(path: &str, content: &str, message: &str) -> String {
    test::ensure_file(path, Some(content));
    add::execute(AddArgs {
        pathspec: vec![path.into()],
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
        message: Some(message.to_string()),
        no_verify: true,
        ..Default::default()
    })
    .await;
    Head::current_commit().await.unwrap().to_string()
}

/// `log` accepts a positional revision range (`A..B`, `A...B`), a positional
/// single revision, `^EXCLUDE` plus an include, and a range followed by a
/// pathspec — matching Git's `log [<revision>...] [<path>...]` (previously these
/// only worked via the `--range` flag).
#[tokio::test]
#[serial]
async fn test_log_positional_revision_range() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    let base = commit_file("a.txt", "a\n", "base").await;
    let _mid = commit_file("a.txt", "a\nb\n", "mid").await;
    let _tip = commit_file("c.txt", "c\n", "tip").await;
    let p = temp_path.path();

    // Positional `A..HEAD` range: includes mid+tip, excludes base.
    let (st, out, err) = run_log_cmd(&["--oneline", &format!("{base}..HEAD")], p);
    assert!(st.success(), "positional range should succeed: {err}");
    assert!(
        out.contains("tip") && out.contains("mid"),
        "range includes tip+mid: {out}"
    );
    assert!(!out.contains("base"), "range excludes base: {out}");

    // Positional single revision: history from `base` back (just base here).
    let (st, out, _) = run_log_cmd(&["--oneline", &base], p);
    assert!(st.success(), "positional single rev should succeed");
    assert!(out.contains("base"), "single rev shows base: {out}");
    assert!(
        !out.contains("tip"),
        "single rev excludes later commits: {out}"
    );

    // Positional symmetric range `A...HEAD`.
    let (st, out, _) = run_log_cmd(&["--oneline", &format!("{base}...HEAD")], p);
    assert!(
        st.success() && out.contains("tip"),
        "symmetric range includes tip: {out}"
    );

    // `^EXCLUDE INCLUDE` positional form.
    let (st, out, _) = run_log_cmd(&["--oneline", &format!("^{base}"), "HEAD"], p);
    assert!(st.success(), "^exclude + include should succeed");
    assert!(
        out.contains("tip") && !out.contains("base"),
        "^base HEAD excludes base: {out}"
    );

    // Range followed by a pathspec: only commits touching a.txt in the range.
    let (st, out, _) = run_log_cmd(&["--oneline", &format!("{base}..HEAD"), "a.txt"], p);
    assert!(st.success(), "range + pathspec should succeed");
    assert!(out.contains("mid"), "a.txt changed in mid: {out}");
    assert!(!out.contains("tip"), "tip did not touch a.txt: {out}");
}

/// A bare positional argument that is BOTH a valid revision and an existing path
/// is rejected as ambiguous (matching Git's refusal to guess); `--range`
/// disambiguates.
#[tokio::test]
#[serial]
async fn test_log_positional_ambiguous_rev_and_path_errors() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    commit_file("a.txt", "a\n", "base").await;
    // A worktree file literally named "HEAD": both a revision and a path.
    test::ensure_file("HEAD", Some("not a ref\n"));

    let p = temp_path.path();
    let (st, _out, err) = run_log_cmd(&["HEAD"], p);
    assert!(!st.success(), "ambiguous name should be rejected");
    assert!(
        err.contains("ambiguous") && err.contains("--range"),
        "error should flag ambiguity and suggest --range: {err}"
    );

    // `--range HEAD` disambiguates to the revision and succeeds.
    let (st, _out, err) = run_log_cmd(&["--oneline", "--range", "HEAD"], p);
    assert!(
        st.success(),
        "--range HEAD should resolve the revision: {err}"
    );
}

/// Positional `A...B` is a true symmetric difference and `A..B` / `^A` exclude
/// the full ancestor closure of the excluded side, verified on a DIVERGENT
/// history (a regression guard for both the symmetric-range and exclusion fixes).
#[test]
#[serial]
fn test_log_positional_symmetric_and_exclusion_divergent() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    let p = repo.path();
    let run = |args: &[&str]| run_libra_command(args, p);
    run(&["config", "set", "user.name", "t"]);
    run(&["config", "set", "user.email", "t@t"]);

    std::fs::write(p.join("f.txt"), "base\n").unwrap();
    run(&["add", "f.txt"]);
    run(&["commit", "-m", "base", "--no-verify"]);
    run(&["branch", "side"]);
    std::fs::write(p.join("f.txt"), "main1\n").unwrap();
    run(&["add", "f.txt"]);
    run(&["commit", "-m", "mainA", "--no-verify"]);
    run(&["switch", "side"]);
    std::fs::write(p.join("g.txt"), "side1\n").unwrap();
    run(&["add", "g.txt"]);
    run(&["commit", "-m", "sideA", "--no-verify"]);
    run(&["switch", "main"]);

    // A...B symmetric difference: both unique tips, never the shared base.
    let out =
        String::from_utf8_lossy(&run(&["log", "main...side", "--pretty=%s"]).stdout).to_string();
    assert!(
        out.contains("mainA") && out.contains("sideA"),
        "symmetric includes both: {out}"
    );
    assert!(
        !out.contains("base"),
        "symmetric excludes shared base: {out}"
    );

    // A..B excludes everything reachable from A (including the shared base).
    let out =
        String::from_utf8_lossy(&run(&["log", "main..side", "--pretty=%s"]).stdout).to_string();
    assert!(out.contains("sideA"), "main..side includes sideA: {out}");
    assert!(
        !out.contains("mainA") && !out.contains("base"),
        "main..side excludes mainA+base: {out}"
    );

    // ^A B positional form behaves like A..B.
    let out =
        String::from_utf8_lossy(&run(&["log", "^main", "side", "--pretty=%s"]).stdout).to_string();
    assert!(
        out.contains("sideA") && !out.contains("base"),
        "^main side excludes base: {out}"
    );
}

/// A pathspec that merely contains `..` (a parent-directory path) is NOT
/// misclassified as a revision range — it falls back to a pathspec filter.
#[test]
#[serial]
fn test_log_positional_parent_dir_path_not_misclassified() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    let p = repo.path();
    let run = |args: &[&str]| run_libra_command(args, p);
    run(&["config", "set", "user.name", "t"]);
    run(&["config", "set", "user.email", "t@t"]);
    std::fs::write(p.join("f.txt"), "1\n").unwrap();
    run(&["add", "f.txt"]);
    run(&["commit", "-m", "c1", "--no-verify"]);

    // Run from a subdirectory with `../f.txt` as the pathspec: it contains `..`
    // but is a path, not a range, so it must succeed and filter by that file.
    let sub = p.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let out = run_libra_command(&["log", "../f.txt", "--pretty=%s"], &sub);
    assert!(
        out.status.success(),
        "../f.txt pathspec should not error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("c1"),
        "should list commits touching f.txt: {stdout}"
    );

    // A range-syntax token that is NEITHER a valid revision NOR an existing path
    // is a typoed revision and must error (not silently filter by a missing path).
    let out = run_libra_command(&["log", "definitely-not-a-ref..HEAD", "--pretty=%s"], p);
    assert!(!out.status.success(), "typoed revision range should error");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unknown revision or path"),
        "typo error should mention unknown revision or path: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------
// `log --trailer` / `--only-trailers` (Libra extensions, lore.md §1.9).
// ---------------------------------------------------------------------------

/// Three commits: one with a Reviewed-by trailer (via commit --trailer), one
/// with -s + --trailer combined (regression: must form ONE trailer block), one
/// with no trailers.
fn trailer_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let commit_with = |file: &str, args: &[&str]| {
        std::fs::write(p.join(file), file).unwrap();
        assert_cli_success(&run_libra_command(&["add", file], p), "add");
        let mut argv = vec!["commit", "--no-verify"];
        argv.extend_from_slice(args);
        assert_cli_success(&run_libra_command(&argv, p), "commit");
    };
    commit_with(
        "a.txt",
        &[
            "-m",
            "add a",
            "--trailer",
            "Reviewed-by: Alice <alice@example.com>",
        ],
    );
    commit_with(
        "b.txt",
        &["-m", "add b", "-s", "--trailer", "Change-Id: I12345"],
    );
    commit_with("c.txt", &["-m", "add c"]);
    repo
}

#[test]
#[serial]
fn test_log_trailer_filter_and_json() {
    let repo = trailer_repo();
    let p = repo.path();
    // Key filter: only the Reviewed-by commit.
    let out = run_libra_command(&["log", "--trailer", "reviewed-by", "--no-pager"], p);
    assert_cli_success(&out, "trailer key filter");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("add a") && !text.contains("add b") && !text.contains("add c"));
    // Key=value exact filter.
    let out = run_libra_command(&["log", "--trailer", "Change-Id=I12345", "--no-pager"], p);
    assert!(String::from_utf8_lossy(&out.stdout).contains("add b"));
    let out = run_libra_command(&["log", "--trailer", "Change-Id=WRONG", "--no-pager"], p);
    assert!(!String::from_utf8_lossy(&out.stdout).contains("add b"));
    // -s + --trailer roundtrip: BOTH trailers live in one block, so filtering
    // by the custom key finds the commit even though -s appended afterward.
    let out = run_libra_command(&["log", "--trailer", "signed-off-by", "--no-pager"], p);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("add b"),
        "-s + --trailer form one Git-parseable block"
    );
    // JSON: additive trailers field, empty for trailer-less commits.
    let out = run_libra_command(&["--json", "log"], p);
    assert_cli_success(&out, "json log");
    let json = parse_json_stdout(&out);
    let commits = json["data"]["commits"].as_array().expect("commits");
    let by_subject = |subj: &str| {
        commits
            .iter()
            .find(|c| c["subject"].as_str() == Some(subj))
            .unwrap_or_else(|| panic!("missing {subj}"))
    };
    let a = by_subject("add a");
    assert_eq!(a["trailers"][0]["key"].as_str(), Some("Reviewed-by"));
    assert_eq!(
        a["trailers"][0]["value"].as_str(),
        Some("Alice <alice@example.com>")
    );
    let b = by_subject("add b");
    let keys: Vec<&str> = b["trailers"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["key"].as_str())
        .collect();
    assert!(
        keys.contains(&"Change-Id") && keys.contains(&"Signed-off-by"),
        "one block carries both: {keys:?}"
    );
    let c = by_subject("add c");
    assert!(
        c["trailers"].as_array().is_some_and(|a| a.is_empty()),
        "trailer-less commit has an empty array"
    );
    // Filtered JSON agrees with the human path.
    let out = run_libra_command(&["--json", "log", "--trailer", "reviewed-by"], p);
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["commits"].as_array().unwrap().len(), 1);
}

#[test]
#[serial]
fn test_log_only_trailers_display_and_errors() {
    let repo = trailer_repo();
    let p = repo.path();
    let out = run_libra_command(&["log", "--only-trailers", "--no-pager"], p);
    assert_cli_success(&out, "--only-trailers");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("Reviewed-by: Alice <alice@example.com>"),
        "trailer lines shown: {text}"
    );
    assert!(
        !text.contains("add a\n") || !text.contains("    add a"),
        "message bodies replaced by trailer blocks"
    );
    // Key-filtered display via --trailer.
    let out = run_libra_command(
        &[
            "log",
            "--only-trailers",
            "--trailer",
            "change-id",
            "--no-pager",
        ],
        p,
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Change-Id: I12345"));
    assert!(
        !text.contains("Signed-off-by"),
        "display filtered to the selected key: {text}"
    );
    // clap exclusions + empty key usage error.
    let out = run_libra_command(&["log", "--only-trailers", "--oneline"], p);
    assert_eq!(out.status.code(), Some(129), "conflicts with --oneline");
    let out = run_libra_command(&["log", "--trailer", "=x", "--no-pager"], p);
    assert_eq!(out.status.code(), Some(129), "empty key is a usage error");
}
