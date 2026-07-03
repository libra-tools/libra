//! Tests cherry-pick scenarios that apply commits and verify results or conflicts.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::{fs, path::PathBuf};

use libra::{
    command::{
        add, cherry_pick, cherry_pick::CherryPickArgs, commit, init, switch, switch::SwitchArgs,
    },
    internal::head::Head,
};
use serial_test::serial;
use tempfile::tempdir;

use super::*;

#[test]
fn test_cherry_pick_cli_outside_repository_returns_fatal_128() {
    let temp = tempdir().unwrap();
    let output = run_libra_command(&["cherry-pick", "abc123"], temp.path());
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal: not a libra repository"),
        "unexpected stderr: {stderr}"
    );
}

/// Test basic cherry-pick functionality
/// This test follows the workflow:
/// 1. Create a common ancestor commit (C1)
/// 2. Create a feature branch and add commits (C2, C3)
/// 3. Switch back to master branch
/// 4. Cherry-pick feature commits to master
#[tokio::test]
#[serial]
async fn test_basic_cherry_pick() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    println!("===== SCENARIO: BASIC CHERRY-PICK TEST =====");

    // --- 1. Create common ancestor commit (C1) ---
    fs::write("base.txt", "base").unwrap();
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
        message: Some("C1: Initial commit, our common ancestor".to_string()),
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
    println!("C1: Created common ancestor.");

    // --- 2. Create and switch to feature branch ---
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
    println!("Switched to new branch 'feature'.");

    // --- 3. Create two commits on feature branch ---
    // Commit C2: First target to cherry-pick
    fs::write("feature_a.txt", "feature A").unwrap();
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
        message: Some("C2: Add feature_a.txt".to_string()),
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
    println!("C2: Added feature_a.txt on feature branch.");

    // Get C2 commit hash for cherry-picking later
    let c2_commit = Head::current_commit()
        .await
        .expect("Should have current commit");

    // Commit C3: Second target to cherry-pick
    fs::write("feature_b.txt", "feature B").unwrap();
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
        message: Some("C3: Add feature_b.txt".to_string()),
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
    println!("C3: Added feature_b.txt on feature branch.");

    // --- 4. Switch back to master branch ---
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
    println!("Switched back to master.");

    // --- 5. Verify initial state on master ---
    println!("\nCherry-pick test repo is ready. Current state:");
    let files: Vec<_> = fs::read_dir(".")
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with('.') && name.ends_with(".txt") {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    for file in &files {
        println!("{file}");
    }

    // Should only have base.txt on master
    assert!(
        PathBuf::from("base.txt").exists(),
        "base.txt should exist on master"
    );
    assert!(
        !PathBuf::from("feature_a.txt").exists(),
        "feature_a.txt should not exist on master before cherry-pick"
    );
    assert!(
        !PathBuf::from("feature_b.txt").exists(),
        "feature_b.txt should not exist on master before cherry-pick"
    );

    // --- 6. Cherry-pick C2 (feature_a.txt) with --no-commit flag ---
    println!("\n--- Cherry-picking C2 with --no-commit ---");
    cherry_pick::execute(cherry_pick::CherryPickArgs {
        commits: vec![c2_commit.to_string()],
        no_commit: true,
        ..Default::default()
    })
    .await;

    // --- 7. Verify state after cherry-pick --no-commit ---
    println!("Files after cherry-pick --no-commit:");
    let files_after_cherry_pick: Vec<_> = fs::read_dir(".")
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with('.') && name.ends_with(".txt") {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    for file in &files_after_cherry_pick {
        println!("{file}");
    }

    // Should now have both base.txt and feature_a.txt
    assert!(
        PathBuf::from("base.txt").exists(),
        "base.txt should still exist"
    );
    assert!(
        PathBuf::from("feature_a.txt").exists(),
        "feature_a.txt should exist after cherry-pick"
    );
    assert!(
        !PathBuf::from("feature_b.txt").exists(),
        "feature_b.txt should not exist (not cherry-picked)"
    );

    // Verify content of cherry-picked file
    let feature_a_content = fs::read_to_string("feature_a.txt").unwrap();
    assert_eq!(
        feature_a_content, "feature A",
        "feature_a.txt should have correct content"
    );

    // Check that changes are staged but not committed (no new commit created)
    let _ = Head::current_commit().await.expect("Should have HEAD");

    // The head should still be the same as before cherry-pick since we used --no-commit
    // In a real test, we might want to check the index status here

    println!("Cherry-pick --no-commit test passed");

    println!("\nAll cherry-pick tests completed successfully!");
}

/// Test cherry-pick with automatic commit
#[tokio::test]
#[serial]
async fn test_cherry_pick_with_commit() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create base commit
    fs::write("base.txt", "base content").unwrap();
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

    fs::write("feature.txt", "feature content").unwrap();
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

    let feature_commit = Head::current_commit()
        .await
        .expect("Should have current commit");

    // Switch back to master
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

    let head_before = Head::current_commit()
        .await
        .expect("Should have HEAD before cherry-pick");

    // Cherry-pick with automatic commit
    cherry_pick::execute(cherry_pick::CherryPickArgs {
        commits: vec![feature_commit.to_string()],
        no_commit: false,
        ..Default::default()
    })
    .await;

    // Verify new commit was created
    let head_after = Head::current_commit()
        .await
        .expect("Should have HEAD after cherry-pick");
    assert_ne!(
        head_before, head_after,
        "A new commit should have been created"
    );
    let cherry_pick_commit: Commit =
        load_object(&head_after).expect("Should load cherry-pick commit");
    assert_eq!(cherry_pick_commit.message.trim(), "Feature commit");
    assert!(
        !cherry_pick_commit
            .message
            .contains("(cherry picked from commit "),
        "default cherry-pick should not append source line"
    );

    // Verify file was cherry-picked
    assert!(
        PathBuf::from("feature.txt").exists(),
        "feature.txt should exist after cherry-pick"
    );
    let content = fs::read_to_string("feature.txt").unwrap();
    assert_eq!(
        content, "feature content",
        "feature.txt should have correct content"
    );

    println!("Cherry-pick with commit test passed");
}

/// Test cherry-pick multiple commits
#[tokio::test]
#[serial]
async fn test_cherry_pick_multiple_commits() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create base commit
    fs::write("base.txt", "base").unwrap();
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

    // Create feature branch
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

    // Create first feature commit
    fs::write("file1.txt", "content1").unwrap();
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
        message: Some("Feature commit 1".to_string()),
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
    let commit1 = Head::current_commit().await.expect("Should have commit1");

    // Create second feature commit
    fs::write("file2.txt", "content2").unwrap();
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
        message: Some("Feature commit 2".to_string()),
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
    let commit2 = Head::current_commit().await.expect("Should have commit2");

    // Switch back to master
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

    // Cherry-pick both commits
    cherry_pick::execute(cherry_pick::CherryPickArgs {
        commits: vec![commit1.to_string(), commit2.to_string()],
        no_commit: false,
        ..Default::default()
    })
    .await;

    // Verify both files exist
    assert!(
        PathBuf::from("file1.txt").exists(),
        "file1.txt should exist"
    );
    assert!(
        PathBuf::from("file2.txt").exists(),
        "file2.txt should exist"
    );

    let content1 = fs::read_to_string("file1.txt").unwrap();
    let content2 = fs::read_to_string("file2.txt").unwrap();
    assert_eq!(
        content1, "content1",
        "file1.txt should have correct content"
    );
    assert_eq!(
        content2, "content2",
        "file2.txt should have correct content"
    );

    println!("Multiple commits cherry-pick test passed");
}

/// Test error cases for cherry-pick
#[tokio::test]
#[serial]
async fn test_cherry_pick_errors() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Test cherry-picking non-existent commit should fail gracefully
    cherry_pick::execute(cherry_pick::CherryPickArgs {
        commits: vec!["nonexistent".to_string()],
        no_commit: false,
        ..Default::default()
    })
    .await;

    println!("Error handling test completed");
}

