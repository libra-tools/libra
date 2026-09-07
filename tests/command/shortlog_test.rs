//! Tests for the `git shortlog` command.
//!
//! This module contains integration tests for the `shortlog` command, verifying:
//! - Basic author aggregation
//! - Output sorting (`-n`)
//! - Output format (`-s`, `-e`)
//! - Date filtering (`--since`, `--until`)
//! - Grouping logic (merging authors with same name but different emails when `-e` is absent)
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::fs;

use super::*;

#[test]
fn test_shortlog_cli_outside_repository_returns_fatal_128() {
    let temp = tempdir().unwrap();
    let output = run_libra_command(&["shortlog"], temp.path());
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal: not a libra repository"),
        "unexpected stderr: {stderr}"
    );
}

use clap::Parser;
use git_internal::{
    hash::ObjectHash,
    internal::object::{
        commit::Commit,
        signature::{Signature, SignatureType},
    },
};
use libra::internal::{db::get_db_conn_instance, log::date_parser::parse_date, model::reference};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serial_test::serial;

#[tokio::test]
#[serial(cwd)]
async fn test_shortlog_corrupt_head_reference_returns_repo_corrupt() {
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

    let output = run_libra_command(&["shortlog"], repo.path());
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
fn test_shortlog_json_output_has_author_summary() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["shortlog", "--json"], repo.path());
    assert_cli_success(&output, "shortlog --json should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "shortlog");
    assert_eq!(json["data"]["total_commits"], 1);
    assert_eq!(json["data"]["authors"][0]["name"], "Test User");
    assert_eq!(json["data"]["authors"][0]["count"], 1);
}

#[test]
fn test_shortlog_format_renders_custom_per_commit_line() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("second.txt"), "second\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "second.txt"], p), "add second");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "second commit", "--no-verify"], p),
        "commit second",
    );

    // `--format` replaces the subject line with the rendered template, indented
    // by the standard 6 spaces, while keeping the author header.
    let output = run_libra_command(&["shortlog", "--format=* %s (%an)"], p);
    assert_cli_success(&output, "shortlog --format");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("      * second commit (Test User)"),
        "rendered template line present: {stdout}"
    );
    assert!(
        stdout.contains("Test User"),
        "author header present: {stdout}"
    );
    // The bare subject must not leak through (it is wrapped in the template).
    assert!(
        !stdout.lines().any(|line| line.trim() == "second commit"),
        "bare subject must not appear under --format: {stdout}"
    );

    // `%h` renders an abbreviated hex hash (7+ chars), indented.
    let hashes = run_libra_command(&["shortlog", "--format=%h"], p);
    assert_cli_success(&hashes, "shortlog --format=%h");
    let htext = String::from_utf8_lossy(&hashes.stdout);
    assert!(
        htext.lines().any(|line| {
            line.starts_with("      ") && {
                let h = line.trim();
                h.len() >= 7 && h.chars().all(|c| c.is_ascii_hexdigit())
            }
        }),
        "an abbreviated hash line is present: {htext}"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_shortlog_revision_argument_limits_history() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    fs::write(repo.path().join("second.txt"), "second\n").unwrap();
    let add_output = run_libra_command(&["add", "second.txt"], repo.path());
    assert_cli_success(&add_output, "failed to add second file");

    let commit_output = run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path());
    assert_cli_success(&commit_output, "failed to create second commit");

    let head = Head::current_commit().await.unwrap();
    let commits = get_reachable_commits(head.to_string(), None).await.unwrap();
    let base_commit = commits
        .iter()
        .find(|commit| commit.message.contains("base"))
        .expect("expected base commit")
        .id
        .to_string();

    let args = ShortlogArgs::parse_from(["shortlog", base_commit.as_str()]);
    let mut buf = Vec::new();
    shortlog::execute_to(args, &mut buf).await.unwrap();

    let stdout = String::from_utf8(buf).unwrap();
    assert!(
        stdout.contains("base"),
        "expected base commit in shortlog output"
    );
    assert!(
        !stdout.contains("second"),
        "revision-limited shortlog should not include newer commits: {stdout}"
    );
}

fn create_signature(signature_type: SignatureType, name: &str) -> Signature {
    Signature::from_data(
        format!(
            "{} {} <{}@oa.org> {} +0800",
            match signature_type {
                SignatureType::Author => "author",
                SignatureType::Committer => "committer",
                _ => panic!("Unsupported signature type"),
            },
            name,
            name.to_lowercase(),
            chrono::Utc::now().timestamp()
        )
        .to_string()
        .into_bytes(),
    )
    .unwrap()
}

