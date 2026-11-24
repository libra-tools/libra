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
    let expected = r#"GUXUE (1):
      Commit_7

LEAVE (5):
      Commit_1
      Commit_2
      Commit_4
      Commit_11
      Commit_12

LENGSA (1):
      Commit_8

MMONK (1):
      Commit_10

SHY (2):
      Commit_5
      Commit_13

SunZo (2):
      Commit_9
      Commit_14
"#;

    let out_lines: Vec<_> = output.trim_end_matches('\n').lines().collect();
    let exp_lines: Vec<_> = expected.trim_end_matches('\n').lines().collect();
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

    let expected = r#"LEAVE (5):
      Commit_1
      Commit_2
      Commit_4
      Commit_11
      Commit_12

SHY (2):
      Commit_5
      Commit_13

SunZo (2):
      Commit_9
      Commit_14

GUXUE (1):
      Commit_7

LENGSA (1):
      Commit_8

MMONK (1):
      Commit_10
"#;

    let out_lines: Vec<_> = output.trim_end_matches('\n').lines().collect();
    let exp_lines: Vec<_> = expected.trim_end_matches('\n').lines().collect();
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

    let expected = r#"     1	GUXUE
     5	LEAVE
     1	LENGSA
     1	MMONK
     2	SHY
     2	SunZo
"#;

    let out_lines: Vec<_> = output.trim_end_matches('\n').lines().collect();
    let exp_lines: Vec<_> = expected.trim_end_matches('\n').lines().collect();
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

    let expected = r#"GUXUE <guxue@oa.org> (1):
      Commit_7

LEAVE <leave@oa.org> (5):
      Commit_1
      Commit_2
      Commit_4
      Commit_11
      Commit_12

LENGSA <lengsa@oa.org> (1):
      Commit_8

MMONK <mmonk@oa.org> (1):
      Commit_10

SHY <shy@oa.org> (2):
      Commit_5
      Commit_13

SunZo <sunzo@oa.org> (2):
      Commit_9
      Commit_14
"#;

    let out_lines: Vec<_> = output.trim_end_matches('\n').lines().collect();
    let exp_lines: Vec<_> = expected.trim_end_matches('\n').lines().collect();
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

    let expected = r#"     5	LEAVE
     2	SHY
     2	SunZo
     1	GUXUE
     1	LENGSA
     1	MMONK
"#;

    let out_lines: Vec<_> = output.trim_end_matches('\n').lines().collect();
    let exp_lines: Vec<_> = expected.trim_end_matches('\n').lines().collect();
    assert_eq!(out_lines, exp_lines);

    // test shortlog command with -n -e options
    let args = ShortlogArgs::try_parse_from(["libra", "-n", "-e"]).unwrap();
    let mut buf = Vec::new();
    shortlog::execute_to(args, &mut buf).await.unwrap();
    let output = String::from_utf8(buf).unwrap();

    let expected = r#"LEAVE <leave@oa.org> (5):
      Commit_1
      Commit_2
      Commit_4
      Commit_11
      Commit_12

SHY <shy@oa.org> (2):
      Commit_5
      Commit_13

SunZo <sunzo@oa.org> (2):
      Commit_9
      Commit_14

GUXUE <guxue@oa.org> (1):
      Commit_7

LENGSA <lengsa@oa.org> (1):
      Commit_8

MMONK <mmonk@oa.org> (1):
      Commit_10
"#;

    let out_lines: Vec<_> = output.trim_end_matches('\n').lines().collect();
    let exp_lines: Vec<_> = expected.trim_end_matches('\n').lines().collect();
    assert_eq!(out_lines, exp_lines);

    // test shortlog command with -s -e options
    let args = ShortlogArgs::try_parse_from(["libra", "-s", "-e"]).unwrap();
    let mut buf = Vec::new();
    shortlog::execute_to(args, &mut buf).await.unwrap();
    let output = String::from_utf8(buf).unwrap();

    let expected = r#"     1	GUXUE <guxue@oa.org>
     5	LEAVE <leave@oa.org>
     1	LENGSA <lengsa@oa.org>
     1	MMONK <mmonk@oa.org>
     2	SHY <shy@oa.org>
     2	SunZo <sunzo@oa.org>
"#;

    let out_lines: Vec<_> = output.trim_end_matches('\n').lines().collect();
    let exp_lines: Vec<_> = expected.trim_end_matches('\n').lines().collect();
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

    let expected = r#"LEAVE (2):
      Commit_11
      Commit_12

MMONK (1):
      Commit_10

SHY (1):
      Commit_13

SunZo (1):
      Commit_14