#[tokio::test]
#[serial]
async fn test_cherry_pick_x_appends_source_line_to_commit_message() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    let output = run_libra_command(&["switch", "-c", "feature"], repo.path());
    assert_cli_success(&output, "switch -c feature should succeed");

    fs::write("feature.txt", "feature content\n").unwrap();
    let output = run_libra_command(&["add", "feature.txt"], repo.path());
    assert_cli_success(&output, "add feature.txt should succeed");

    let output = run_libra_command(
        &["commit", "-m", "Feature commit", "--no-verify"],
        repo.path(),
    );
    assert_cli_success(&output, "feature commit should succeed");

    let feature_commit = Head::current_commit()
        .await
        .expect("expected feature commit");

    let output = run_libra_command(&["switch", "main"], repo.path());
    assert_cli_success(&output, "switch main should succeed");

    let output = run_libra_command(
        &["cherry-pick", "-x", &feature_commit.to_string()],
        repo.path(),
    );
    assert_cli_success(&output, "cherry-pick -x should succeed");

    let head_after = Head::current_commit()
        .await
        .expect("expected cherry-pick commit");
    let picked_commit: Commit = load_object(&head_after).expect("expected cherry-pick commit");
    let expected_source_line = format!("(cherry picked from commit {feature_commit})");
    assert!(
        picked_commit.message.contains("Feature commit"),
        "cherry-pick -x should preserve source commit message"
    );
    assert!(
        picked_commit.message.contains(&expected_source_line),
        "cherry-pick -x should append source line"
    );
}

#[test]
#[serial]
fn test_cherry_pick_invalid_commit_returns_cli_invalid_target() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["cherry-pick", "nonexistent"], repo.path());
    assert_eq!(output.status.code(), Some(129));

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        human.contains("fatal: failed to resolve commit reference 'nonexistent'"),
        "unexpected stderr: {human}"
    );
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert_eq!(report.exit_code, 129);
}

#[tokio::test]
#[serial]
async fn test_cherry_pick_merge_commit_rejection_uses_invalid_arguments_code() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    let head = Head::current_commit().await.expect("expected HEAD commit");
    let head_commit: Commit = load_object(&head).expect("failed to load HEAD commit");
    let merge_commit = Commit::from_tree_id(
        head_commit.tree_id,
        vec![head, head],
        &format_commit_msg("synthetic merge commit", None),
    );
    save_object(&merge_commit, &merge_commit.id).expect("failed to save synthetic merge commit");

    let output = run_libra_command(&["cherry-pick", &merge_commit.id.to_string()], repo.path());
    assert_eq!(output.status.code(), Some(129));

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        human.contains("fatal: cherry-picking merge commits is not supported"),
        "unexpected stderr: {human}"
    );
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert_eq!(report.exit_code, 129);
}

#[tokio::test]
#[serial]
async fn test_cherry_pick_json_output() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    let output = run_libra_command(&["switch", "-c", "feature"], repo.path());
    assert_cli_success(&output, "switch -c feature should succeed");

    fs::write("feature.txt", "feature content\n").unwrap();
    let output = run_libra_command(&["add", "feature.txt"], repo.path());
    assert_cli_success(&output, "add feature.txt should succeed");

    let output = run_libra_command(
        &["commit", "-m", "Feature commit", "--no-verify"],
        repo.path(),
    );
    assert_cli_success(&output, "feature commit should succeed");

    let feature_commit = Head::current_commit()
        .await
        .expect("expected feature commit");

    let output = run_libra_command(&["switch", "main"], repo.path());
    assert_cli_success(&output, "switch main should succeed");

    let output = run_libra_command(
        &["cherry-pick", "--json", &feature_commit.to_string()],
        repo.path(),
    );
    assert_cli_success(&output, "cherry-pick --json should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "cherry-pick");
    assert_eq!(json["data"]["no_commit"], false);
    assert_eq!(json["data"]["picked"].as_array().unwrap().len(), 1);
    assert_eq!(
        json["data"]["picked"][0]["source_commit"],
        feature_commit.to_string()
    );
    assert!(json["data"]["picked"][0]["new_commit"].as_str().is_some());
}