/// create a test commit tree structure as graph and create branch (master) head to commit 6
/// return a commit hash of commit 6
///             3(SHY) --  6(SHY)
///            /          /
///    1(LEAVE)  --  4(SHY)  --  5(SHY) -- 7(GUXUE) -- 10(MMONK) -- 14(SunZo)
///            \     /                                 /            /  
///             2(LEAVE)  --  8(LENGSA)  --  9(SunZo)              /
///              \                                                /
///               11(LEAVE) -- 12(LEAVE) -- 13(SHY) ---- ---- ---
/// The time of commit and the commit number should be in the same order.
async fn create_test_commit_tree() -> String {
    let mut commit_1 = Commit::new(
        create_signature(SignatureType::Author, "LEAVE"),
        create_signature(SignatureType::Committer, "LEAVE"),
        ObjectHash::new(&[1; 20]),
        vec![],
        &format_commit_msg("Commit_1", None),
    );
    commit_1.author.timestamp = parse_date("2026-01-01").unwrap() as usize;
    commit_1.committer.timestamp = commit_1.author.timestamp;
    save_object(&commit_1, &commit_1.id).unwrap();

    let mut commit_2 = Commit::new(
        create_signature(SignatureType::Author, "LEAVE"),
        create_signature(SignatureType::Committer, "LEAVE"),
        ObjectHash::new(&[2; 20]),
        vec![commit_1.id],
        &format_commit_msg("Commit_2", None),
    );
    commit_2.author.timestamp = parse_date("2026-01-02").unwrap() as usize;
    commit_2.committer.timestamp = commit_2.author.timestamp;
    save_object(&commit_2, &commit_2.id).unwrap();

    let mut commit_3 = Commit::new(
        create_signature(SignatureType::Author, "SHY"),
        create_signature(SignatureType::Committer, "SHY"),
        ObjectHash::new(&[3; 20]),
        vec![commit_1.id],
        &format_commit_msg("Commit_3", None),
    );
    commit_3.author.timestamp = parse_date("2026-01-03").unwrap() as usize;
    commit_3.committer.timestamp = commit_3.author.timestamp;
    save_object(&commit_3, &commit_3.id).unwrap();

    let mut commit_4 = Commit::new(
        create_signature(SignatureType::Author, "LEAVE"),
        create_signature(SignatureType::Committer, "LEAVE"),
        ObjectHash::new(&[4; 20]),
        vec![commit_1.id, commit_2.id],
        &format_commit_msg("Commit_4", None),
    );
    commit_4.author.timestamp = parse_date("2026-01-04").unwrap() as usize;
    commit_4.committer.timestamp = commit_4.author.timestamp;
    save_object(&commit_4, &commit_4.id).unwrap();

    let mut commit_5 = Commit::new(
        create_signature(SignatureType::Author, "SHY"),
        create_signature(SignatureType::Committer, "SHY"),
        ObjectHash::new(&[5; 20]),
        vec![commit_4.id],
        &format_commit_msg("Commit_5", None),
    );
    commit_5.author.timestamp = parse_date("2026-01-05").unwrap() as usize;
    commit_5.committer.timestamp = commit_5.author.timestamp;
    save_object(&commit_5, &commit_5.id).unwrap();

    let mut commit_6 = Commit::new(
        create_signature(SignatureType::Author, "SHY"),
        create_signature(SignatureType::Committer, "SHY"),
        ObjectHash::new(&[6; 20]),
        vec![commit_3.id, commit_4.id],
        &format_commit_msg("Commit_6", None),
    );
    commit_6.author.timestamp = parse_date("2026-01-06").unwrap() as usize;
    commit_6.committer.timestamp = commit_6.author.timestamp;
    save_object(&commit_6, &commit_6.id).unwrap();

    let mut commit_7 = Commit::new(
        create_signature(SignatureType::Author, "GUXUE"),
        create_signature(SignatureType::Committer, "GUXUE"),
        ObjectHash::new(&[7; 20]),
        vec![commit_5.id],
        &format_commit_msg("Commit_7", None),
    );
    commit_7.author.timestamp = parse_date("2026-01-07").unwrap() as usize;
    commit_7.committer.timestamp = commit_7.author.timestamp;
    save_object(&commit_7, &commit_7.id).unwrap();

    let mut commit_8 = Commit::new(
        create_signature(SignatureType::Author, "LENGSA"),
        create_signature(SignatureType::Committer, "LENGSA"),
        ObjectHash::new(&[8; 20]),
        vec![commit_2.id],
        &format_commit_msg("Commit_8", None),
    );
    commit_8.author.timestamp = parse_date("2026-01-08").unwrap() as usize;
    commit_8.committer.timestamp = commit_8.author.timestamp;
    save_object(&commit_8, &commit_8.id).unwrap();

    let mut commit_9 = Commit::new(
        create_signature(SignatureType::Author, "SunZo"),
        create_signature(SignatureType::Committer, "SunZo"),
        ObjectHash::new(&[9; 20]),
        vec![commit_8.id],
        &format_commit_msg("Commit_9", None),
    );
    commit_9.author.timestamp = parse_date("2026-01-09").unwrap() as usize;
    commit_9.committer.timestamp = commit_9.author.timestamp;
    save_object(&commit_9, &commit_9.id).unwrap();

    let mut commit_10 = Commit::new(
        create_signature(SignatureType::Author, "MMONK"),
        create_signature(SignatureType::Committer, "MMONK"),
        ObjectHash::new(&[10; 20]),
        vec![commit_7.id, commit_9.id],
        &format_commit_msg("Commit_10", None),
    );
    commit_10.author.timestamp = parse_date("2026-01-10").unwrap() as usize;
    commit_10.committer.timestamp = commit_10.author.timestamp;
    save_object(&commit_10, &commit_10.id).unwrap();

    let mut commit_11 = Commit::new(
        create_signature(SignatureType::Author, "LEAVE"),
        create_signature(SignatureType::Committer, "LEAVE"),
        ObjectHash::new(&[11; 20]),
        vec![commit_2.id],
        &format_commit_msg("Commit_11", None),
    );
    commit_11.author.timestamp = parse_date("2026-01-11").unwrap() as usize;
    commit_11.committer.timestamp = commit_11.author.timestamp;
    save_object(&commit_11, &commit_11.id).unwrap();

    let mut commit_12 = Commit::new(
        create_signature(SignatureType::Author, "LEAVE"),
        create_signature(SignatureType::Committer, "LEAVE"),
        ObjectHash::new(&[12; 20]),
        vec![commit_11.id],
        &format_commit_msg("Commit_12", None),
    );
    commit_12.author.timestamp = parse_date("2026-01-12").unwrap() as usize;
    commit_12.committer.timestamp = commit_12.author.timestamp;
    save_object(&commit_12, &commit_12.id).unwrap();

    let mut commit_13 = Commit::new(
        create_signature(SignatureType::Author, "SHY"),
        create_signature(SignatureType::Committer, "SHY"),
        ObjectHash::new(&[13; 20]),
        vec![commit_12.id],
        &format_commit_msg("Commit_13", None),
    );
    commit_13.author.timestamp = parse_date("2026-01-13").unwrap() as usize;
    commit_13.committer.timestamp = commit_13.author.timestamp;
    save_object(&commit_13, &commit_13.id).unwrap();

    let mut commit_14 = Commit::new(
        create_signature(SignatureType::Author, "SunZo"),
        create_signature(SignatureType::Committer, "SunZo"),
        ObjectHash::new(&[14; 20]),
        vec![commit_10.id, commit_13.id],
        &format_commit_msg("Commit_14", None),
    );
    commit_14.author.timestamp = parse_date("2026-01-14").unwrap() as usize;
    commit_14.committer.timestamp = commit_14.author.timestamp;
    save_object(&commit_14, &commit_14.id).unwrap();

    // set current branch head to commit 14
    let head = Head::current().await;
    let branch_name = match head {
        Head::Branch(name) => name,
        _ => panic!("should be branch"),
    };

    Branch::update_branch(&branch_name, &commit_14.id.to_string(), None)
        .await
        .unwrap();

    commit_14.id.to_string()
}