"#;
    let out_lines: Vec<_> = output.trim_end_matches('\n').lines().collect();
    let exp_lines: Vec<_> = expected.trim_end_matches('\n').lines().collect();
    assert_eq!(out_lines, exp_lines);

    // test shortlog command with --until option
    let args = ShortlogArgs::try_parse_from(["libra", "--until", "2026-01-13"]).unwrap();
    let mut buf = Vec::new();
    shortlog::execute_to(args, &mut buf).await.unwrap();
    let output = String::from_utf8(buf).unwrap();

    let expected = r#"GUXUE (1):
      Commit_7

LEAVE (5):
      Commit_1
      Commit_2
      Commit_4
      Commit_11
      Commit_12

LENGSA (1):
      Commit_8

MMONK (1):
      Commit_10

SHY (2):
      Commit_5
      Commit_13

SunZo (1):
      Commit_9
"#;

    let out_lines: Vec<_> = output.trim_end_matches('\n').lines().collect();
    let exp_lines: Vec<_> = expected.trim_end_matches('\n').lines().collect();
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

    let expected = r#"LEAVE <leave@oa.org> (4):
      Commit_2
      Commit_4
      Commit_11
      Commit_12

SHY <shy@oa.org> (2):
      Commit_5
      Commit_13

GUXUE <guxue@oa.org> (1):
      Commit_7

LENGSA <lengsa@oa.org> (1):
      Commit_8

MMONK <mmonk@oa.org> (1):
      Commit_10

SunZo <sunzo@oa.org> (1):
      Commit_9
"#;

    let out_lines: Vec<_> = output.trim_end_matches('\n').lines().collect();
    let exp_lines: Vec<_> = expected.trim_end_matches('\n').lines().collect();
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
    assert_eq!(
        stdout,
        "Alice (2):\n      first commit\n      third commit\n\nBob (1):\n      second commit subject\n\n",
        "Git group format from stdin"
    );

    // `-s` (summary) suppresses subjects.
    let summary = run_libra_command_with_stdin(&["shortlog", "-s"], p, log);
    assert_cli_success(&summary, "shortlog -s from stdin");
    let summary_out = String::from_utf8_lossy(&summary.stdout);
    assert_eq!(
        summary_out, "     2	Alice\n     1	Bob\n",
        "Git `%6d` summary from stdin"
    );
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