#[tokio::test]
#[serial]
/// Verify cherry-pick behavior under SHA-256: accepts 64-hex commit ids, rejects SHA-1 length.
async fn test_cherry_pick_sha256_hash_handling() {
    let temp_path = tempdir().unwrap();
    test::setup_clean_testing_env_in(temp_path.path());
    let _guard = ChangeDirGuard::new(temp_path.path());

    // init repo with sha256
    init::init(init::InitArgs {
        bare: false,
        initial_branch: Some("main".to_string()),
        template: None,
        repo_directory: temp_path.path().to_str().unwrap().to_string(),
        quiet: true,
        shared: None,
        object_format: Some("sha256".to_string()),
        ref_format: None,
        from_git_repository: None,
        vault: false,
    })
    .await
    .unwrap();
    libra::internal::config::ConfigKv::set("user.name", "Cherry Test User", false)
        .await
        .unwrap();
    libra::internal::config::ConfigKv::set("user.email", "cherry-test@example.com", false)
        .await
        .unwrap();

    // base commit on main
    fs::write("base.txt", "base").unwrap();
    add::execute(add::AddArgs {
        pathspec: vec!["base.txt".into()],
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
    commit::execute(commit::CommitArgs {
        message: Some("base".into()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    // feature branch with one commit
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some("feature".into()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;
    fs::write("feature.txt", "feature").unwrap();
    add::execute(add::AddArgs {
        pathspec: vec!["feature.txt".into()],
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
    commit::execute(commit::CommitArgs {
        message: Some("feature".into()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;
    let feature_commit = Head::current_commit().await.expect("need feature commit");
    assert_eq!(feature_commit.to_string().len(), 64);

    // back to main
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".into()),
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
    let head_before = Head::current_commit().await.unwrap();

    // attempt cherry-pick with SHA-1 length hash: should no-op and not create file
    cherry_pick::execute(CherryPickArgs {
        commits: vec!["4b825dc642cb6eb9a060e54bf8d69288fbee4904".into()],
        no_commit: false,
        ..Default::default()
    })
    .await;
    let head_after_invalid = Head::current_commit().await.unwrap();
    assert_eq!(
        head_before, head_after_invalid,
        "invalid hash must not advance HEAD"
    );
    assert!(
        !PathBuf::from("feature.txt").exists(),
        "invalid hash must not apply changes"
    );

    // cherry-pick with valid SHA-256 commit should succeed
    cherry_pick::execute(CherryPickArgs {
        commits: vec![feature_commit.to_string()],
        no_commit: false,
        ..Default::default()
    })
    .await;
    let head_after_valid = Head::current_commit().await.unwrap();
    assert_ne!(
        head_before, head_after_valid,
        "valid cherry-pick should create new commit"
    );
    assert!(
        PathBuf::from("feature.txt").exists(),
        "feature.txt should be present after valid cherry-pick"
    );
}

// ── Batch 0: commit-modifier flags (-x / -s / -e / --allow-empty*) ──

/// `libra rev-parse <rev>` → trimmed OID string (panics on failure).
fn cp_rev_parse(repo: &std::path::Path, rev: &str) -> String {
    let out = run_libra_command(&["rev-parse", rev], repo);
    assert_cli_success(&out, "rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Raw `cat-file -p HEAD` body (includes the commit message).
fn cp_head_message(repo: &std::path::Path) -> String {
    let out = run_libra_command(&["cat-file", "-p", "HEAD"], repo);
    assert_cli_success(&out, "cat-file -p HEAD");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Fresh repo with a `feature` branch holding one commit that adds `file`=`content`
/// (message `msg`). Returns `(repo, feature_oid)` with HEAD back on `main`.
fn repo_with_feature_commit(file: &str, content: &str, msg: &str) -> (tempfile::TempDir, String) {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "switch -c feature",
    );
    std::fs::write(p.join(file), content).unwrap();
    assert_cli_success(&run_libra_command(&["add", file], p), "add feature file");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", msg, "--no-verify"], p),
        "feature commit",
    );
    let oid = cp_rev_parse(p, "HEAD");
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    (repo, oid)
}

/// Like [`repo_with_feature_commit`], but the feature commit's message is stored
/// verbatim (`commit --cleanup=verbatim`), so a later `cherry-pick --cleanup`
/// has comment/whitespace content to act on (a plain `-m` commit is already
/// Strip-cleaned).
fn repo_with_verbatim_feature_commit(
    file: &str,
    content: &str,
    msg: &str,
) -> (tempfile::TempDir, String) {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "switch -c feature",
    );
    std::fs::write(p.join(file), content).unwrap();
    assert_cli_success(&run_libra_command(&["add", file], p), "add feature file");
    assert_cli_success(
        &run_libra_command(
            &["commit", "--cleanup=verbatim", "-m", msg, "--no-verify"],
            p,
        ),
        "verbatim feature commit",
    );
    let oid = cp_rev_parse(p, "HEAD");
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    (repo, oid)
}

/// `cherry-pick --cleanup=strip` removes `#` comment lines and trailing
/// whitespace from the replayed message; `--cleanup=verbatim` preserves them.
#[test]
fn cherry_pick_cleanup_strip_then_verbatim() {
    let msg = "pick subject\n\nkept body line\n# comment to strip\n";

    let (repo, oid) = repo_with_verbatim_feature_commit("f.txt", "feat\n", msg);
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--cleanup=strip", &oid], repo.path()),
        "cherry-pick --cleanup=strip",
    );
    let stripped = cp_head_message(repo.path());
    assert!(
        stripped.contains("pick subject"),
        "subject kept: {stripped}"
    );
    assert!(stripped.contains("kept body line"), "body kept: {stripped}");
    assert!(
        !stripped.contains("# comment to strip"),
        "strip must drop the `#` comment line: {stripped}"
    );

    // verbatim keeps the comment line intact.
    let (repo2, oid2) = repo_with_verbatim_feature_commit("f.txt", "feat\n", msg);
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--cleanup=verbatim", &oid2], repo2.path()),
        "cherry-pick --cleanup=verbatim",
    );
    assert!(
        cp_head_message(repo2.path()).contains("# comment to strip"),
        "verbatim must preserve the `#` comment line"
    );

    // `--cleanup=strip -s`: the body is cleaned but the blank-line separator
    // before the appended Signed-off-by trailer must survive (the trailer is
    // appended AFTER cleanup, so it is never collapsed into the body).
    let (repo3, oid3) = repo_with_verbatim_feature_commit("f.txt", "feat\n", msg);
    assert_cli_success(
        &run_libra_command(
            &["cherry-pick", "--cleanup=strip", "-s", &oid3],
            repo3.path(),
        ),
        "cherry-pick --cleanup=strip -s",
    );
    let signed = cp_head_message(repo3.path());
    assert!(
        signed.contains("\n\nSigned-off-by:"),
        "trailer separator preserved under strip: {signed:?}"
    );
    assert!(
        !signed.contains("# comment to strip"),
        "strip still drops the comment: {signed:?}"
    );

    // `--cleanup=default` with no editor falls back to `whitespace` (keeps `#`
    // lines), matching Git's "if the message is to be edited" clause and commit.
    let (repo4, oid4) = repo_with_verbatim_feature_commit("f.txt", "feat\n", msg);
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--cleanup=default", &oid4], repo4.path()),
        "cherry-pick --cleanup=default",
    );
    assert!(
        cp_head_message(repo4.path()).contains("# comment to strip"),
        "default without an editor keeps `#` lines (whitespace fallback)"
    );
}

/// The `--cleanup` mode round-trips through the SQLite sequencer: a pick that
/// conflicts, is resolved, and resumed with `--continue` still cleans the
/// resumed commit's message.
#[test]
fn cherry_pick_cleanup_survives_conflict_resume() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // feature: a commit touching shared.txt with a verbatim (messy) message.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "switch -c feature",
    );
    std::fs::write(p.join("shared.txt"), "feature\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add shared");
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--cleanup=verbatim",
                "-m",
                "conflicting subject\n\nkept body\n# strip me\n",
                "--no-verify",
            ],
            p,
        ),
        "verbatim feature commit",
    );
    let oid = cp_rev_parse(p, "HEAD");

    // main diverges on shared.txt so the pick conflicts.
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    std::fs::write(p.join("shared.txt"), "main\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add main");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main edit", "--no-verify"], p),
        "commit main",
    );

    // cherry-pick --cleanup=strip → conflict.
    assert_eq!(
        run_libra_command(&["cherry-pick", "--cleanup=strip", &oid], p)
            .status
            .code(),
        Some(128),
        "pick conflicts"
    );

    // Resolve + continue → the resumed commit applies the stored cleanup mode.
    std::fs::write(p.join("shared.txt"), "resolved\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "shared.txt"], p),
        "add resolved",
    );
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--continue"], p),
        "cherry-pick --continue",
    );
    let msg = cp_head_message(p);
    assert!(msg.contains("conflicting subject"), "subject kept: {msg}");
    assert!(
        !msg.contains("# strip me"),
        "cleanup mode survived the resume and stripped the comment: {msg}"
    );
}

/// `cherry-pick --cleanup=<bogus>` is a usage error (exit 129, LBR-CLI-002),
/// rejected up front before any commit is created.
#[test]
fn cherry_pick_invalid_cleanup_mode_rejected() {
    let (repo, oid) = repo_with_feature_commit("f.txt", "feat\n", "feature work");
    let out = run_libra_command(&["cherry-pick", "--cleanup=bogus", &oid], repo.path());
    assert_eq!(
        out.status.code(),
        Some(129),
        "invalid cleanup mode should exit 129: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (_h, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");

    // The mode is validated BEFORE the sequencer-control dispatch, so an invalid
    // mode fails fast even alongside `--continue` (rather than slipping through
    // to a resumed commit / a "no cherry-pick in progress" error).
    let cont = run_libra_command(
        &["cherry-pick", "--continue", "--cleanup=bogus"],
        repo.path(),
    );
    assert_eq!(
        cont.status.code(),
        Some(129),
        "invalid --cleanup with --continue should still exit 129: {}",
        String::from_utf8_lossy(&cont.stderr)
    );
    assert_eq!(
        parse_cli_error_stderr(&cont.stderr).1.error_code,
        "LBR-CLI-002"
    );
}

/// Default cherry-pick (no `-x`) must NOT append the cherry-picked-from line
/// (behavior reversal — previously always appended).
#[test]
fn cherry_pick_default_omits_cherry_picked_from_line() {
    let (repo, oid) = repo_with_feature_commit("f.txt", "feat\n", "feature work");
    assert_cli_success(
        &run_libra_command(&["cherry-pick", &oid], repo.path()),
        "cherry-pick default",
    );
    let msg = cp_head_message(repo.path());
    assert!(
        !msg.contains("(cherry picked from commit"),
        "default cherry-pick must not append the origin line, got: {msg}"
    );
    assert!(msg.contains("feature work"), "message: {msg}");
}

/// `-x` appends the cherry-picked-from line (and only once).
#[test]
fn cherry_pick_dash_x_appends_origin_line() {
    let (repo, oid) = repo_with_feature_commit("f.txt", "feat\n", "feature work");
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "-x", &oid], repo.path()),
        "cherry-pick -x",
    );
    let msg = cp_head_message(repo.path());
    let needle = format!("(cherry picked from commit {oid})");
    assert_eq!(
        msg.matches(&needle).count(),
        1,
        "origin line must appear exactly once, got: {msg}"
    );
}

/// `-s` appends a Signed-off-by trailer.
#[test]
fn cherry_pick_signoff_appends_trailer() {
    let (repo, oid) = repo_with_feature_commit("f.txt", "feat\n", "feature work");
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "-s", &oid], repo.path()),
        "cherry-pick -s",
    );
    let msg = cp_head_message(repo.path());
    assert!(
        msg.contains("Signed-off-by:"),
        "signoff trailer missing, got: {msg}"
    );
}