#[tokio::test]
#[serial(cwd)]
async fn test_shortlog_basic() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    let _ = create_test_commit_tree().await;

    // test shortlog command without options
    let args = ShortlogArgs::try_parse_from(["libra"]).unwrap();

    let mut buf = Vec::new();
    shortlog::execute_to(args, &mut buf).await.unwrap();
    let output = String::from_utf8(buf).unwrap();

    // expected output
    let expected = r#"   1  GUXUE
      Commit_7
   5  LEAVE
      Commit_12
      Commit_11
      Commit_4
      Commit_2
      Commit_1
   1  LENGSA
      Commit_8
   1  MMONK
      Commit_10
   2  SHY
      Commit_13
      Commit_5
   2  SunZo
      Commit_14
      Commit_9
"#;

    let out_lines: Vec<_> = output.lines().collect();
    let exp_lines: Vec<_> = expected.lines().collect();
    assert_eq!(out_lines, exp_lines);
}

#[tokio::test]
#[serial(cwd)]
async fn test_shortlog_numbered() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    let _ = create_test_commit_tree().await;

    // test shortlog command with -n option
    let args = ShortlogArgs::try_parse_from(["libra", "-n"]).unwrap();
    let mut buf = Vec::new();
    shortlog::execute_to(args, &mut buf).await.unwrap();
    let output = String::from_utf8(buf).unwrap();

    let expected = r#"   5  LEAVE
      Commit_12
      Commit_11
      Commit_4
      Commit_2
      Commit_1
   2  SHY
      Commit_13
      Commit_5
   2  SunZo
      Commit_14
      Commit_9
   1  GUXUE
      Commit_7
   1  LENGSA
      Commit_8
   1  MMONK
      Commit_10