/// LG-20 (M-SLOG S1–S3): ranges, multiple revisions, exclusions, `--all`, and
/// `-- <path>` selection (t4201:203 port).
#[test]
#[serial(cwd)]
fn test_shortlog_revision_ranges_matrix() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    for index in 1..=4 {
        let file = format!("f{index}.txt");
        fs::write(p.join(&file), format!("c{index}\n")).unwrap();
        assert_cli_success(&run_libra_command(&["add", &file], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", &format!("c{index}"), "--no-verify"], p),
            "commit",
        );
    }

    let run = |args: &[&str]| {
        let out = run_libra_command(args, p);
        assert_cli_success(&out, "shortlog");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    // S1: `A..B` selects the commits after A (2 commits).
    assert!(run(&["shortlog", "-s", "HEAD~2..HEAD"]).contains('2'));
    let detailed = run(&["shortlog", "HEAD~2..HEAD"]);
    assert!(detailed.contains("c4"), "{detailed}");
    assert!(detailed.contains("c3"), "{detailed}");
    assert!(!detailed.contains("c2"), "{detailed}");

    // S2: multiple revisions union; `^` excludes an ancestor closure.
    let multi = run(&["shortlog", "-s", "HEAD~3", "HEAD"]);
    assert_eq!(multi.split_whitespace().next(), Some("5"), "{multi}");
    let exclude = run(&["shortlog", "-s", "^HEAD~1", "HEAD"]);
    assert_eq!(exclude.split_whitespace().next(), Some("1"), "{exclude}");

    // S3: `--all` covers every ref; `-- <path>` limits the selection.
    let all = run(&["shortlog", "-s", "--all"]);
    assert_eq!(all.split_whitespace().next(), Some("5"), "{all}");
    let path = run(&["shortlog", "-s", "HEAD", "--", "f1.txt"]);
    assert_eq!(path.split_whitespace().next(), Some("1"), "{path}");

    // t4201:203 port: an empty range produces no authors.
    let empty = run(&["shortlog", "-s", "HEAD..HEAD"]);
    assert!(empty.trim().is_empty(), "{empty}");
}

/// PM-16: human output follows Git's `shortlog` format and ordering.
#[test]
fn shortlog_output_matches_git_format() {
    use std::process::{Command, Stdio};

    let libra = env!("CARGO_BIN_EXE_libra");
    let init = || {
        let repo = tempdir().unwrap();
        let setup = Command::new(libra)
            .args(["init", "--vault", "false"])
            .current_dir(repo.path())
            .env("HOME", repo.path().join(".home"))
            .env("LIBRA_TEST", "1")
            .output()
            .unwrap();
        assert!(setup.status.success());
        for (key, value) in [("user.name", "t"), ("user.email", "t@t")] {
            let out = Command::new(libra)
                .args(["config", key, value])
                .current_dir(repo.path())
                .env("HOME", repo.path().join(".home"))
                .env("LIBRA_TEST", "1")
                .output()
                .unwrap();
            assert!(out.status.success());
        }
        repo
    };
    let commit = |repo: &std::path::Path, subject: &str, author: &str| {
        std::fs::write(repo.join("f"), format!("{subject}\n")).unwrap();
        for args in [
            vec!["add", "f"],
            vec!["commit", "-m", subject, "--author", author, "--no-verify"],
        ] {
            let out = Command::new(libra)
                .args(&args)
                .current_dir(repo)
                .env("HOME", repo.join(".home"))
                .env("LIBRA_TEST", "1")
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}: {out:?}");
        }
    };
    let run = |repo: &std::path::Path, args: &[&str]| {
        Command::new(libra)
            .args(args)
            .current_dir(repo)
            .env("HOME", repo.join(".home"))
            .env("LIBRA_TEST", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };

    // F16m: Alice(2), Bob(1).
    let repo = init();
    commit(repo.path(), "first", "Alice <alice@example.com>");
    commit(repo.path(), "second", "Bob <bob@example.com>");
    commit(repo.path(), "third", "Alice <alice@example.com>");
    let out = run(repo.path(), &["shortlog", "HEAD"]);
    assert!(out.status.success(), "AC1");
    assert_eq!(
        out.stdout,
        b"Alice (2):\n      first\n      third\n\nBob (1):\n      second\n\n".to_vec(),
        "AC1"
    );
    let out = run(repo.path(), &["shortlog", "-s", "HEAD"]);
    assert!(out.status.success(), "AC2");
    assert_eq!(out.stdout, b"     2\tAlice\n     1\tBob\n".to_vec(), "AC2");

    // F16o: byte-order grouping and numbered ordering.
    let repo = init();
    commit(repo.path(), "s1", "Amy <amy@example.com>");
    commit(repo.path(), "s2", "Zed <zed@example.com>");
    commit(repo.path(), "s3", "bob <bob@example.com>");
    commit(repo.path(), "s4", "Zed <zed@example.com>");
    let out = run(repo.path(), &["shortlog", "-s", "HEAD"]);
    assert!(out.status.success(), "AC3");
    assert_eq!(
        out.stdout,
        b"     1\tAmy\n     2\tZed\n     1\tbob\n".to_vec(),
        "AC3"
    );
    let out = run(repo.path(), &["shortlog", "-n", "-s", "HEAD"]);
    assert!(out.status.success(), "AC4");
    assert_eq!(
        out.stdout,
        b"     2\tZed\n     1\tAmy\n     1\tbob\n".to_vec(),
        "AC4"
    );
    let out = run(
        repo.path(),
        &["shortlog", "-s", "--reverse", "--top", "2", "HEAD"],
    );
    assert!(out.status.success(), "AC8");
    assert_eq!(out.stdout, b"     1\tbob\n     2\tZed\n".to_vec(), "AC8");

    // AC6: the JSON path keeps the previous payload byte-for-byte.
    let out = run(
        repo.path(),
        &[
            "--json",
            "shortlog",
            "-s",
            "--min-count",
            "2",
            "--reverse",
            "--top",
            "2",
            "HEAD",
        ],
    );
    assert!(out.status.success(), "AC6");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "{\n  \"command\": \"shortlog\",\n  \"data\": {\n    \"authors\": [\n      {\n        \"count\": 2,\n        \"email\": null,\n        \"name\": \"Zed\",\n        \"subjects\": []\n      }\n    ],\n    \"email\": false,\n    \"numbered\": false,\n    \"revision\": \"HEAD\",\n    \"summary\": true,\n    \"total_authors\": 1,\n    \"total_commits\": 4\n  },\n  \"ok\": true\n}\n",
        "AC6"
    );

    // F16m with two emails for Alice: full-key byte ordering.
    let repo = init();
    commit(repo.path(), "e1", "Alice <alice@example.com>");
    commit(repo.path(), "e2", "Bob <bob@example.com>");
    commit(repo.path(), "e3", "Alice <alice@other.example>");
    let out = run(repo.path(), &["shortlog", "-e", "-s", "HEAD"]);
    assert!(out.status.success(), "AC5");
    assert_eq!(
        out.stdout,
        b"     1\tAlice <alice@example.com>\n     1\tAlice <alice@other.example>\n     1\tBob <bob@example.com>\n".to_vec(),
        "AC5"
    );
}