/// `-x -s` ordering: the cherry-picked-from line precedes Signed-off-by.
#[test]
fn cherry_pick_x_and_signoff_ordering() {
    let (repo, oid) = repo_with_feature_commit("f.txt", "feat\n", "feature work");
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "-x", "-s", &oid], repo.path()),
        "cherry-pick -x -s",
    );
    let msg = cp_head_message(repo.path());
    let x_pos = msg
        .find("(cherry picked from commit")
        .expect("origin line present");
    let s_pos = msg.find("Signed-off-by:").expect("signoff present");
    assert!(
        x_pos < s_pos,
        "cherry-picked-from must precede Signed-off-by, got: {msg}"
    );
}

/// `-n c1 c2` no longer errors and accumulates both changes into the index.
#[test]
fn cherry_pick_multiple_with_no_commit_accumulates_index() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "switch -c feature",
    );
    std::fs::write(p.join("a.txt"), "aaa\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add a");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "add a", "--no-verify"], p),
        "commit a",
    );
    let c1 = cp_rev_parse(p, "HEAD");
    std::fs::write(p.join("b.txt"), "bbb\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.txt"], p), "add b");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "add b", "--no-verify"], p),
        "commit b",
    );
    let c2 = cp_rev_parse(p, "HEAD");
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    let head_before = cp_rev_parse(p, "HEAD");

    let out = run_libra_command(&["cherry-pick", "-n", &c1, &c2], p);
    assert_cli_success(&out, "cherry-pick -n c1 c2 must not error");

    // HEAD unchanged (no commits made), both files staged.
    assert_eq!(
        cp_rev_parse(p, "HEAD"),
        head_before,
        "HEAD must not advance"
    );
    let status = run_libra_command(&["status"], p);
    let body = String::from_utf8_lossy(&status.stdout);
    assert!(body.contains("a.txt"), "a.txt staged: {body}");
    assert!(body.contains("b.txt"), "b.txt staged: {body}");
}

/// A commit whose own change set is empty is blocked without `--allow-empty`.
#[test]
fn cherry_pick_originally_empty_blocked_without_allow_empty() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "switch -c feature",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "--allow-empty", "-m", "empty feat", "--no-verify"],
            p,
        ),
        "empty feature commit",
    );
    let empty_oid = cp_rev_parse(p, "HEAD");
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");

    let out = run_libra_command(&["cherry-pick", &empty_oid], p);
    assert_eq!(out.status.code(), Some(129), "empty commit blocked");
    let (_h, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
}

/// `--allow-empty` lets an originally-empty commit through.
#[test]
fn cherry_pick_allow_empty_creates_commit() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "switch -c feature",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "--allow-empty", "-m", "empty feat", "--no-verify"],
            p,
        ),
        "empty feature commit",
    );
    let empty_oid = cp_rev_parse(p, "HEAD");
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    let head_before = cp_rev_parse(p, "HEAD");

    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--allow-empty", &empty_oid], p),
        "cherry-pick --allow-empty",
    );
    assert_ne!(
        cp_rev_parse(p, "HEAD"),
        head_before,
        "an empty commit should still create a new commit under --allow-empty"
    );
}

/// A commit that becomes redundant after replay is blocked by default, kept with
/// `--keep-redundant-commits`.
#[test]
fn cherry_pick_redundant_blocked_then_kept() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // feature adds dup.txt=same
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "switch -c feature",
    );
    std::fs::write(p.join("dup.txt"), "same\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "dup.txt"], p), "add dup");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "feat dup", "--no-verify"], p),
        "feature commit",
    );
    let feat = cp_rev_parse(p, "HEAD");
    // main independently adds the identical dup.txt=same
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    std::fs::write(p.join("dup.txt"), "same\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "dup.txt"], p), "add dup main");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main dup", "--no-verify"], p),
        "main commit",
    );
    let head_before = cp_rev_parse(p, "HEAD");

    // default: redundant → blocked, HEAD unchanged.
    let blocked = run_libra_command(&["cherry-pick", &feat], p);
    assert_eq!(blocked.status.code(), Some(129), "redundant blocked");
    let (_h, report) = parse_cli_error_stderr(&blocked.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert_eq!(
        cp_rev_parse(p, "HEAD"),
        head_before,
        "HEAD unchanged on block"
    );

    // --keep-redundant-commits: kept.
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--keep-redundant-commits", &feat], p),
        "cherry-pick --keep-redundant-commits",
    );
    assert_ne!(
        cp_rev_parse(p, "HEAD"),
        head_before,
        "redundant commit kept advances HEAD"
    );
}

/// Unsupported Git options are rejected with LBR-UNSUPPORTED-001 / exit 128.
#[test]
fn cherry_pick_unsupported_flags_rejected() {
    let (repo, oid) = repo_with_feature_commit("f.txt", "feat\n", "feature work");
    // `--rerere-autoupdate` is now honoured (it steers the rerere hook), so it is
    // no longer in this rejection list.
    let cases: Vec<Vec<&str>> = vec![vec!["cherry-pick", "--commit", &oid]];
    for args in cases {
        let out = run_libra_command(&args, repo.path());
        assert_eq!(
            out.status.code(),
            Some(128),
            "{args:?} should be unsupported: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (_h, report) = parse_cli_error_stderr(&out.stderr);
        assert_eq!(report.error_code, "LBR-UNSUPPORTED-001", "args: {args:?}");
    }
}

/// `-e` in machine mode (no TTY) degrades to the assembled message without
/// launching an editor or panicking.
#[test]
fn cherry_pick_edit_no_tty_falls_back() {
    let (repo, oid) = repo_with_feature_commit("f.txt", "feat\n", "feature work");
    let out = run_libra_command(&["cherry-pick", "--machine", "-e", &oid], repo.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "machine -e should succeed without an editor: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `--machine` emits machine JSON (NDJSON) rather than suppressing stdout.
#[test]
fn cherry_pick_machine_emits_ndjson() {
    let (repo, oid) = repo_with_feature_commit("f.txt", "feat\n", "feature work");
    let out = run_libra_command(&["cherry-pick", "--machine", &oid], repo.path());
    assert_cli_success(&out, "cherry-pick --machine");
    let json = parse_json_stdout(&out);
    assert_eq!(json["command"], "cherry-pick");
    assert_eq!(json["data"]["picked"].as_array().unwrap().len(), 1);
}

// ── Batch 1a: cherry_pick_state SQLite sequencer facade ──

/// `CherryPickState` round-trips through the SQLite `cherry_pick_state` table
/// and clears cleanly (mirrors `RebaseState`).
#[tokio::test]
#[serial]
async fn cherry_pick_state_roundtrip_persists_and_clears() {
    use std::str::FromStr;

    use git_internal::hash::ObjectHash;
    use libra::command::cherry_pick::CherryPickState;

    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let _guard = ChangeDirGuard::new(temp.path());

    assert!(
        !CherryPickState::is_in_progress().await.unwrap(),
        "a fresh repo has no in-progress cherry-pick"
    );

    let orig = ObjectHash::from_str(&"a".repeat(40)).unwrap();
    let current = ObjectHash::from_str(&"b".repeat(40)).unwrap();
    let next = ObjectHash::from_str(&"c".repeat(40)).unwrap();
    let state = CherryPickState {
        head_name: "main".to_string(),
        head_orig: orig,
        current_oid: current,
        todo: std::collections::VecDeque::from(vec![next]),
        opts_json: "{\"x\":true}".to_string(),
    };
    state.save().await.unwrap();

    assert!(CherryPickState::is_in_progress().await.unwrap());
    let loaded = CherryPickState::load()
        .await
        .unwrap()
        .expect("state present after save");
    assert_eq!(loaded.head_name, "main");
    assert_eq!(loaded.head_orig, orig);
    assert_eq!(loaded.current_oid, current);
    assert_eq!(loaded.todo, std::collections::VecDeque::from(vec![next]));
    assert_eq!(loaded.opts_json, "{\"x\":true}");

    CherryPickState::clear().await.unwrap();
    assert!(!CherryPickState::is_in_progress().await.unwrap());
    assert!(CherryPickState::load().await.unwrap().is_none());
}

// ── Batch 1b/1c: conflict sequencer (--continue/--skip/--abort/--quit) ──

/// Build a repo where cherry-picking the returned `feat` commit onto `main`
/// conflicts on `shared.txt` (base/ours/theirs all differ). HEAD on `main`.
fn conflict_repo() -> (tempfile::TempDir, String) {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("shared.txt"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base shared", "--no-verify"], p),
        "commit base",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "branch",
    );
    std::fs::write(p.join("shared.txt"), "feature side\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add feat");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "feature edit", "--no-verify"], p),
        "commit feat",
    );
    let feat = cp_rev_parse(p, "HEAD");
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    std::fs::write(p.join("shared.txt"), "main side\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add main");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main edit", "--no-verify"], p),
        "commit main",
    );
    (repo, feat)
}