"#;

    let out_lines: Vec<_> = output.lines().collect();
    let exp_lines: Vec<_> = expected.lines().collect();
    assert_eq!(out_lines, exp_lines);
}

#[tokio::test]
#[serial(cwd)]
async fn test_shortlog_summary() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    let _ = create_test_commit_tree().await;

    // test shortlog command with -s option
    let args = ShortlogArgs::try_parse_from(["libra", "-s"]).unwrap();
    let mut buf = Vec::new();
    shortlog::execute_to(args, &mut buf).await.unwrap();
    let output = String::from_utf8(buf).unwrap();

    let expected = r#"   1  GUXUE
   5  LEAVE
   1  LENGSA
   1  MMONK
   2  SHY
   2  SunZo
"#;

    let out_lines: Vec<_> = output.lines().collect();
    let exp_lines: Vec<_> = expected.lines().collect();
    assert_eq!(out_lines, exp_lines);
}

#[tokio::test]
#[serial(cwd)]
async fn test_shortlog_email() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    let _ = create_test_commit_tree().await;

    // test shortlog command with -e option
    let args = ShortlogArgs::try_parse_from(["libra", "-e"]).unwrap();
    let mut buf = Vec::new();
    shortlog::execute_to(args, &mut buf).await.unwrap();
    let output = String::from_utf8(buf).unwrap();

    let expected = r#"   1  GUXUE <guxue@oa.org>
      Commit_7
   5  LEAVE <leave@oa.org>
      Commit_12
      Commit_11
      Commit_4
      Commit_2
      Commit_1
   1  LENGSA <lengsa@oa.org>
      Commit_8
   1  MMONK <mmonk@oa.org>
      Commit_10
   2  SHY <shy@oa.org>
      Commit_13
      Commit_5
   2  SunZo <sunzo@oa.org>
      Commit_14
      Commit_9
"#;

    let out_lines: Vec<_> = output.lines().collect();
    let exp_lines: Vec<_> = expected.lines().collect();
    assert_eq!(out_lines, exp_lines);
}

#[tokio::test]
#[serial(cwd)]
async fn test_shortlog_combined_flags() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    let _ = create_test_commit_tree().await;

    // test shortlog command with -n -s options
    let args = ShortlogArgs::try_parse_from(["libra", "-n", "-s"]).unwrap();
    let mut buf = Vec::new();
    shortlog::execute_to(args, &mut buf).await.unwrap();
    let output = String::from_utf8(buf).unwrap();

    let expected = r#"   5  LEAVE
   2  SHY
   2  SunZo
   1  GUXUE
   1  LENGSA
   1  MMONK
"#;

    let out_lines: Vec<_> = output.lines().collect();
    let exp_lines: Vec<_> = expected.lines().collect();
    assert_eq!(out_lines, exp_lines);

    // test shortlog command with -n -e options
    let args = ShortlogArgs::try_parse_from(["libra", "-n", "-e"]).unwrap();
    let mut buf = Vec::new();
    shortlog::execute_to(args, &mut buf).await.unwrap();
    let output = String::from_utf8(buf).unwrap();

    let expected = r#"   5  LEAVE <leave@oa.org>
      Commit_12
      Commit_11
      Commit_4
      Commit_2
      Commit_1
   2  SHY <shy@oa.org>
      Commit_13
      Commit_5
   2  SunZo <sunzo@oa.org>
      Commit_14
      Commit_9
   1  GUXUE <guxue@oa.org>
      Commit_7
   1  LENGSA <lengsa@oa.org>
      Commit_8
   1  MMONK <mmonk@oa.org>
      Commit_10