/// PM-16 AC7: piped `log` input renders the same Git group format, with the
/// subjects in reverse input order.
#[test]
fn shortlog_stdin_output_matches_git_format() {
    let repo = tempdir().unwrap();
    let libra = env!("CARGO_BIN_EXE_libra");
    let _ = Command::new(libra)
        .args(["init", "--vault", "false"])
        .current_dir(repo.path())
        .env("HOME", repo.path().join(".home"))
        .env("LIBRA_TEST", "1")
        .output()
        .unwrap();
    let log = "commit 1111111111111111111111111111111111111111\nAuthor: Alice <alice@example.com>\nDate:   Fri Jun 26 10:00:01 2026 +0000\n\n    first\n\ncommit 2222222222222222222222222222222222222222\nAuthor: Bob <bob@example.com>\nDate:   Fri Jun 26 10:00:02 2026 +0000\n\n    second\n\ncommit 3333333333333333333333333333333333333333\nAuthor: Alice <alice@example.com>\nDate:   Fri Jun 26 10:00:03 2026 +0000\n\n    third\n";
    let out = run_libra_command_with_stdin(&["shortlog"], repo.path(), log);
    assert_cli_success(&out, "AC7");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Alice (2):\n      third\n      first\n\nBob (1):\n      second\n\n",
        "AC7"
    );
}

/// PM-30: repeatable `--group`, `--no-group`, and per-commit key de-duplication.
#[test]
fn shortlog_group_options_match_git() {
    use std::process::Command;

    let libra = env!("CARGO_BIN_EXE_libra");
    let repo = tempdir().unwrap();
    let p = repo.path();
    let home = p.join(".home");
    let run = |args: &[&str], envs: &[(&str, &str)]| {
        let mut cmd = Command::new(libra);
        cmd.args(args)
            .current_dir(p)
            .env("HOME", &home)
            .env("LIBRA_TEST", "1")
            .env("GIT_AUTHOR_DATE", "1112875200 +0000")
            .env("GIT_COMMITTER_DATE", "1112875200 +0000");
        for (key, value) in envs {
            cmd.env(key, value);
        }
        cmd.output().unwrap()
    };
    assert!(run(&["init", "--vault", "false"], &[]).status.success());
    assert!(run(&["config", "user.name", "t"], &[]).status.success());
    assert!(run(&["config", "user.email", "t@t"], &[]).status.success());
    let commit = |body: &str, author: &str, committer_name: &str, committer_email: &str| {
        std::fs::write(p.join("f"), format!("{body}\n")).unwrap();
        assert!(run(&["add", "f"], &[]).status.success());
        let out = run(
            &["commit", "-m", body, "--author", author, "--no-verify"],
            &[
                ("GIT_COMMITTER_NAME", committer_name),
                ("GIT_COMMITTER_EMAIL", committer_email),
            ],
        );
        assert!(out.status.success(), "{out:?}");
    };
    commit(
        "first",
        "Alice <alice@example.com>",
        "Carol",
        "carol@example.com",
    );
    commit(
        "second\n\nCo-authored-by: Alice <alice@example.com>\nCo-authored-by: Dave <dave@example.com>",
        "Bob <bob@example.com>",
        "Carol",
        "carol@example.com",
    );
    commit(
        "third",
        "Alice <alice@other.example>",
        "Dan",
        "dan@example.com",
    );

    // AC1: both groups contribute keys for every commit.
    let out = run(
        &[
            "shortlog",
            "-s",
            "--group=author",
            "--group=committer",
            "HEAD",
        ],
        &[],
    );
    assert!(out.status.success(), "AC1");
    assert_eq!(
        out.stdout,
        b"     2\tAlice\n     1\tBob\n     2\tCarol\n     1\tDan\n".to_vec(),
        "AC1"
    );

    // AC2: `--no-group` clears earlier groups; only committer remains.
    let out = run(
        &[
            "shortlog",
            "-s",
            "--group=author",
            "--no-group",
            "--group=committer",
            "HEAD",
        ],
        &[],
    );
    assert!(out.status.success(), "AC2");
    assert_eq!(out.stdout, b"     2\tCarol\n     1\tDan\n".to_vec(), "AC2");

    // AC3: an empty group list falls back to author.
    let out = run(&["shortlog", "-s", "--no-group", "HEAD"], &[]);
    assert!(out.status.success(), "AC3");
    assert_eq!(out.stdout, b"     2\tAlice\n     1\tBob\n".to_vec(), "AC3");

    // AC4: unknown group types are usage errors.
    let out = run(&["shortlog", "-s", "--group=bogus", "HEAD"], &[]);
    assert!(!out.status.success(), "AC4 must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-002"),
        "AC4 code"
    );
}