/// Two-commit feature sequence onto a conflicting main: `f1` conflicts on
/// `shared.txt`, `f2` cleanly adds `extra.txt`. Returns (repo, f1, f2).
fn conflict_sequence_repo() -> (tempfile::TempDir, String, String) {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("shared.txt"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base shared", "--no-verify"], p),
        "commit base",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "branch",
    );
    std::fs::write(p.join("shared.txt"), "feature side\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add f1");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "f1 edit", "--no-verify"], p),
        "commit f1",
    );
    let f1 = cp_rev_parse(p, "HEAD");
    std::fs::write(p.join("extra.txt"), "extra\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "extra.txt"], p), "add f2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "f2 add extra", "--no-verify"], p),
        "commit f2",
    );
    let f2 = cp_rev_parse(p, "HEAD");
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    std::fs::write(p.join("shared.txt"), "main side\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add main");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main edit", "--no-verify"], p),
        "commit main",
    );
    (repo, f1, f2)
}

/// `merge.conflictStyle = diff3` is honored by cherry-pick's line-level markers
/// (parity with `libra merge` — Git honors the config for both): the base block
/// appears as `||||||| base` with the common-ancestor content (lore.md §1.3).
#[test]
fn cherry_pick_conflict_honors_diff3_style() {
    let (repo, feat) = conflict_repo();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "merge.conflictStyle", "diff3"], p),
        "set conflictStyle",
    );
    let out = run_libra_command(&["cherry-pick", &feat], p);
    assert_eq!(out.status.code(), Some(128), "conflict exit");
    let body = std::fs::read_to_string(p.join("shared.txt")).unwrap();
    assert!(
        body.contains("||||||| base\nbase\n=======\n"),
        "diff3 base block with ancestor content: {body:?}"
    );
}

/// An unsupported `merge.conflictStyle` is a hard error raised BEFORE the
/// conflicted index/worktree state is written: no markers, no persisted
/// sequencer state (a follow-up pick is NOT blocked), worktree untouched.
#[test]
fn cherry_pick_conflict_style_invalid_rejected_before_mutation() {
    let (repo, feat) = conflict_repo();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "merge.conflictStyle", "zdiff3"], p),
        "set conflictStyle",
    );
    let out = run_libra_command(&["cherry-pick", &feat], p);
    assert_eq!(out.status.code(), Some(128), "invalid style is fatal");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unsupported merge.conflictStyle 'zdiff3'"),
        "actionable error names the bad value: {stderr}"
    );
    let body = std::fs::read_to_string(p.join("shared.txt")).unwrap();
    assert_eq!(
        body, "main side\n",
        "worktree untouched — no markers, no partial reset"
    );
    // No sequencer state persisted: fixing the config lets a fresh pick proceed
    // (it conflicts normally rather than reporting an in-progress pick).
    assert_cli_success(
        &run_libra_command(&["config", "merge.conflictStyle", "merge"], p),
        "fix conflictStyle",
    );
    let retry = run_libra_command(&["cherry-pick", &feat], p);
    let retry_stderr = String::from_utf8_lossy(&retry.stderr);
    assert!(
        !retry_stderr.contains("already in progress"),
        "no stale sequencer state was left behind: {retry_stderr}"
    );
    let body = std::fs::read_to_string(p.join("shared.txt")).unwrap();
    assert!(
        body.contains("<<<<<<< HEAD"),
        "retry conflicts normally with markers: {body}"
    );
}

/// A conflict exits 128/LBR-CONFLICT-001, writes worktree markers, and persists
/// resumable state (proven by a follow-up new pick being blocked).
#[test]
fn cherry_pick_conflict_persists_state() {
    let (repo, feat) = conflict_repo();
    let p = repo.path();
    let out = run_libra_command(&["cherry-pick", &feat], p);
    assert_eq!(out.status.code(), Some(128), "conflict exit");
    let (_h, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-001");
    let body = std::fs::read_to_string(p.join("shared.txt")).unwrap();
    assert!(body.contains("<<<<<<< HEAD"), "markers: {body}");
    assert!(body.contains(">>>>>>>"), "markers: {body}");
    // A new pick is now blocked → state persisted.
    let blocked = run_libra_command(&["cherry-pick", &feat], p);
    let (_h2, report2) = parse_cli_error_stderr(&blocked.stderr);
    assert_eq!(report2.error_code, "LBR-CONFLICT-002");
}

/// An in-progress cherry-pick blocks a new `merge`.
#[test]
fn cherry_pick_in_progress_blocks_merge() {
    let (repo, feat) = conflict_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &feat], p).status.code(),
        Some(128)
    );
    let out = run_libra_command(&["merge", "feature"], p);
    let (_h, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-002", "merge blocked");
}

/// An in-progress cherry-pick blocks a new `rebase`.
#[test]
fn cherry_pick_in_progress_blocks_rebase() {
    let (repo, feat) = conflict_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &feat], p).status.code(),
        Some(128)
    );
    let out = run_libra_command(&["rebase", "feature"], p);
    let (_h, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-002", "rebase blocked");
}

/// `--abort` restores HEAD/worktree to the pre-sequence state and clears it.
#[test]
fn cherry_pick_abort_restores_head() {
    let (repo, feat) = conflict_repo();
    let p = repo.path();
    let head_before = cp_rev_parse(p, "HEAD");
    assert_eq!(
        run_libra_command(&["cherry-pick", &feat], p).status.code(),
        Some(128)
    );
    assert_cli_success(&run_libra_command(&["cherry-pick", "--abort"], p), "abort");
    assert_eq!(cp_rev_parse(p, "HEAD"), head_before, "HEAD restored");
    assert_eq!(
        std::fs::read_to_string(p.join("shared.txt")).unwrap(),
        "main side\n",
        "worktree restored, no markers"
    );
    // State cleared → a second --abort now errors with "no cherry-pick".
    let again = run_libra_command(&["cherry-pick", "--abort"], p);
    let (_h, report) = parse_cli_error_stderr(&again.stderr);
    assert_eq!(report.error_code, "LBR-REPO-003");
}

/// `--quit` clears state but leaves the conflicted worktree untouched.
#[test]
fn cherry_pick_quit_clears_state_keeps_worktree() {
    let (repo, feat) = conflict_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &feat], p).status.code(),
        Some(128)
    );
    assert_cli_success(&run_libra_command(&["cherry-pick", "--quit"], p), "quit");
    // Worktree still has the conflict markers.
    let body = std::fs::read_to_string(p.join("shared.txt")).unwrap();
    assert!(body.contains("<<<<<<< HEAD"), "markers kept: {body}");
    // A fresh pick is no longer blocked (state cleared) — it conflicts again.
    let out = run_libra_command(&["cherry-pick", &feat], p);
    let (_h, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(
        report.error_code, "LBR-CONFLICT-001",
        "not blocked, re-conflicts"
    );
}