"#;

    let out_lines: Vec<_> = output.lines().collect();
    let exp_lines: Vec<_> = expected.lines().collect();
    assert_eq!(out_lines, exp_lines);

    // test shortlog command with -s -e options
    let args = ShortlogArgs::try_parse_from(["libra", "-s", "-e"]).unwrap();
    let mut buf = Vec::new();
    shortlog::execute_to(args, &mut buf).await.unwrap();
    let output = String::from_utf8(buf).unwrap();

    let expected = r#"   1  GUXUE <guxue@oa.org>
   5  LEAVE <leave@oa.org>
   1  LENGSA <lengsa@oa.org>
   1  MMONK <mmonk@oa.org>
   2  SHY <shy@oa.org>
   2  SunZo <sunzo@oa.org>
"#;

    let out_lines: Vec<_> = output.lines().collect();
    let exp_lines: Vec<_> = expected.lines().collect();
    assert_eq!(out_lines, exp_lines);
}

#[tokio::test]
#[serial(cwd)]
async fn test_shortlog_date_filter() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    let _ = create_test_commit_tree().await;

    // test shortlog command with --since option
    let args = ShortlogArgs::try_parse_from(["libra", "--since", "2026-01-10"]).unwrap();
    let mut buf = Vec::new();
    shortlog::execute_to(args, &mut buf).await.unwrap();
    let output = String::from_utf8(buf).unwrap();

    let expected = r#"   2  LEAVE
      Commit_12
      Commit_11
   1  MMONK
      Commit_10
   1  SHY
      Commit_13
   1  SunZo
      Commit_14
"#;
    let out_lines: Vec<_> = output.lines().collect();
    let exp_lines: Vec<_> = expected.lines().collect();
    assert_eq!(out_lines, exp_lines);

    // test shortlog command with --until option
    let args = ShortlogArgs::try_parse_from(["libra", "--until", "2026-01-13"]).unwrap();
    let mut buf = Vec::new();
    shortlog::execute_to(args, &mut buf).await.unwrap();
    let output = String::from_utf8(buf).unwrap();

    let expected = r#"   1  GUXUE
      Commit_7
   5  LEAVE
      Commit_12
      Commit_11
      Commit_4
      Commit_2
      Commit_1
   1  LENGSA
      Commit_8
   1  MMONK
      Commit_10
   2  SHY
      Commit_13
      Commit_5
   1  SunZo
      Commit_9
"#;

    let out_lines: Vec<_> = output.lines().collect();
    let exp_lines: Vec<_> = expected.lines().collect();
    assert_eq!(out_lines, exp_lines);

    // test shortlog command with comprehensive options
    let args = ShortlogArgs::try_parse_from([
        "libra",
        "-n",
        "-e",
        "--since",
        "2026-01-02",
        "--until",
        "2026-01-13",
    ])
    .unwrap();
    let mut buf = Vec::new();
    shortlog::execute_to(args, &mut buf).await.unwrap();
    let output = String::from_utf8(buf).unwrap();

    let expected = r#"   4  LEAVE <leave@oa.org>
      Commit_12
      Commit_11
      Commit_4
      Commit_2
   2  SHY <shy@oa.org>
      Commit_13
      Commit_5
   1  GUXUE <guxue@oa.org>
      Commit_7
   1  LENGSA <lengsa@oa.org>
      Commit_8
   1  MMONK <mmonk@oa.org>
      Commit_10
   1  SunZo <sunzo@oa.org>
      Commit_9
"#;

    let out_lines: Vec<_> = output.lines().collect();
    let exp_lines: Vec<_> = expected.lines().collect();
    assert_eq!(out_lines, exp_lines);
}

#[tokio::test]
#[serial(cwd)]
async fn test_shortlog_committer_date_filter() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create a commit with different author and committer dates
    let mut commit = Commit::new(
        create_signature(SignatureType::Author, "TEST"),
        create_signature(SignatureType::Committer, "TEST"),
        ObjectHash::new(&[1; 20]),
        vec![],
        &format_commit_msg("Test Commit", None),
    );
    // Author date: 2026-01-01
    commit.author.timestamp = parse_date("2026-01-01").unwrap() as usize;
    // Committer date: 2026-02-01
    commit.committer.timestamp = parse_date("2026-02-01").unwrap() as usize;
    save_object(&commit, &commit.id).unwrap();

    let head = Head::current().await;
    let branch_name = match head {
        Head::Branch(name) => name,
        _ => panic!("should be branch"),
    };
    Branch::update_branch(&branch_name, &commit.id.to_string(), None)
        .await
        .unwrap();

    // Filter since 2026-01-15
    // Should exclude if using author date (Jan 1 < Jan 15)
    // Should include if using committer date (Feb 1 > Jan 15)
    let args = ShortlogArgs::try_parse_from(["libra", "--since", "2026-01-15"]).unwrap();

    let mut buf = Vec::new();
    shortlog::execute_to(args, &mut buf).await.unwrap();
    let output = String::from_utf8(buf).unwrap();

    // Expect the commit to be present
    assert!(output.contains("TEST"));
    assert!(output.contains("Test Commit"));
}