/// PM-30 AC5/AC6: repeated keys inside one commit count once.
#[test]
fn shortlog_group_dedups_within_commit() {
    use std::process::Command;

    let libra = env!("CARGO_BIN_EXE_libra");
    let repo = tempdir().unwrap();
    let p = repo.path();
    let home = p.join(".home");
    let run = |args: &[&str], envs: &[(&str, &str)]| {
        let mut cmd = Command::new(libra);
        cmd.args(args)
            .current_dir(p)
            .env("HOME", &home)
            .env("LIBRA_TEST", "1")
            .env("GIT_AUTHOR_DATE", "1000000000 +0000")
            .env("GIT_COMMITTER_DATE", "1000000000 +0000");
        for (key, value) in envs {
            cmd.env(key, value);
        }
        cmd.output().unwrap()
    };
    assert!(run(&["init", "--vault", "false"], &[]).status.success());
    assert!(run(&["config", "user.name", "t"], &[]).status.success());
    assert!(run(&["config", "user.email", "t@t"], &[]).status.success());

    // AC5 fixture: repeated trailer values.
    std::fs::write(p.join("f"), "root\n").unwrap();
    assert!(run(&["add", "f"], &[]).status.success());
    assert!(
        run(&["commit", "-m", "root", "--no-verify"], &[])
            .status
            .success()
    );
    for (body, author) in [
        ("a\n\nRepeated: Foo\nRepeated: Foo", "A <a@x>"),
        ("b\n\nRepeated: Foo\nRepeated: Bar", "B <b@x>"),
    ] {
        std::fs::write(p.join("f"), format!("{body}\n")).unwrap();
        assert!(run(&["add", "f"], &[]).status.success());
        assert!(
            run(
                &["commit", "-m", body, "--author", author, "--no-verify"],
                &[]
            )
            .status
            .success()
        );
    }
    let out = run(&["shortlog", "-s", "--group=trailer:repeated", "HEAD"], &[]);
    assert!(out.status.success(), "AC5");
    assert_eq!(out.stdout, b"     1\tBar\n     2\tFoo\n".to_vec(), "AC5");

    // AC6 fixture: the same name from author and committer groups.
    let repo = tempdir().unwrap();
    let p = repo.path();
    let home = p.join(".home");
    let run = |args: &[&str], envs: &[(&str, &str)]| {
        let mut cmd = Command::new(libra);
        cmd.args(args)
            .current_dir(p)
            .env("HOME", &home)
            .env("LIBRA_TEST", "1")
            .env("GIT_AUTHOR_DATE", "1000000000 +0000")
            .env("GIT_COMMITTER_DATE", "1000000000 +0000");
        for (key, value) in envs {
            cmd.env(key, value);
        }
        cmd.output().unwrap()
    };
    assert!(run(&["init", "--vault", "false"], &[]).status.success());
    assert!(run(&["config", "user.name", "t"], &[]).status.success());
    assert!(run(&["config", "user.email", "t@t"], &[]).status.success());
    for (body, author) in [("c1", "Same <s@x>"), ("c2", "Ann <a@x>")] {
        std::fs::write(p.join("f"), format!("{body}\n")).unwrap();
        assert!(run(&["add", "f"], &[]).status.success());
        assert!(
            run(
                &["commit", "-m", body, "--author", author, "--no-verify"],
                &[
                    ("GIT_COMMITTER_NAME", "Same"),
                    ("GIT_COMMITTER_EMAIL", "s@x")
                ],
            )
            .status
            .success()
        );
    }
    let out = run(
        &[
            "shortlog",
            "-s",
            "--group=author",
            "--group=committer",
            "HEAD",
        ],
        &[],
    );
    assert!(out.status.success(), "AC6");
    assert_eq!(out.stdout, b"     1\tAnn\n     2\tSame\n".to_vec(), "AC6");
}