/// `--continue` with unresolved conflicts is rejected.
#[test]
fn cherry_pick_continue_requires_resolved_index() {
    let (repo, feat) = conflict_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &feat], p).status.code(),
        Some(128)
    );
    // Do NOT resolve/add; continue must refuse.
    let out = run_libra_command(&["cherry-pick", "--continue"], p);
    assert_eq!(out.status.code(), Some(128));
    let (_h, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-001");
}

/// Resolve + add + `--continue` finishes the conflicted pick and the rest of the
/// sequence.
#[test]
fn cherry_pick_continue_resumes_sequence() {
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1, &f2], p)
            .status
            .code(),
        Some(128),
        "f1 conflicts"
    );
    // Resolve the conflict and stage it.
    std::fs::write(p.join("shared.txt"), "resolved\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "shared.txt"], p),
        "add resolved",
    );
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--continue"], p),
        "continue",
    );
    // f2 was applied and the resolution stuck.
    assert!(p.join("extra.txt").exists(), "f2 applied");
    assert_eq!(
        std::fs::read_to_string(p.join("shared.txt")).unwrap(),
        "resolved\n"
    );
    // State cleared.
    let done = run_libra_command(&["cherry-pick", "--continue"], p);
    let (_h, report) = parse_cli_error_stderr(&done.stderr);
    assert_eq!(report.error_code, "LBR-REPO-003");
}

/// `--skip` discards the conflicted commit and applies the rest.
#[test]
fn cherry_pick_skip_advances() {
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1, &f2], p)
            .status
            .code(),
        Some(128)
    );
    assert_cli_success(&run_libra_command(&["cherry-pick", "--skip"], p), "skip");
    // f1 dropped (shared.txt stays main side), f2 applied.
    assert_eq!(
        std::fs::read_to_string(p.join("shared.txt")).unwrap(),
        "main side\n",
        "f1 discarded"
    );
    assert!(p.join("extra.txt").exists(), "f2 applied after skip");
}

/// Sequencer control flags with no in-progress state error with RepoStateInvalid.
#[test]
fn cherry_pick_continue_without_state_errors() {
    let repo = create_committed_repo_via_cli();
    for flag in ["--continue", "--skip", "--abort", "--quit"] {
        let out = run_libra_command(&["cherry-pick", flag], repo.path());
        assert_eq!(out.status.code(), Some(128), "{flag} with no state");
        let (_h, report) = parse_cli_error_stderr(&out.stderr);
        assert_eq!(report.error_code, "LBR-REPO-003", "{flag}");
        assert!(
            report.message.contains("no cherry-pick in progress"),
            "{flag}: {}",
            report.message
        );
    }
}

/// `--continue --abort` together is a usage conflict. Libra remaps clap's
/// `ArgumentConflict` for a present subcommand to `command_usage` (129), not
/// clap's native exit 2.
#[test]
fn cherry_pick_continue_and_abort_clap_conflict() {
    let repo = create_committed_repo_via_cli();
    let out = run_libra_command(&["cherry-pick", "--continue", "--abort"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(129),
        "clap mutex → command_usage: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `-n c1 c2` whose sequence conflicts does NOT persist resumable state.
#[test]
fn cherry_pick_no_commit_sequence_conflict_does_not_persist_state() {
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    let out = run_libra_command(&["cherry-pick", "-n", &f1, &f2], p);
    assert_eq!(out.status.code(), Some(128), "no-commit conflict");
    let (_h, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-001");
    // No resumable state: --continue reports nothing in progress.
    let cont = run_libra_command(&["cherry-pick", "--continue"], p);
    let (_h2, report2) = parse_cli_error_stderr(&cont.stderr);
    assert_eq!(report2.error_code, "LBR-REPO-003", "no state persisted");
}

/// Resuming from a different branch than the sequence started on is rejected.
#[test]
fn cherry_pick_continue_on_wrong_branch_rejected() {
    let (repo, feat) = conflict_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &feat], p).status.code(),
        Some(128)
    );
    // Move off the sequence branch. Discard the dirty conflict worktree first
    // (`reset --hard` leaves the cherry_pick_state row intact), then switch.
    assert_cli_success(
        &run_libra_command(&["reset", "--hard", "HEAD"], p),
        "clear conflict worktree",
    );
    assert_cli_success(&run_libra_command(&["switch", "feature"], p), "switch away");
    let out = run_libra_command(&["cherry-pick", "--continue"], p);
    assert_eq!(out.status.code(), Some(128));
    let (_h, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-REPO-003", "wrong-branch rejected");
}

/// A malformed `todo` OID in the persisted state surfaces as an error, never a panic.
#[tokio::test]
#[serial]
async fn cherry_pick_malformed_todo_oid_errors_not_panics() {
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};

    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path().to_path_buf();
    // Trigger a conflict so a state row with a non-empty todo exists.
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1, &f2], &p)
            .status
            .code(),
        Some(128)
    );
    // Corrupt the persisted todo OID directly in the repo database.
    let db_url = format!("sqlite://{}?mode=rwc", p.join(".libra/libra.db").display());
    let conn = Database::connect(db_url).await.expect("connect repo db");
    // lore.md 2.6: cherry-pick state now lives in the unified `sequence_state`
    // table (kind='cherry_pick'), not the retired `cherry_pick_state` table.
    conn.execute(Statement::from_string(
        DatabaseBackend::Sqlite,
        "UPDATE sequence_state SET todo = 'not-a-valid-oid' WHERE kind = 'cherry_pick'".to_string(),
    ))
    .await
    .expect("corrupt todo");
    drop(conn);

    let out = run_libra_command(&["cherry-pick", "--continue"], &p);
    // Must fail gracefully (non-zero), not panic/crash.
    assert_eq!(out.status.code(), Some(128), "malformed todo handled");
    let (_h, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-IO-001", "read failure, not panic");
}

// ── Batch 2: -m mainline, --ff fast-forward, --strategy reject, -S gpg-sign ──

/// Build a repo with a clean (disjoint) merge commit `M` on `main` and a `target`
/// branch sitting at the common base `C0`. Cherry-picking `M` onto `target`:
///   `-m 1` brings `other_only.txt`; `-m 2` brings `main_only.txt`.
/// Returns (repo, merge_oid). HEAD left on `target`.
fn merge_commit_repo() -> (tempfile::TempDir, String) {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let c0 = cp_rev_parse(p, "HEAD");
    assert_cli_success(
        &run_libra_command(&["branch", "other", &c0], p),
        "branch other",
    );
    // main side
    std::fs::write(p.join("main_only.txt"), "m\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "main_only.txt"], p), "add main");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main edit", "--no-verify"], p),
        "commit main",
    );
    // other side
    assert_cli_success(&run_libra_command(&["switch", "other"], p), "switch other");
    std::fs::write(p.join("other_only.txt"), "o\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "other_only.txt"], p),
        "add other",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "other edit", "--no-verify"], p),
        "commit other",
    );
    // merge other into main → 2-parent merge commit
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    assert_cli_success(&run_libra_command(&["merge", "other"], p), "merge other");
    let merge_oid = cp_rev_parse(p, "HEAD");
    // target branch at the common base
    assert_cli_success(
        &run_libra_command(&["branch", "target", &c0], p),
        "branch target",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "target"], p),
        "switch target",
    );
    (repo, merge_oid)
}

/// A merge commit without `-m` is rejected (MergeCommitUnsupported / 129).
#[test]
fn cherry_pick_merge_commit_without_mainline_errors() {
    let (repo, merge_oid) = merge_commit_repo();
    let out = run_libra_command(&["cherry-pick", &merge_oid], repo.path());
    assert_eq!(out.status.code(), Some(129), "merge commit needs -m");
    let (_h, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
}

/// `-m 1` follows parent 1 (applies the *other* side's change).
#[test]
fn cherry_pick_mainline_1_applies() {
    let (repo, merge_oid) = merge_commit_repo();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "-m", "1", &merge_oid], p),
        "cherry-pick -m 1",
    );
    assert!(p.join("other_only.txt").exists(), "-m 1 applies other side");
    assert!(!p.join("main_only.txt").exists(), "-m 1 excludes main side");
}