#[tokio::test]
#[serial(cwd)]
async fn test_shortlog_no_merges_excludes_merge_commits() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    let _ = create_test_commit_tree().await;

    let full_args = ShortlogArgs::try_parse_from(["libra"]).unwrap();
    let mut full_buf = Vec::new();
    shortlog::execute_to(full_args, &mut full_buf)
        .await
        .unwrap();
    let full = String::from_utf8(full_buf).unwrap();

    let nm_args = ShortlogArgs::try_parse_from(["libra", "--no-merges"]).unwrap();
    let mut nm_buf = Vec::new();
    shortlog::execute_to(nm_args, &mut nm_buf).await.unwrap();
    let nm = String::from_utf8(nm_buf).unwrap();

    // Commit_4 (parents 1,2) and Commit_14 are merge commits; --no-merges drops
    // them while keeping the single-parent Commit_7.
    assert!(
        full.contains("Commit_4"),
        "full summary should include merge Commit_4"
    );
    assert!(
        full.contains("Commit_14"),
        "full summary should include merge Commit_14"
    );
    assert!(
        !nm.contains("Commit_4"),
        "--no-merges must drop merge Commit_4:\n{nm}"
    );
    assert!(
        !nm.contains("Commit_14"),
        "--no-merges must drop merge Commit_14:\n{nm}"
    );
    assert!(
        nm.contains("Commit_7"),
        "--no-merges keeps non-merge Commit_7:\n{nm}"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_shortlog_committer_groups_by_committer_identity() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    let mut solo = Commit::new(
        create_signature(SignatureType::Author, "ALICE"),
        create_signature(SignatureType::Committer, "BOB"),
        ObjectHash::new(&[42; 20]),
        vec![],
        &format_commit_msg("Solo", None),
    );
    solo.committer.timestamp = solo.author.timestamp;
    save_object(&solo, &solo.id).unwrap();
    let branch_name = match Head::current().await {
        Head::Branch(name) => name,
        _ => panic!("should be branch"),
    };
    Branch::update_branch(&branch_name, &solo.id.to_string(), None)
        .await
        .unwrap();

    let author_args = ShortlogArgs::try_parse_from(["libra"]).unwrap();
    let mut author_buf = Vec::new();
    shortlog::execute_to(author_args, &mut author_buf)
        .await
        .unwrap();
    let author_out = String::from_utf8(author_buf).unwrap();
    assert!(
        author_out.contains("ALICE"),
        "default groups by author:\n{author_out}"
    );
    assert!(
        !author_out.contains("BOB"),
        "default must not show committer:\n{author_out}"
    );

    let committer_args = ShortlogArgs::try_parse_from(["libra", "-c"]).unwrap();
    let mut committer_buf = Vec::new();
    shortlog::execute_to(committer_args, &mut committer_buf)
        .await
        .unwrap();
    let committer_out = String::from_utf8(committer_buf).unwrap();
    assert!(
        committer_out.contains("BOB"),
        "-c groups by committer:\n{committer_out}"
    );
    assert!(
        !committer_out.contains("ALICE"),
        "-c must not show author:\n{committer_out}"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_shortlog_top_and_min_count_limit_output() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    let _ = create_test_commit_tree().await;

    // --numbered sorts by descending count; --top 1 keeps only the busiest
    // identity, so the output has a single author line.
    let top_args = ShortlogArgs::try_parse_from(["libra", "-n", "-s", "--top", "1"]).unwrap();
    let mut top_buf = Vec::new();
    shortlog::execute_to(top_args, &mut top_buf).await.unwrap();
    let top = String::from_utf8(top_buf).unwrap();
    let author_lines = top.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(author_lines, 1, "--top 1 keeps one author line:\n{top}");

    // --min-count higher than any single identity's commit count yields no rows.
    let min_args = ShortlogArgs::try_parse_from(["libra", "-s", "--min-count", "100000"]).unwrap();
    let mut min_buf = Vec::new();
    shortlog::execute_to(min_args, &mut min_buf).await.unwrap();
    let min = String::from_utf8(min_buf).unwrap();
    assert!(
        min.trim().is_empty(),
        "--min-count above all counts drops every author:\n{min}"
    );
}

#[test]
fn group_trailer_groups_by_trailer_value() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    for file in ["g1.txt", "g2.txt"] {
        std::fs::write(p.join(file), "x\n").unwrap();
        run_libra_command(&["add", file], p);
        run_libra_command(
            &[
                "commit",
                "-m",
                "feat",
                "--trailer",
                "Co-authored-by: Alice <alice@x.io>",
                "--no-verify",
            ],
            p,
        );
    }

    // `--group=trailer:<key>` groups by each Co-authored-by value, ignoring
    // author/committer; Alice appears with both commits.
    let out = run_libra_command(&["shortlog", "--group=trailer:Co-authored-by", "-s"], p);
    assert_cli_success(&out, "shortlog --group=trailer");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.lines().any(|l| l.contains("Alice") && l.contains('2')),
        "Alice should be grouped with 2 commits: {s:?}"
    );

    // `--group=committer` is accepted as the canonical spelling of `-c`.
    assert_cli_success(
        &run_libra_command(&["shortlog", "--group=committer", "-s"], p),
        "shortlog --group=committer",
    );

    // An unknown `--group` type is a usage error.
    let bad = run_libra_command(&["shortlog", "--group=bogus"], p);
    assert!(
        !bad.status.success(),
        "invalid --group should fail: {}",
        String::from_utf8_lossy(&bad.stderr)
    );
}