/// PM-19: the `-<n>` shorthand and `--max-count` limit the whole walk.
#[test]
fn shortlog_max_count_shorthand() {
    use std::process::Command;

    let libra = env!("CARGO_BIN_EXE_libra");
    let repo = tempdir().unwrap();
    let p = repo.path();
    let home = p.join(".home");
    let run = |args: &[&str], envs: &[(&str, &str)]| {
        let mut cmd = Command::new(libra);
        cmd.args(args)
            .current_dir(p)
            .env("HOME", &home)
            .env("LIBRA_TEST", "1")
            .env("GIT_AUTHOR_DATE", "1112875200 +0000")
            .env("GIT_COMMITTER_DATE", "1112875200 +0000");
        for (key, value) in envs {
            cmd.env(key, value);
        }
        cmd.output().unwrap()
    };
    assert!(run(&["init", "--vault", "false"], &[]).status.success());
    assert!(run(&["config", "user.name", "t"], &[]).status.success());
    assert!(run(&["config", "user.email", "t@t"], &[]).status.success());
    let commit =
        |body: &str, author: &str, committer_name: &str, committer_email: &str, ts: &str| {
            std::fs::write(p.join("f"), format!("{body}\n")).unwrap();
            assert!(run(&["add", "f"], &[]).status.success());
            let out = run(
                &["commit", "-m", body, "--author", author, "--no-verify"],
                &[
                    ("GIT_COMMITTER_NAME", committer_name),
                    ("GIT_COMMITTER_EMAIL", committer_email),
                    ("GIT_AUTHOR_DATE", ts),
                    ("GIT_COMMITTER_DATE", ts),
                ],
            );
            assert!(out.status.success(), "{out:?}");
        };
    commit(
        "first",
        "Alice <alice@example.com>",
        "Carol",
        "carol@example.com",
        "1112875200 +0000",
    );
    commit(
        "second",
        "Bob <bob@example.com>",
        "Carol",
        "carol@example.com",
        "1146484800 +0000",
    );
    commit(
        "third",
        "Alice <alice@other.example>",
        "Dan",
        "dan@example.com",
        "1180785600 +0000",
    );

    // AC1: `-1` keeps only the newest commit.
    let out = run(&["shortlog", "-s", "-1", "HEAD"], &[]);
    assert!(out.status.success(), "AC1");
    assert_eq!(out.stdout, b"     1\tAlice\n".to_vec(), "AC1");

    // AC2: `-2` keeps the two newest commits.
    let out = run(&["shortlog", "-2", "HEAD"], &[]);
    assert!(out.status.success(), "AC2");
    assert_eq!(
        out.stdout,
        b"Alice (1):\n      third\n\nBob (1):\n      second\n\n".to_vec(),
        "AC2"
    );

    // AC4: the long spelling is equivalent.
    let out = run(&["shortlog", "-s", "--max-count=1", "HEAD"], &[]);
    assert!(out.status.success(), "AC4");
    assert_eq!(out.stdout, b"     1\tAlice\n".to_vec(), "AC4");

    // AC3: `-n` stays `--numbered` next to `-<n>`.
    let repo = tempdir().unwrap();
    let p = repo.path();
    let home = p.join(".home");
    let run = |args: &[&str], envs: &[(&str, &str)]| {
        let mut cmd = Command::new(libra);
        cmd.args(args)
            .current_dir(p)
            .env("HOME", &home)
            .env("LIBRA_TEST", "1")
            .env("GIT_AUTHOR_DATE", "1000000000 +0000")
            .env("GIT_COMMITTER_DATE", "1000000000 +0000");
        for (key, value) in envs {
            cmd.env(key, value);
        }
        cmd.output().unwrap()
    };
    assert!(run(&["init", "--vault", "false"], &[]).status.success());
    assert!(run(&["config", "user.name", "t"], &[]).status.success());
    assert!(run(&["config", "user.email", "t@t"], &[]).status.success());
    for (index, author) in [
        ("1", "Amy <amy@example.com>"),
        ("2", "Zed <zed@example.com>"),
        ("3", "bob <bob@example.com>"),
        ("4", "Zed <zed@example.com>"),
    ] {
        std::fs::write(p.join("f"), format!("{index}\n")).unwrap();
        assert!(run(&["add", "f"], &[]).status.success());
        assert!(
            run(
                &["commit", "-m", index, "--author", author, "--no-verify"],
                &[]
            )
            .status
            .success()
        );
    }
    let out = run(&["shortlog", "-ns", "-4", "HEAD"], &[]);
    assert!(out.status.success(), "AC3");
    assert_eq!(
        out.stdout,
        b"     2\tZed\n     1\tAmy\n     1\tbob\n".to_vec(),
        "AC3"
    );
}