/// `-m 2` follows parent 2 (applies the *main* side's change).
#[test]
fn cherry_pick_mainline_2_applies() {
    let (repo, merge_oid) = merge_commit_repo();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "-m", "2", &merge_oid], p),
        "cherry-pick -m 2",
    );
    assert!(p.join("main_only.txt").exists(), "-m 2 applies main side");
    assert!(
        !p.join("other_only.txt").exists(),
        "-m 2 excludes other side"
    );
}

/// `-m 3` on a 2-parent merge is out of range (CliInvalidArguments / 129).
#[test]
fn cherry_pick_mainline_out_of_range_errors() {
    let (repo, merge_oid) = merge_commit_repo();
    let out = run_libra_command(&["cherry-pick", "-m", "3", &merge_oid], repo.path());
    assert_eq!(out.status.code(), Some(129));
    let (_h, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
}

/// `-m` on a non-merge commit is rejected (CliInvalidArguments / 129).
#[test]
fn cherry_pick_mainline_on_non_merge_errors() {
    let (repo, feat) = repo_with_feature_commit("f.txt", "feat\n", "feature work");
    let out = run_libra_command(&["cherry-pick", "-m", "1", &feat], repo.path());
    assert_eq!(out.status.code(), Some(129));
    let (_h, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
}

/// `--ff` fast-forwards HEAD to a direct child without a new commit (no hash drift).
#[test]
fn cherry_pick_ff_advances_head() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let c0 = cp_rev_parse(p, "HEAD");
    std::fs::write(p.join("ff.txt"), "x\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "ff.txt"], p), "add ff");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ff child", "--no-verify"], p),
        "commit ff",
    );
    let c1 = cp_rev_parse(p, "HEAD");
    // A branch sitting at C0 (the parent of C1).
    assert_cli_success(
        &run_libra_command(&["branch", "ffbranch", &c0], p),
        "branch",
    );
    assert_cli_success(&run_libra_command(&["switch", "ffbranch"], p), "switch");

    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--ff", &c1], p),
        "cherry-pick --ff",
    );
    // HEAD advanced to C1 itself (same OID — no rewrite), and the file is present.
    assert_eq!(
        cp_rev_parse(p, "HEAD"),
        c1,
        "fast-forwarded to the picked commit"
    );
    assert!(p.join("ff.txt").exists());
}

/// `--strategy <name>` is rejected as unsupported (LBR-UNSUPPORTED-001 / 128).
#[test]
fn cherry_pick_unsupported_strategy_rejected() {
    let (repo, feat) = repo_with_feature_commit("f.txt", "feat\n", "feature work");
    let out = run_libra_command(
        &["cherry-pick", "--strategy", "recursive", &feat],
        repo.path(),
    );
    assert_eq!(out.status.code(), Some(128));
    let (_h, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-UNSUPPORTED-001");
}

/// `-S/--gpg-sign` routes through the vault signing chain (reused from merge).
/// The libra vault auto-provisions a signing key, so signing succeeds — and
/// since the code path errors when the vault yields no signature, a clean exit
/// proves the commit was actually signed; the commit carries a signature block.
#[test]
fn cherry_pick_gpg_sign_via_vault_succeeds() {
    let (repo, feat) = repo_with_feature_commit("f.txt", "feat\n", "feature work");
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "-S", &feat], repo.path()),
        "cherry-pick -S signs via vault (a clean exit proves the vault yielded a signature)",
    );
}

/// `-S` survives the conflict sequencer: commits finalized via `--continue` are
/// still signed (the `gpg_sign` option round-trips through `cherry_pick_state`).
#[test]
fn cherry_pick_continue_retains_gpg_sign() {
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    // -S sequence; f1 conflicts (no commit yet, so no signing at conflict time).
    assert_eq!(
        run_libra_command(&["cherry-pick", "-S", &f1, &f2], p)
            .status
            .code(),
        Some(128),
        "f1 conflicts"
    );
    std::fs::write(p.join("shared.txt"), "resolved\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "shared.txt"], p),
        "add resolved",
    );
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--continue"], p),
        "continue",
    );
    // HEAD = f2's resumed commit; HEAD~1 = f1's finalized commit. Both must be
    // signed — proving `gpg_sign` was not dropped on resume.
    let head_body = cp_head_message(p);
    assert!(
        head_body.contains("-----BEGIN PGP SIGNATURE-----"),
        "resumed commit must stay signed: {head_body}"
    );
    let prev = run_libra_command(&["cat-file", "-p", "HEAD~1"], p);
    let prev_body = String::from_utf8_lossy(&prev.stdout);
    assert!(
        prev_body.contains("-----BEGIN PGP SIGNATURE-----"),
        "finalized conflicted commit must stay signed: {prev_body}"
    );
}

/// A non-conflict hard error part-way through a resumed sequence leaves the
/// sequencer pointing at the failing commit (not the stale pre-resume one), so
/// a follow-up `--skip` correctly drops just that commit and finishes.
#[test]
fn cherry_pick_resume_nonconflict_error_keeps_accurate_state() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("shared.txt"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base shared", "--no-verify"], p),
        "commit base",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "branch",
    );
    // f1: conflicting edit
    std::fs::write(p.join("shared.txt"), "feature\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add f1");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "f1", "--no-verify"], p),
        "commit f1",
    );
    let f1 = cp_rev_parse(p, "HEAD");
    // f2: clean (adds extra.txt)
    std::fs::write(p.join("extra.txt"), "extra\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "extra.txt"], p), "add f2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "f2", "--no-verify"], p),
        "commit f2",
    );
    let f2 = cp_rev_parse(p, "HEAD");
    // f3: originally-empty → hard EmptyCommit error when picked without --allow-empty
    assert_cli_success(
        &run_libra_command(
            &["commit", "--allow-empty", "-m", "f3 empty", "--no-verify"],
            p,
        ),
        "commit f3 empty",
    );
    let f3 = cp_rev_parse(p, "HEAD");
    // main diverges on shared.txt
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    std::fs::write(p.join("shared.txt"), "main\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add main");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main edit", "--no-verify"], p),
        "commit main",
    );

    // Pick all three → f1 conflicts.
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1, &f2, &f3], p)
            .status
            .code(),
        Some(128),
        "f1 conflicts"
    );
    // Resolve f1 + continue → f2 applies cleanly, f3 hard-errors (empty commit).
    std::fs::write(p.join("shared.txt"), "resolved\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "shared.txt"], p),
        "add resolved",
    );
    let cont = run_libra_command(&["cherry-pick", "--continue"], p);
    assert_eq!(cont.status.code(), Some(129), "f3 empty-commit hard error");
    assert!(p.join("extra.txt").exists(), "f2 applied before f3 failed");

    // State must now point at f3 (todo empty). `--skip` drops f3 and finishes —
    // if state were stale (pointing at f1), this would mis-recover.
    assert_cli_success(&run_libra_command(&["cherry-pick", "--skip"], p), "skip f3");
    // Sequence complete → state cleared.
    let after = run_libra_command(&["cherry-pick", "--skip"], p);
    let (_h, report) = parse_cli_error_stderr(&after.stderr);
    assert_eq!(
        report.error_code, "LBR-REPO-003",
        "state cleared after skip"
    );
}