#[test]
fn shortlog_wrap_wraps_long_subjects() {
    use std::fs;

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("wraptest.txt"), "x\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "wraptest.txt"], p), "add");
    let long = "alpha bravo charlie delta echo foxtrot golf hotel india juliet";
    assert_cli_success(
        &run_libra_command(&["commit", "-m", long, "--no-verify"], p),
        "commit long subject",
    );

    // -w30: the long subject wraps, so its first and last words land on
    // different physical lines and every line fits within 30 columns.
    let wrapped = run_libra_command(&["shortlog", "-w30"], p);
    assert_cli_success(&wrapped, "shortlog -w30");
    let w = String::from_utf8_lossy(&wrapped.stdout);
    let alpha_line = w.lines().position(|l| l.contains("alpha"));
    let juliet_line = w.lines().position(|l| l.contains("juliet"));
    assert!(
        alpha_line.is_some() && juliet_line.is_some() && alpha_line != juliet_line,
        "long subject must wrap onto multiple lines: {w:?}"
    );
    for line in w.lines() {
        assert!(
            line.chars().count() <= 30,
            "each wrapped line must fit width 30: {line:?}"
        );
    }
    // A continuation line is indented by 9 spaces (indent2 default).
    assert!(
        w.lines().any(|l| l.starts_with("         ")
            && l.trim_start()
                .starts_with(|c: char| c.is_ascii_alphabetic())),
        "continuation lines must use the 9-space indent: {w:?}"
    );

    // Without -w the whole subject stays on one line (first and last word
    // together).
    let plain = run_libra_command(&["shortlog"], p);
    assert_cli_success(&plain, "shortlog");
    let s = String::from_utf8_lossy(&plain.stdout);
    assert!(
        s.lines()
            .any(|l| l.contains("alpha") && l.contains("juliet")),
        "without -w the subject stays on one line: {s:?}"
    );

    // More than three comma-separated -w components is a usage error.
    let bad = run_libra_command(&["shortlog", "-w30,2,4,8"], p);
    assert!(
        !bad.status.success(),
        "a -w spec with more than three components must be rejected"
    );
}