/// PM-40: `shortlog` and `check-mailmap` share the mailmap resolver, which
/// reads `.mailmap` then `mailmap.file` (the latter overriding) and matches
/// names/emails case-insensitively.
#[test]
fn shortlog_and_check_mailmap_apply_mailmap() {
    use std::process::Command;

    let libra = env!("CARGO_BIN_EXE_libra");
    let repo = tempdir().unwrap();
    let p = repo.path();
    let home = p.join(".home");
    let run = |args: &[&str], envs: &[(&str, &str)]| {
        let mut cmd = Command::new(libra);
        cmd.args(args)
            .current_dir(p)
            .env("HOME", &home)
            .env("LIBRA_TEST", "1")
            .env("GIT_AUTHOR_DATE", "1112875200 +0000")
            .env("GIT_COMMITTER_DATE", "1112875200 +0000");
        for (key, value) in envs {
            cmd.env(key, value);
        }
        cmd.output().unwrap()
    };
    assert!(run(&["init", "--vault", "false"], &[]).status.success());
    assert!(run(&["config", "user.name", "t"], &[]).status.success());
    assert!(run(&["config", "user.email", "t@t"], &[]).status.success());
    let commit =
        |body: &str, author: &str, committer_name: &str, committer_email: &str, ts: &str| {
            std::fs::write(p.join("f"), format!("{body}\n")).unwrap();
            assert!(run(&["add", "f"], &[]).status.success());
            let out = run(
                &["commit", "-m", body, "--author", author, "--no-verify"],
                &[
                    ("GIT_COMMITTER_NAME", committer_name),
                    ("GIT_COMMITTER_EMAIL", committer_email),
                    ("GIT_AUTHOR_DATE", ts),
                    ("GIT_COMMITTER_DATE", ts),
                ],
            );
            assert!(out.status.success(), "{out:?}");
        };
    commit(
        "first",
        "Alice <alice@example.com>",
        "Carol",
        "carol@example.com",
        "1112875200 +0000",
    );
    commit(
        "second",
        "Bob <bob@example.com>",
        "Carol",
        "carol@example.com",
        "1146484800 +0000",
    );
    commit(
        "third",
        "Alice <alice@other.example>",
        "Dan",
        "dan@example.com",
        "1180785600 +0000",
    );

    // AC1/AC3: the worktree mailmap maps both Alice emails and is case-insensitive.
    std::fs::write(
        p.join(".mailmap"),
        "Alice Proper <alice@proper.example> <alice@other.example>\nAlice Proper <alice@proper.example> Alice <alice@example.com>\n",
    )
    .unwrap();
    let out = run(&["shortlog", "-s", "-e", "HEAD"], &[]);
    assert!(out.status.success(), "AC1");
    assert_eq!(
        out.stdout,
        b"     2\tAlice Proper <alice@proper.example>\n     1\tBob <bob@example.com>\n".to_vec(),
        "AC1"
    );
    let out = run(&["check-mailmap", "alice <ALICE@EXAMPLE.COM>"], &[]);
    assert!(out.status.success(), "AC3");
    assert_eq!(
        out.stdout,
        b"Alice Proper <alice@proper.example>\n".to_vec(),
        "AC3"
    );

    // AC2: `mailmap.file` is read after `.mailmap` and overrides it.
    std::fs::write(
        p.join(".mailmap"),
        "Robert <robert@x.example> <bob@example.com>\n",
    )
    .unwrap();
    let other = p.join("other.map");
    std::fs::write(&other, "Bobby <bobby@x.example> <bob@example.com>\n").unwrap();
    assert!(
        run(&["config", "mailmap.file", other.to_str().unwrap()], &[])
            .status
            .success()
    );
    let out = run(&["shortlog", "-s", "-e", "HEAD"], &[]);
    assert!(out.status.success(), "AC2");
    assert_eq!(
        out.stdout,
        b"     1\tAlice <alice@example.com>\n     1\tAlice <alice@other.example>\n     1\tBobby <bobby@x.example>\n".to_vec(),
        "AC2"
    );

    // AC4: committer grouping uses the mapped identity.
    assert!(
        run(&["config", "unset", "mailmap.file"], &[])
            .status
            .success()
    );
    std::fs::write(
        p.join(".mailmap"),
        "Caroline <caroline@x.example> <carol@example.com>\n",
    )
    .unwrap();
    let out = run(&["shortlog", "-s", "-e", "-c", "HEAD"], &[]);
    assert!(out.status.success(), "AC4");
    assert_eq!(
        out.stdout,
        b"     2\tCaroline <caroline@x.example>\n     1\tDan <dan@example.com>\n".to_vec(),
        "AC4"
    );
}