/// `--empty=<mode>` controls a pick that becomes redundant against HEAD after
/// replay: `drop` skips it (HEAD unchanged), `stop` (default) halts, `keep`
/// records the empty commit. An invalid mode is a usage error.
#[tokio::test]
#[serial]
async fn test_cherry_pick_empty_modes() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let _guard = ChangeDirGuard::new(p);

    // feature: add line X to shared.txt.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "branch feature",
    );
    std::fs::write(p.join("shared.txt"), "base\nX\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "shared.txt"], p),
        "add on feature",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "add X", "--no-verify"], p),
        "feature commit",
    );
    let feature_commit = Head::current_commit()
        .await
        .expect("feature commit")
        .to_string();

    // main: make the IDENTICAL change, so cherry-picking feature's commit is
    // redundant (the resulting tree equals HEAD's).
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    std::fs::write(p.join("shared.txt"), "base\nX\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add on main");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main also adds X", "--no-verify"], p),
        "main commit",
    );
    let main_tip = Head::current_commit().await.expect("main tip");

    // --empty=drop: skip the redundant commit; HEAD must not move, and the
    // "dropping … patch contents already upstream" notice names the real subject.
    let drop = run_libra_command(&["cherry-pick", "--empty=drop", &feature_commit], p);
    assert_cli_success(&drop, "--empty=drop succeeds");
    assert_eq!(
        Head::current_commit().await.expect("HEAD"),
        main_tip,
        "--empty=drop leaves HEAD unmoved"
    );
    let drop_out = String::from_utf8_lossy(&drop.stdout);
    assert!(
        drop_out.contains("dropping")
            && drop_out.contains("add X")
            && drop_out.contains("already upstream"),
        "--empty=drop reports the dropped commit: {drop_out}"
    );

    // --empty=stop (the default) halts with a "redundant" error.
    let stop = run_libra_command(&["cherry-pick", "--empty=stop", &feature_commit], p);
    assert_ne!(stop.status.code(), Some(0), "--empty=stop halts");
    assert!(
        String::from_utf8_lossy(&stop.stderr).contains("redundant"),
        "--empty=stop explains the redundancy"
    );

    // --empty=keep: record the empty commit; HEAD advances.
    let keep = run_libra_command(&["cherry-pick", "--empty=keep", &feature_commit], p);
    assert_cli_success(&keep, "--empty=keep succeeds");
    assert_ne!(
        Head::current_commit().await.expect("HEAD"),
        main_tip,
        "--empty=keep records the (empty) commit, advancing HEAD"
    );

    // Invalid mode is a usage error (exit 129) naming the bad value.
    let bogus = run_libra_command(&["cherry-pick", "--empty=bogus", &feature_commit], p);
    assert_eq!(
        bogus.status.code(),
        Some(129),
        "invalid --empty mode exits 129"
    );
    assert!(
        String::from_utf8_lossy(&bogus.stderr).contains("--empty"),
        "the error names --empty"
    );
}

/// `--empty=drop` survives a conflict + `--continue`: the mode round-trips through
/// the sequencer state, so a LATER commit in the sequence that becomes redundant
/// is dropped (not stopped on) when the resume reaches it.
#[test]
#[serial]
fn cherry_pick_empty_drop_survives_conflict_resume() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("shared.txt"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base shared", "--no-verify"], p),
        "commit base",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "branch feature",
    );

    // f1: conflicting edit to shared.txt.
    std::fs::write(p.join("shared.txt"), "feature\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add f1");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "f1", "--no-verify"], p),
        "commit f1",
    );
    let f1 = cp_rev_parse(p, "HEAD");

    // f2: add redundant.txt=R — main will already have the identical file, so this
    // pick becomes redundant against HEAD after f1 lands.
    std::fs::write(p.join("redundant.txt"), "R\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "redundant.txt"], p), "add f2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "f2 add R", "--no-verify"], p),
        "commit f2",
    );
    let f2 = cp_rev_parse(p, "HEAD");

    // main: conflict on shared.txt AND already add the identical redundant.txt.
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    std::fs::write(p.join("shared.txt"), "main\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "shared.txt"], p),
        "add main edit",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main edit", "--no-verify"], p),
        "commit main edit",
    );
    std::fs::write(p.join("redundant.txt"), "R\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "redundant.txt"], p),
        "add main R",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main adds R", "--no-verify"], p),
        "commit main R",
    );

    // Pick f1, f2 with --empty=drop → f1 conflicts and halts.
    assert_eq!(
        run_libra_command(&["cherry-pick", "--empty=drop", &f1, &f2], p)
            .status
            .code(),
        Some(128),
        "f1 conflicts"
    );

    // Resolve f1 and continue: f1 commits, then the resume reaches f2 — which is
    // redundant — and (because --empty=drop round-tripped through the state) drops
    // it rather than halting. The sequence completes.
    std::fs::write(p.join("shared.txt"), "resolved\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "shared.txt"], p),
        "add resolved",
    );
    let cont = run_libra_command(&["cherry-pick", "--continue"], p);
    assert_cli_success(&cont, "--continue drops the redundant f2 and finishes");
    let cont_out = String::from_utf8_lossy(&cont.stdout);
    assert!(
        cont_out.contains("dropping") && cont_out.contains("already upstream"),
        "the resumed redundant f2 is reported as dropped: {cont_out}"
    );

    // State cleared (sequence complete): another sequencer control errors.
    let after = run_libra_command(&["cherry-pick", "--continue"], p);
    let (_h, report) = parse_cli_error_stderr(&after.stderr);
    assert_eq!(
        report.error_code, "LBR-REPO-003",
        "state cleared after resume"
    );
}

/// A modify/modify conflict on one line of a multi-line file produces LINE-LEVEL
/// conflict markers (matching Git): the shared context lines stay OUTSIDE the
/// `<<<<<<< / ======= / >>>>>>>` region, which only encloses the diverging line.
/// This would fail under the old whole-file presentation (which wrapped every
/// line of each side inside the markers).
#[test]
fn cherry_pick_conflict_is_line_level() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("shared.txt"), "top\nl1\nl2\nl3\nbottom\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "branch",
    );
    std::fs::write(p.join("shared.txt"), "top\nl1\nFEATURE\nl3\nbottom\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add feat");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "feature edit", "--no-verify"], p),
        "commit feat",
    );
    let feat = cp_rev_parse(p, "HEAD");
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    std::fs::write(p.join("shared.txt"), "top\nl1\nMAIN\nl3\nbottom\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add main");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main edit", "--no-verify"], p),
        "commit main",
    );

    let out = run_libra_command(&["cherry-pick", &feat], p);
    assert_eq!(out.status.code(), Some(128), "conflict exits 128");
    let body = std::fs::read_to_string(p.join("shared.txt")).unwrap();

    // Shared context is OUTSIDE the conflict region (line-level, like Git).
    assert!(
        body.starts_with("top\nl1\n<<<<<<< HEAD\n"),
        "shared prefix precedes the markers: {body:?}"
    );
    assert!(
        body.ends_with("l3\nbottom\n"),
        "shared suffix follows the markers: {body:?}"
    );
    // The "ours" region encloses ONLY the diverging line, not the whole file.
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
    // Whole-file would have put the shared lines inside the markers.
    assert!(
        !ours.contains("top") && !ours.contains("bottom"),
        "shared lines must not be inside the conflict region: {body:?}"
    );
}

/// lore.md 2.6 symmetric mutex: an in-progress cherry-pick conflict blocks a
/// NEW merge / revert / rebase with LBR-CONFLICT-002, while the cherry-pick's
/// own --continue/--abort stay available.
#[test]
fn cherry_pick_in_progress_blocks_other_sequences() {
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path().to_path_buf();
    // Pause on a conflict.
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1, &f2], &p)
            .status
            .code(),
        Some(128),
        "cherry-pick conflicts and pauses"
    );
    // A NEW sequence of a DIFFERENT kind is refused, naming the blocking op.
    for argv in [
        vec!["merge", "feature"],
        vec!["revert", "HEAD"],
        vec!["rebase", "feature"],
    ] {
        let out = run_libra_command(&argv, &p);
        assert_eq!(
            out.status.code(),
            Some(128),
            "{argv:?} must be blocked by the in-progress cherry-pick"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("cherry-pick") && stderr.contains("LBR-CONFLICT-002"),
            "{argv:?} names the blocking op + typed code: {stderr}"
        );
    }
    // The cherry-pick's own --abort is NOT blocked.
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--abort"], &p),
        "own --abort stays available",
    );
    // After abort, a fresh sequence starts cleanly.
    let after = run_libra_command(&["revert", "HEAD", "--no-edit"], &p);
    assert_eq!(after.status.code(), Some(0), "sequence clear after abort");
}