#[test]
fn test_shortlog_merges_and_no_merges() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // Diverge main and feat on different files, then merge (a real merge commit).
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feat"], p),
        "switch feat",
    );
    std::fs::write(p.join("feat.txt"), "f\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "feat.txt"], p), "add feat");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "feat-c1", "--no-verify"], p),
        "commit feat-c1",
    );
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    std::fs::write(p.join("main.txt"), "m\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "main.txt"], p), "add main");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main-c2", "--no-verify"], p),
        "commit main-c2",
    );
    assert_cli_success(&run_libra_command(&["merge", "feat"], p), "merge feat");

    let body = |args: &[&str]| -> String {
        let out = run_libra_command(args, p);
        assert!(out.status.success(), "shortlog ok: {args:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    // --merges keeps only the merge commit.
    let m = body(&["shortlog", "--merges"]);
    assert!(
        m.contains("Merge feat into main"),
        "--merges shows the merge: {m:?}"
    );
    assert!(
        !m.contains("main-c2"),
        "--merges excludes a non-merge: {m:?}"
    );

    // --no-merges drops the merge commit.
    let nm = body(&["shortlog", "--no-merges"]);
    assert!(
        !nm.contains("Merge feat into main"),
        "--no-merges drops the merge: {nm:?}"
    );
    assert!(
        nm.contains("main-c2"),
        "--no-merges keeps a non-merge: {nm:?}"
    );
}

/// `git log | libra shortlog` (no revision, piped stdin) summarises the piped
/// `git log`/`libra log` output instead of walking the repository. Mirrors
/// `git shortlog`'s stdin mode; the repository (here, the helper's own commit)
/// is ignored once stdin carries data.
#[test]
fn test_shortlog_reads_piped_log_input() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command_with_stdin};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // Medium-format `git log` output: Alice authored two commits, Bob one.
    let log = "\
commit cccccccccccccccccccccccccccccccccccccccc
Author: Alice <a@b>
Date:   Fri Jun 26 10:00:03 2026 +0000

    third commit

commit bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
Author: Bob <bob@b>
Date:   Fri Jun 26 10:00:02 2026 +0000

    second commit subject

commit aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
Author: Alice <a@b>
Date:   Fri Jun 26 10:00:01 2026 +0000

    first commit
";

    let out = run_libra_command_with_stdin(&["shortlog"], p, log);
    assert_cli_success(&out, "shortlog from stdin");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Aggregated from stdin (Alice 2, Bob 1), NOT from the repo's single commit.
    assert!(stdout.contains("2  Alice"), "Alice has 2 commits: {stdout}");
    assert!(stdout.contains("1  Bob"), "Bob has 1 commit: {stdout}");
    assert!(
        stdout.contains("third commit") && stdout.contains("first commit"),
        "Alice's subjects are listed: {stdout}"
    );
    assert!(
        stdout.contains("second commit subject"),
        "Bob's subject is listed: {stdout}"
    );

    // `-s` (summary) suppresses subjects.
    let summary = run_libra_command_with_stdin(&["shortlog", "-s"], p, log);
    assert_cli_success(&summary, "shortlog -s from stdin");
    let summary_out = String::from_utf8_lossy(&summary.stdout);
    assert!(summary_out.contains("2  Alice") && summary_out.contains("1  Bob"));
    assert!(
        !summary_out.contains("third commit"),
        "summary omits subjects: {summary_out}"
    );

    // `-n` (numbered) orders the higher-count author first.
    let numbered = run_libra_command_with_stdin(&["shortlog", "-n", "-s"], p, log);
    assert_cli_success(&numbered, "shortlog -n -s from stdin");
    let numbered_out = String::from_utf8_lossy(&numbered.stdout);
    let alice_pos = numbered_out.find("Alice").expect("Alice present");
    let bob_pos = numbered_out.find("Bob").expect("Bob present");
    assert!(
        alice_pos < bob_pos,
        "Alice (2) sorts before Bob (1) with -n: {numbered_out}"
    );

    // `--author` filters the piped commits by author identity.
    let filtered = run_libra_command_with_stdin(&["shortlog", "-s", "--author=Bob"], p, log);
    assert_cli_success(&filtered, "shortlog --author from stdin");
    let filtered_out = String::from_utf8_lossy(&filtered.stdout);
    assert!(
        filtered_out.contains("Bob") && !filtered_out.contains("Alice"),
        "--author=Bob keeps only Bob: {filtered_out}"
    );

    // `--json` in stdin mode reflects the `--author` filter in `total_commits`
    // (consistent with the repo-walk path, which filters before counting).
    let json_out = run_libra_command_with_stdin(&["shortlog", "--json", "--author=Bob"], p, log);
    assert_cli_success(&json_out, "shortlog --json --author from stdin");
    let json = super::parse_json_stdout(&json_out);
    assert_eq!(json["command"], "shortlog");
    assert_eq!(
        json["data"]["total_commits"], 1,
        "total_commits counts only the filtered (Bob) commit: {json}"
    );
    assert_eq!(
        json["data"]["total_authors"], 1,
        "only Bob remains after --author=Bob: {json}"
    );
}