/// Build the shared F16m fixture (three commits, Carol/Dan as committers).
fn shortlog_f16m() -> (tempfile::TempDir, impl Fn(&[&str]) -> std::process::Output) {
    use std::process::Command;
    let libra = env!("CARGO_BIN_EXE_libra");
    let repo = tempdir().unwrap();
    let p = repo.path().to_path_buf();
    let home = p.join(".home");
    let run_owned = move |args: &[&str]| {
        Command::new(libra)
            .args(args)
            .current_dir(&p)
            .env("HOME", &home)
            .env("LIBRA_TEST", "1")
            .env("GIT_AUTHOR_DATE", "1112875200 +0000")
            .env("GIT_COMMITTER_DATE", "1112875200 +0000")
            .output()
            .unwrap()
    };
    assert!(run_owned(&["init", "--vault", "false"]).status.success());
    assert!(run_owned(&["config", "user.name", "t"]).status.success());
    assert!(run_owned(&["config", "user.email", "t@t"]).status.success());
    let commit =
        |body: &str, author: &str, committer_name: &str, committer_email: &str, ts: &str| {
            std::fs::write(repo.path().join("f"), format!("{body}\n")).unwrap();
            assert!(run_owned(&["add", "f"]).status.success());
            let out = Command::new(libra)
                .args(["commit", "-m", body, "--author", author, "--no-verify"])
                .current_dir(repo.path())
                .env("HOME", repo.path().join(".home"))
                .env("LIBRA_TEST", "1")
                .env("GIT_COMMITTER_NAME", committer_name)
                .env("GIT_COMMITTER_EMAIL", committer_email)
                .env("GIT_AUTHOR_DATE", ts)
                .env("GIT_COMMITTER_DATE", ts)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
        };
    commit(
        "first",
        "Alice <alice@example.com>",
        "Carol",
        "carol@example.com",
        "1112875200 +0000",
    );
    commit(
        "second",
        "Bob <bob@example.com>",
        "Carol",
        "carol@example.com",
        "1146484800 +0000",
    );
    commit(
        "third",
        "Alice <alice@other.example>",
        "Dan",
        "dan@example.com",
        "1180785600 +0000",
    );
    (repo, run_owned)
}

/// PM-35: `format:` groups (plus DWIM `%`) and `--date`.
#[test]
fn shortlog_format_groups_and_date() {
    let (repo, run) = shortlog_f16m();
    // AC1: `%cN` groups by committer name.
    let out = run(&["shortlog", "-s", "--group=format:%cN", "HEAD"]);
    assert!(out.status.success(), "AC1");
    assert_eq!(out.stdout, b"     2\tCarol\n     1\tDan\n".to_vec(), "AC1");

    // AC2: values containing `%` are DWIM format groups.
    let out = run(&["shortlog", "-s", "--group=%cn", "--group=%an", "HEAD"]);
    assert!(out.status.success(), "AC2");
    assert_eq!(
        out.stdout,
        b"     2\tAlice\n     1\tBob\n     2\tCarol\n     1\tDan\n".to_vec(),
        "AC2"
    );

    // AC3: `--date` applies to date placeholders in the group format.
    let out = run(&[
        "shortlog",
        "-s",
        "--date=format:%Y",
        "--group=format:%cN (%cd)",
        "HEAD",
    ]);
    assert!(out.status.success(), "AC3");
    assert_eq!(
        out.stdout,
        b"     1\tCarol (2005)\n     1\tCarol (2006)\n     1\tDan (2007)\n".to_vec(),
        "AC3"
    );
    drop(repo);
}

/// PM-35: `%aN`/`%cN` format groups use the shared mailmap.
#[test]
fn shortlog_format_groups_apply_mailmap() {
    let (repo, run) = shortlog_f16m();
    std::fs::write(
        repo.path().join(".mailmap"),
        "Carol Proper <carol@proper.example> <carol@example.com>\nRobert <robert@x.example> <bob@example.com>\n",
    )
    .unwrap();
    let out = run(&["shortlog", "-s", "--group=format:%cN", "HEAD"]);
    assert!(out.status.success(), "AC4");
    assert_eq!(
        out.stdout,
        b"     2\tCarol Proper\n     1\tDan\n".to_vec(),
        "AC4"
    );
    let out = run(&["shortlog", "-s", "--group=format:%aN", "HEAD"]);
    assert!(out.status.success(), "AC5");
    assert_eq!(
        out.stdout,
        b"     2\tAlice\n     1\tRobert\n".to_vec(),
        "AC5"
    );
    drop(repo);
}
