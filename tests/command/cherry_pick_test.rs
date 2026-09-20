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
fn cherry_pick_driver_union_resolves_overlapping_change() {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    fs::write(root.join("driver.txt"), "top\nbase\nbottom\n").unwrap();
    fs::write(root.join(".gitattributes"), "*.txt merge=union\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "driver.txt", ".gitattributes"], root),
        "add driver fixture",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "driver base", "--no-verify"], root),
        "commit driver fixture",
    );
    assert_cli_success(
        &run_libra_command(&["branch", "pick-side"], root),
        "create pick side",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "pick-side"], root),
        "checkout pick side",
    );
    fs::write(root.join("driver.txt"), "top\ntheirs\nbottom\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "driver.txt"], root),
        "add pick change",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "pick theirs", "--no-verify"], root),
        "commit pick change",
    );
    let picked = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], root).stdout)
        .trim()
        .to_string();
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], root),
        "checkout main",
    );
    fs::write(root.join("driver.txt"), "top\nours\nbottom\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "driver.txt"], root),
        "add current change",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "current ours", "--no-verify"], root),
        "commit current change",
    );

    let output = run_libra_command(&["cherry-pick", &picked], root);
    assert_cli_success(&output, "union cherry-pick");
    assert_eq!(
        fs::read_to_string(root.join("driver.txt")).unwrap(),
        "top\nours\ntheirs\nbottom\n"
    );
}

#[test]
fn cherry_pick_driver_default_union_resolves_add_add() {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "merge.default", "union"], root),
        "configure default union driver",
    );
    assert_cli_success(
        &run_libra_command(&["branch", "pick-side"], root),
        "create pick side",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "pick-side"], root),
        "checkout pick side",
    );
    fs::write(root.join("added.txt"), "theirs\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "added.txt"], root),
        "add picked file",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "pick add", "--no-verify"], root),
        "commit picked addition",
    );
    let picked = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], root).stdout)
        .trim()
        .to_string();
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], root),
        "checkout main",
    );
    fs::write(root.join("added.txt"), "ours\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "added.txt"], root),
        "add current file",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "current add", "--no-verify"], root),
        "commit current addition",
    );

    let output = run_libra_command(&["cherry-pick", &picked], root);
    assert_cli_success(&output, "default-union add/add cherry-pick");
    assert_eq!(
        fs::read_to_string(root.join("added.txt")).unwrap(),
        "ours\ntheirs\n"
    );
}

#[test]
fn cherry_pick_driver_binary_preserves_surviving_modify_delete_side() {
    for (attribute, default_driver, label) in [
        (Some("*.txt -merge\n"), None, "binary attribute"),
        (None, Some("binary"), "binary default"),
    ] {
        let repo = create_committed_repo_via_cli();
        let root = repo.path();
        fs::write(root.join("driver.txt"), "base\n").unwrap();
        let mut paths = vec!["driver.txt"];
        if let Some(attribute) = attribute {
            fs::write(root.join(".gitattributes"), attribute).unwrap();
            paths.push(".gitattributes");
        }
        assert_cli_success(
            &run_libra_command(&["add", paths[0]], root),
            "add binary-driver base",
        );
        if paths.len() == 2 {
            assert_cli_success(
                &run_libra_command(&["add", paths[1]], root),
                "add binary-driver attributes",
            );
        }
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "binary base", "--no-verify"], root),
            "commit binary-driver base",
        );
        if let Some(default_driver) = default_driver {
            assert_cli_success(
                &run_libra_command(&["config", "merge.default", default_driver], root),
                "configure default binary driver",
            );
        }
        assert_cli_success(
            &run_libra_command(&["branch", "pick-side"], root),
            "create pick side",
        );
        assert_cli_success(
            &run_libra_command(&["checkout", "pick-side"], root),
            "checkout pick side",
        );
        fs::write(root.join("driver.txt"), "picked modification\n").unwrap();
        assert_cli_success(
            &run_libra_command(&["add", "driver.txt"], root),
            "add picked modification",
        );
        assert_cli_success(
            &run_libra_command(
                &["commit", "-m", "picked modification", "--no-verify"],
                root,
            ),
            "commit picked modification",
        );
        let picked =
            String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], root).stdout)
                .trim()
                .to_string();
        assert_cli_success(
            &run_libra_command(&["checkout", "main"], root),
            "checkout main",
        );
        assert_cli_success(
            &run_libra_command(&["rm", "driver.txt"], root),
            "delete current file",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "current deletion", "--no-verify"], root),
            "commit current deletion",
        );

        let output = run_libra_command(&["cherry-pick", &picked], root);
        assert!(
            !output.status.success(),
            "{label}: modify/delete must remain conflicted"
        );
        assert_eq!(
            fs::read_to_string(root.join("driver.txt")).unwrap(),
            "picked modification\n",
            "{label}: preserve the complete surviving side without text markers"
        );
    }
}

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
#[serial(cwd)]
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
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
        ignore_other_worktrees: false,
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
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
        ignore_other_worktrees: false,
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
#[serial(cwd)]
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
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
        ignore_other_worktrees: false,
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
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
        ignore_other_worktrees: false,
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
#[serial(cwd)]
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
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
        ignore_other_worktrees: false,
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
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
        ignore_other_worktrees: false,
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
#[serial(cwd)]
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
#[serial(cwd)]
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
#[serial(cwd)]
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
#[serial(cwd)]
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
#[serial(cwd)]
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
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
        ignore_other_worktrees: false,
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
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
        ignore_other_worktrees: false,
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
pub(crate) fn cp_rev_parse(repo: &std::path::Path, rev: &str) -> String {
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

/// Raw `HEAD` commit-object bytes via `cat-file --batch` (includes `gpgsig`).
fn cp_raw_head_commit(repo: &std::path::Path) -> String {
    let out = run_libra_command_with_stdin(&["cat-file", "--batch"], repo, "HEAD\n");
    assert_cli_success(&out, "cat-file --batch HEAD");
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
#[serial(cwd)]
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
        stop_concluded: false,
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
fn cherry_pick_subject_label(oid: &str, subject: &str) -> String {
    format!("{} ({subject})", oid.chars().take(7).collect::<String>())
}

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

/// Two commits that each conflict with `main` on a distinct file. This lets a
/// resumed cherry-pick prove that its persisted options reach the later pick.
fn rerere_conflict_sequence_repo() -> (tempfile::TempDir, String, String) {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    for path in ["first.txt", "second.txt"] {
        std::fs::write(p.join(path), "base\n").unwrap();
        assert_cli_success(&run_libra_command(&["add", path], p), "add base");
    }
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base rerere files", "--no-verify"], p),
        "commit base",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "branch feature",
    );
    std::fs::write(p.join("first.txt"), "feature first\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "first.txt"], p), "add f1");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "f1", "--no-verify"], p),
        "commit f1",
    );
    let f1 = cp_rev_parse(p, "HEAD");
    std::fs::write(p.join("second.txt"), "feature second\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "second.txt"], p), "add f2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "f2", "--no-verify"], p),
        "commit f2",
    );
    let f2 = cp_rev_parse(p, "HEAD");
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    for (path, contents) in [
        ("first.txt", "main first\n"),
        ("second.txt", "main second\n"),
    ] {
        std::fs::write(p.join(path), contents).unwrap();
        assert_cli_success(&run_libra_command(&["add", path], p), "add main change");
    }
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main rerere changes", "--no-verify"], p),
        "commit main changes",
    );
    (repo, f1, f2)
}

/// `merge.conflictStyle = diff3` is honored by cherry-pick's line-level markers
/// (parity with `libra merge` — Git honors the config for both): the base block
/// is `||||||| parent of <abbrev7> (subject)` (HF-04 / ADR-HF-05 L8b).
#[test]
fn cherry_pick_conflict_honors_diff3_style() {
    let (repo, feat) = conflict_repo();
    let p = repo.path();
    let pick = cherry_pick_subject_label(&feat, "feature edit");
    assert_cli_success(
        &run_libra_command(&["config", "merge.conflictStyle", "diff3"], p),
        "set conflictStyle",
    );
    let out = run_libra_command(&["cherry-pick", &feat], p);
    assert_eq!(out.status.code(), Some(128), "conflict exit");
    let body = std::fs::read_to_string(p.join("shared.txt")).unwrap();
    assert!(
        body.contains(&format!("||||||| parent of {pick}\nbase\n=======\n")),
        "diff3 base block with ancestor content: {body:?}"
    );
}

/// MG-10 G8: cherry-pick consumes the same zdiff3 renderer as merge. The
/// ancestor section is present and the command pauses with resumable conflict
/// state instead of rejecting the now-supported style.
#[test]
fn cherry_pick_conflict_honors_zdiff3_style() {
    let (repo, feat) = conflict_repo();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "merge.conflictStyle", "zdiff3"], p),
        "set conflictStyle",
    );
    let out = run_libra_command(&["cherry-pick", &feat], p);
    assert_eq!(out.status.code(), Some(128), "conflict exit");
    let body = std::fs::read_to_string(p.join("shared.txt")).unwrap();
    let pick = cherry_pick_subject_label(&feat, "feature edit");
    assert!(
        body.contains(&format!("||||||| parent of {pick}\nbase\n=======\n")),
        "zdiff3 base block comes from the shared renderer: {body:?}"
    );
    assert!(
        body.contains("<<<<<<< HEAD") && body.contains(">>>>>>>"),
        "zdiff3 conflict retains resolvable markers: {body:?}"
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
        &run_libra_command(&["config", "merge.conflictStyle", "bogus"], p),
        "set conflictStyle",
    );
    let out = run_libra_command(&["cherry-pick", &feat], p);
    assert_eq!(out.status.code(), Some(128), "invalid style is fatal");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unsupported merge.conflictStyle 'bogus'"),
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

#[test]
fn cherry_pick_invalid_conflict_style_does_not_block_clean_content_merge() {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    fs::write(root.join("shared.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], root), "add base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], root),
        "commit base",
    );
    assert_cli_success(&run_libra_command(&["branch", "pick-side"], root), "branch");
    assert_cli_success(
        &run_libra_command(&["checkout", "pick-side"], root),
        "checkout pick side",
    );
    fs::write(root.join("shared.txt"), "one\nPICK\nthree\nfour\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], root), "add pick");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "pick", "--no-verify"], root),
        "commit pick",
    );
    let picked = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], root).stdout)
        .trim()
        .to_string();
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], root),
        "checkout main",
    );
    fs::write(root.join("shared.txt"), "one\ntwo\nthree\nMAIN\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], root), "add main");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main", "--no-verify"], root),
        "commit main",
    );
    assert_cli_success(
        &run_libra_command(&["config", "merge.conflictStyle", "bogus"], root),
        "set invalid style",
    );

    assert_cli_success(
        &run_libra_command(&["cherry-pick", &picked], root),
        "clean content merge does not render conflict markers",
    );
    assert_eq!(
        fs::read_to_string(root.join("shared.txt")).unwrap(),
        "one\nPICK\nthree\nMAIN\n"
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

#[test]
fn cherry_pick_rerere_autoupdate_flags_override_configured_staging() {
    let (repo, feat) = conflict_repo();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "rerere.enabled", "true"], p),
        "enable rerere",
    );

    // Seed the reusable resolution cache, then abandon the first sequence.
    assert_eq!(
        run_libra_command(&["cherry-pick", &feat], p).status.code(),
        Some(128),
        "first pick conflicts"
    );
    fs::write(p.join("shared.txt"), "resolved\n").expect("write resolution");
    assert_cli_success(&run_libra_command(&["rerere"], p), "record resolution");
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--abort"], p),
        "abort seed pick",
    );

    assert_cli_success(
        &run_libra_command(&["config", "rerere.autoUpdate", "true"], p),
        "configure auto staging",
    );
    assert_eq!(
        run_libra_command(&["cherry-pick", "--no-rerere-autoupdate", &feat], p)
            .status
            .code(),
        Some(128),
        "explicit off still stops on the replayed conflict"
    );
    assert_eq!(
        fs::read_to_string(p.join("shared.txt")).unwrap(),
        "resolved\n"
    );
    assert!(
        !run_libra_command(&["ls-files", "-u"], p).stdout.is_empty(),
        "explicit off must leave replayed content unstaged despite true config"
    );
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--abort"], p),
        "abort explicit-off pick",
    );

    assert_cli_success(
        &run_libra_command(&["config", "rerere.autoUpdate", "false"], p),
        "configure no auto staging",
    );
    assert_eq!(
        run_libra_command(&["cherry-pick", "--rerere-autoupdate", &feat], p)
            .status
            .code(),
        Some(128),
        "explicit on still reports the replayed conflict"
    );
    assert_eq!(
        fs::read_to_string(p.join("shared.txt")).unwrap(),
        "resolved\n"
    );
    assert!(
        run_libra_command(&["ls-files", "-u"], p).stdout.is_empty(),
        "explicit on must stage replayed content despite false config"
    );
}

#[test]
fn cherry_pick_rerere_autoupdate_off_survives_conflict_resume() {
    let (repo, f1, f2) = rerere_conflict_sequence_repo();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "rerere.enabled", "true"], p),
        "enable rerere",
    );

    // Seed both independent conflict resolutions before starting the sequence
    // under test. The second cache entry is only encountered after --continue.
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1, &f2], p)
            .status
            .code(),
        Some(128),
        "f1 seed conflict"
    );
    fs::write(p.join("first.txt"), "resolved first\n").unwrap();
    assert_cli_success(&run_libra_command(&["rerere"], p), "record f1 resolution");
    assert_cli_success(
        &run_libra_command(&["add", "first.txt"], p),
        "stage f1 seed",
    );
    assert_eq!(
        run_libra_command(&["cherry-pick", "--continue"], p)
            .status
            .code(),
        Some(128),
        "f2 seed conflict"
    );
    fs::write(p.join("second.txt"), "resolved second\n").unwrap();
    assert_cli_success(&run_libra_command(&["rerere"], p), "record f2 resolution");
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--abort"], p),
        "abort seed sequence",
    );

    assert_cli_success(
        &run_libra_command(&["config", "rerere.autoUpdate", "true"], p),
        "configure auto staging",
    );
    assert_eq!(
        run_libra_command(&["cherry-pick", "--no-rerere-autoupdate", &f1, &f2], p,)
            .status
            .code(),
        Some(128),
        "f1 replay stops unstaged"
    );
    assert_cli_success(
        &run_libra_command(&["add", "first.txt"], p),
        "manually stage f1 before the new-process continue",
    );

    // No flag is supplied to this fresh process. It must retain the original
    // explicit off choice when it reaches f2, despite config being true.
    assert_eq!(
        run_libra_command(&["cherry-pick", "--continue"], p)
            .status
            .code(),
        Some(128),
        "f2 replay stops after continue"
    );
    assert_eq!(
        fs::read_to_string(p.join("second.txt")).unwrap(),
        "resolved second\n"
    );
    assert!(
        !run_libra_command(&["ls-files", "-u"], p).stdout.is_empty(),
        "persisted explicit off must leave f2's replayed resolution unstaged"
    );
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
    conn.execute_raw(Statement::from_string(
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
        "cherry-pick -S signs via vault",
    );
    let body = cp_raw_head_commit(repo.path());
    assert!(
        body.contains("-----BEGIN PGP SIGNATURE-----"),
        "cherry-pick -S must write a signed commit: {body}"
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
    let head_body = cp_raw_head_commit(p);
    assert!(
        head_body.contains("-----BEGIN PGP SIGNATURE-----"),
        "resumed commit must stay signed: {head_body}"
    );
    let prev = run_libra_command_with_stdin(&["cat-file", "--batch"], p, "HEAD~1\n");
    assert_cli_success(&prev, "cat-file --batch HEAD~1");
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
#[serial(cwd)]
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

/// Everything an ADR-HF-04 refusal must leave untouched (GC-HF-02): index bytes,
/// HEAD, refs, unmerged entries, reflog and `sequence_state` rows, sequencer
/// sidecar files (absent vs bytes), and every worktree file outside `.libra`.
#[derive(Debug, PartialEq)]
pub(crate) struct RefusalSnapshot {
    pub(crate) index: Vec<u8>,
    pub(crate) head: String,
    pub(crate) refs: String,
    pub(crate) unmerged: String,
    pub(crate) reflog: Vec<String>,
    pub(crate) sequence_state: Vec<String>,
    pub(crate) sidecars: std::collections::BTreeMap<&'static str, Option<Vec<u8>>>,
    pub(crate) worktree: std::collections::BTreeMap<PathBuf, Vec<u8>>,
}

/// Rows of a repository database table rendered with SQLite `quote()` in rowid
/// order, so any insert, update or delete changes the result.
pub(crate) fn repo_table_rows(repo: &std::path::Path, table: &str) -> Vec<String> {
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};

    let db = repo.join(".libra/libra.db");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime");
    runtime.block_on(async {
        let conn = Database::connect(format!("sqlite://{}?mode=ro", db.display()))
            .await
            .expect("open repo db");
        let columns: Vec<String> = conn
            .query_all_raw(Statement::from_string(
                DatabaseBackend::Sqlite,
                format!("SELECT name FROM pragma_table_info('{table}')"),
            ))
            .await
            .expect("read table columns")
            .iter()
            .map(|row| row.try_get_by_index::<String>(0).expect("column name"))
            .collect();
        assert!(!columns.is_empty(), "table {table} must exist");
        let expr = columns
            .iter()
            .map(|column| format!("quote(\"{column}\")"))
            .collect::<Vec<_>>()
            .join(" || '|' || ");
        let rows = conn
            .query_all_raw(Statement::from_string(
                DatabaseBackend::Sqlite,
                format!("SELECT {expr} FROM \"{table}\" ORDER BY rowid"),
            ))
            .await
            .expect("read table rows");
        rows.iter()
            .map(|row| row.try_get_by_index::<String>(0).expect("row text"))
            .collect()
    })
}

fn worktree_files(
    root: &std::path::Path,
    dir: &std::path::Path,
    files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
) {
    for entry in std::fs::read_dir(dir).expect("read worktree dir") {
        let path = entry.expect("worktree entry").path();
        if path.file_name().is_some_and(|name| name == ".libra") {
            continue;
        }
        if path.is_dir() {
            worktree_files(root, &path, files);
        } else {
            let relative = path
                .strip_prefix(root)
                .expect("relative path")
                .to_path_buf();
            files.insert(relative, std::fs::read(&path).expect("read worktree file"));
        }
    }
}

pub(crate) fn refusal_snapshot(p: &std::path::Path) -> RefusalSnapshot {
    let text =
        |args: &[&str]| String::from_utf8_lossy(&run_libra_command(args, p).stdout).to_string();
    let index = std::fs::read(p.join(".libra/index")).expect("read index");
    let sidecars = [
        "CHERRY_PICK_MSG",
        "REVERT_EDITMSG",
        "COMMIT_EDITMSG",
        "revert-state.json",
    ]
    .into_iter()
    .map(|name| (name, std::fs::read(p.join(".libra").join(name)).ok()))
    .collect();
    let mut worktree = std::collections::BTreeMap::new();
    worktree_files(p, p, &mut worktree);
    RefusalSnapshot {
        index,
        head: text(&["rev-parse", "HEAD"]),
        refs: text(&["show-ref"]),
        unmerged: text(&["ls-files", "-u"]),
        reflog: repo_table_rows(p, "reflog"),
        sequence_state: repo_table_rows(p, "sequence_state"),
        sidecars,
        worktree,
    }
}

/// `conflict_repo` plus two clean commits on `clean` (children of `main`), then a
/// single-commit `--no-commit` pick of the conflicting feature commit. That stop is
/// M-UNMERGED U5 and leaves `shared.txt` unmerged with no sequence state.
fn unmerged_index_repo() -> (tempfile::TempDir, String, String) {
    let (repo, feat) = conflict_repo();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "clean"], p),
        "branch clean",
    );
    for (file, msg) in [("c1.txt", "clean one"), ("c2.txt", "clean two")] {
        std::fs::write(p.join(file), format!("{msg}\n")).unwrap();
        assert_cli_success(&run_libra_command(&["add", file], p), "add clean");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", msg, "--no-verify"], p),
            "commit clean",
        );
    }
    let clean1 = cp_rev_parse(p, "HEAD~1");
    let clean2 = cp_rev_parse(p, "HEAD");
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");

    // U5: the single-commit `-n` conflict is terminal and points at `libra add`.
    let stop = run_libra_command(&["cherry-pick", "-n", &feat], p);
    assert_eq!(stop.status.code(), Some(128), "no-commit conflict exit");
    let (human, report) = parse_cli_error_stderr(&stop.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-001");
    assert!(!human.contains("multi-commit"), "U5 wording: {human}");
    assert!(
        !human.contains("--continue"),
        "U5 must not point at --continue: {human}"
    );
    assert!(human.contains("libra add"), "U5 guidance: {human}");
    let unmerged =
        String::from_utf8_lossy(&run_libra_command(&["ls-files", "-u"], p).stdout).to_string();
    assert!(
        unmerged.contains("shared.txt"),
        "the fixture must leave an unmerged index: {unmerged}"
    );
    (repo, clean1, clean2)
}

/// M-UNMERGED U2-U7 (ADR-HF-04): on an unmerged index every new pick form refuses
/// to start with exit 128 / `LBR-CONFLICT-001`, names the conflicted path, and
/// writes nothing (GC-HF-02).
#[test]
fn test_cherry_pick_refuses_unmerged_index_matrix() {
    let (repo, clean1, clean2) = unmerged_index_repo();
    let p = repo.path();
    let before = refusal_snapshot(p);
    let rows: [(&str, Vec<&str>); 4] = [
        ("U2", vec!["cherry-pick", clean1.as_str()]),
        ("U3", vec!["cherry-pick", "-n", clean1.as_str()]),
        ("U4", vec!["cherry-pick", clean1.as_str(), clean2.as_str()]),
        ("U7", vec!["cherry-pick", "--ff", clean1.as_str()]),
    ];
    for (row, args) in rows {
        let out = run_libra_command(&args, p);
        assert_eq!(
            out.status.code(),
            Some(128),
            "{row} exit: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (human, report) = parse_cli_error_stderr(&out.stderr);
        assert_eq!(report.error_code, "LBR-CONFLICT-001", "{row}");
        assert!(
            human.contains("index has unmerged entries"),
            "{row}: {human}"
        );
        assert!(
            human.contains("unmerged paths: shared.txt"),
            "{row}: {human}"
        );
        assert_eq!(refusal_snapshot(p), before, "{row} must not write");
    }

    // U6: JSON mode emits only the error envelope.
    let out = run_libra_command(&["cherry-pick", "--json", &clean1], p);
    assert_eq!(out.status.code(), Some(128), "U6 exit");
    assert!(
        out.stdout.is_empty(),
        "U6 stdout must carry no data: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let (_human, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-001");
    assert_eq!(refusal_snapshot(p), before, "U6 must not write");

    // No refusal claimed a cherry-pick sequence.
    let abort = run_libra_command(&["cherry-pick", "--abort"], p);
    let (_human, report) = parse_cli_error_stderr(&abort.stderr);
    assert_eq!(report.error_code, "LBR-REPO-003", "no sequence may exist");
    assert_eq!(refusal_snapshot(p), before);
}

/// Fixture for M-UNMERGED U8-U10: `feature` adds `new.txt` (and, when
/// `conflicting`, also edits `shared.txt` against `main`); `main` then holds an
/// untracked `new.txt` that the pick would overwrite.
fn untracked_collision_repo(conflicting: bool) -> (tempfile::TempDir, String) {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let stage = |file: &str, content: &str| {
        std::fs::write(p.join(file), content).unwrap();
        assert_cli_success(&run_libra_command(&["add", file], p), "add");
    };
    let commit = |msg: &str| {
        assert_cli_success(
            &run_libra_command(&["commit", "-m", msg, "--no-verify"], p),
            "commit",
        );
    };
    stage("shared.txt", "base\n");
    commit("base shared");
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "branch",
    );
    stage("new.txt", "feature\n");
    if conflicting {
        stage("shared.txt", "feature side\n");
    }
    commit("feature adds new.txt");
    let feat = cp_rev_parse(p, "HEAD");
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    if conflicting {
        stage("shared.txt", "main side\n");
        commit("main edit");
    }
    std::fs::write(p.join("new.txt"), "mine\n").unwrap();
    (repo, feat)
}

/// M-UNMERGED U8-U10 (ADR-HF-04): a pick that would overwrite an untracked file
/// is refused with exit 128 / `LBR-CONFLICT-001` before any index, worktree, ref,
/// reflog or sequence write, on the commit, `--no-commit` and conflict paths.
#[test]
fn test_cherry_pick_refuses_untracked_overwrite_before_any_write() {
    for (row, conflicting, no_commit) in [
        ("U8", false, false),
        ("U9", false, true),
        ("U10", true, false),
    ] {
        let (repo, feat) = untracked_collision_repo(conflicting);
        let p = repo.path();
        let before = refusal_snapshot(p);
        let mut args = vec!["cherry-pick"];
        if no_commit {
            args.push("-n");
        }
        args.push(feat.as_str());
        let out = run_libra_command(&args, p);
        assert_eq!(
            out.status.code(),
            Some(128),
            "{row} exit: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let (human, report) = parse_cli_error_stderr(&out.stderr);
        assert_eq!(report.error_code, "LBR-CONFLICT-001", "{row}");
        assert!(
            human.contains("untracked working tree file would be overwritten: new.txt"),
            "{row}: {human}"
        );
        assert!(!human.contains("--continue"), "{row}: {human}");
        assert_eq!(refusal_snapshot(p), before, "{row} must not write");
        let abort = run_libra_command(&["cherry-pick", "--abort"], p);
        let (_human, report) = parse_cli_error_stderr(&abort.stderr);
        assert_eq!(
            report.error_code, "LBR-REPO-003",
            "{row}: no sequence may exist"
        );
    }
}

fn head_paths(p: &std::path::Path) -> String {
    String::from_utf8_lossy(&run_libra_command(&["ls-tree", "-r", "--name-only", "HEAD"], p).stdout)
        .to_string()
}

fn status_text(p: &std::path::Path) -> String {
    let out = run_libra_command(&["status"], p);
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// M-UNMERGED U11-U14 (ADR-HF-04): `--ff` refuses an untracked overwrite before its
/// reset; a sequence whose later commit would overwrite an untracked file stops
/// before that commit with its state saved, and `--continue` then re-attempts the
/// commit instead of recording the untouched index as it.
#[test]
fn test_cherry_pick_untracked_overwrite_on_ff_and_sequences() {
    // U11: fast-forward onto a direct child.
    let (repo, feat) = untracked_collision_repo(false);
    let p = repo.path();
    let before = refusal_snapshot(p);
    let out = run_libra_command(&["cherry-pick", "--ff", &feat], p);
    assert_eq!(
        out.status.code(),
        Some(128),
        "U11 exit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (human, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-001", "U11");
    assert!(
        human.contains("untracked working tree file would be overwritten: new.txt"),
        "U11: {human}"
    );
    assert_eq!(refusal_snapshot(p), before, "U11 must not write");

    // U12 / U13: resuming through --continue or --skip.
    for (row, verb) in [("U12", "--continue"), ("U13", "--skip")] {
        let (repo, f1, f2) = conflict_sequence_repo();
        let p = repo.path();
        assert_eq!(
            run_libra_command(&["cherry-pick", &f1, &f2], p)
                .status
                .code(),
            Some(128),
            "{row} setup"
        );
        std::fs::write(p.join("extra.txt"), "mine\n").unwrap();
        if verb == "--continue" {
            std::fs::write(p.join("shared.txt"), "resolved\n").unwrap();
            assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "resolve");
        }
        let stop = run_libra_command(&["cherry-pick", verb], p);
        assert_eq!(
            stop.status.code(),
            Some(128),
            "{row} stop: {}",
            String::from_utf8_lossy(&stop.stderr)
        );
        let (human, report) = parse_cli_error_stderr(&stop.stderr);
        assert_eq!(report.error_code, "LBR-CONFLICT-001", "{row}");
        assert!(
            human.contains("untracked working tree file would be overwritten: extra.txt"),
            "{row}: {human}"
        );
        assert!(
            human.contains("libra cherry-pick --continue"),
            "{row}: {human}"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("extra.txt")).unwrap(),
            "mine\n",
            "{row}: file kept"
        );
        assert!(
            !head_paths(p).contains("extra.txt"),
            "{row}: stopped commit not applied"
        );
        assert!(
            status_text(p).contains("cherry-pick in progress"),
            "{row}: sequence kept"
        );
        std::fs::remove_file(p.join("extra.txt")).unwrap();
        assert_cli_success(
            &run_libra_command(&["cherry-pick", "--continue"], p),
            "re-attempt",
        );
        assert!(
            head_paths(p).contains("extra.txt"),
            "{row}: --continue must apply the stopped commit"
        );
        assert!(
            !status_text(p).contains("cherry-pick in progress"),
            "{row}: sequence finished"
        );
    }

    // U14: a fresh two-commit pick whose second commit collides.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "branch",
    );
    for (file, msg) in [("c1.txt", "adds c1"), ("new.txt", "adds new")] {
        std::fs::write(p.join(file), format!("{msg}\n")).unwrap();
        assert_cli_success(&run_libra_command(&["add", file], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", msg, "--no-verify"], p),
            "commit",
        );
    }
    let c1 = cp_rev_parse(p, "HEAD~1");
    let c2 = cp_rev_parse(p, "HEAD");
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    std::fs::write(p.join("new.txt"), "mine\n").unwrap();
    let out = run_libra_command(&["cherry-pick", &c1, &c2], p);
    assert_eq!(
        out.status.code(),
        Some(128),
        "U14 exit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (human, _report) = parse_cli_error_stderr(&out.stderr);
    assert!(
        human.contains("libra cherry-pick --continue"),
        "U14: {human}"
    );
    assert!(
        head_paths(p).contains("c1.txt") && !head_paths(p).contains("new.txt"),
        "U14: stop after c1"
    );
    assert!(
        status_text(p).contains("cherry-pick in progress"),
        "U14: sequence kept"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("new.txt")).unwrap(),
        "mine\n",
        "U14: file kept"
    );
    std::fs::remove_file(p.join("new.txt")).unwrap();
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--continue"], p),
        "U14 re-attempt",
    );
    assert!(
        head_paths(p).contains("new.txt"),
        "U14: --continue must apply the stopped commit"
    );
}

/// A repository whose `feature` branch adds each file in its own commit (the
/// first on top of `main`); returns the commit ids with `main` checked out.
fn feature_commits_repo(files: &[&str]) -> (tempfile::TempDir, Vec<String>) {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "branch",
    );
    let mut ids = Vec::new();
    for file in files {
        std::fs::write(p.join(file), format!("{file}\n")).unwrap();
        assert_cli_success(&run_libra_command(&["add", file], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", &format!("adds {file}"), "--no-verify"], p),
            "commit",
        );
        ids.push(cp_rev_parse(p, "HEAD"));
    }
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    (repo, ids)
}

fn log_subjects(p: &std::path::Path) -> Vec<String> {
    String::from_utf8_lossy(&run_libra_command(&["log", "--format=%s"], p).stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn run_interrupted_after_head_move(args: &[&str], p: &std::path::Path) -> std::process::Output {
    let out = spawn_libra_command_with_env(
        args,
        p,
        &[
            ("LIBRA_TEST", "1"),
            ("LIBRA_TEST_CHERRY_PICK_FAIL_AFTER_HEAD", "1"),
        ],
    )
    .wait_with_output()
    .expect("wait for interrupted libra");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("test-injected cherry-pick interruption"),
        "failpoint must fire: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// M-UNMERGED U15-U16 (ADR-HF-04): a pick that lands moves the sequence row past
/// itself in the HEAD transaction, so an interruption right after HEAD moves
/// never makes `--continue` replay it.
#[test]
fn test_cherry_pick_interrupted_after_head_move_does_not_replay() {
    // U15: a resumed pick lands, then the run is interrupted.
    let (repo, ids) = feature_commits_repo(&["c1.txt", "new.txt", "c3.txt"]);
    let p = repo.path();
    std::fs::write(p.join("new.txt"), "mine\n").unwrap();
    let stop = run_libra_command(
        &["cherry-pick", "--empty=keep", &ids[0], &ids[1], &ids[2]],
        p,
    );
    assert_eq!(
        stop.status.code(),
        Some(128),
        "U15 stop: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
    std::fs::remove_file(p.join("new.txt")).unwrap();
    run_interrupted_after_head_move(&["cherry-pick", "--continue"], p);
    assert!(
        head_paths(p).contains("new.txt"),
        "U15: landed before the cut"
    );
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--continue"], p),
        "U15 resume",
    );
    let subjects = log_subjects(p);
    assert_eq!(
        subjects.iter().filter(|s| *s == "adds new.txt").count(),
        1,
        "U15: the landed pick must not be replayed: {subjects:?}"
    );
    assert!(head_paths(p).contains("c3.txt"), "U15: the rest applied");
    assert!(
        !status_text(p).contains("cherry-pick in progress"),
        "U15 done"
    );

    // U16: a resolved conflict is finalized, then the run is interrupted.
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", "--empty=keep", &f1, &f2], p)
            .status
            .code(),
        Some(128),
        "U16 conflict"
    );
    std::fs::write(p.join("shared.txt"), "resolved\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "resolve");
    let before = log_subjects(p).len();
    run_interrupted_after_head_move(&["cherry-pick", "--continue"], p);
    assert_eq!(log_subjects(p).len(), before + 1, "U16: resolution landed");
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--continue"], p),
        "U16 resume",
    );
    assert_eq!(
        log_subjects(p).len(),
        before + 2,
        "U16: the resolution must not be recorded twice: {:?}",
        log_subjects(p)
    );
    assert!(head_paths(p).contains("extra.txt"), "U16: f2 applied");
    assert!(
        !status_text(p).contains("cherry-pick in progress"),
        "U16 done"
    );
}

/// M-UNMERGED U17 (ADR-HF-04): a `--no-commit` run whose later commit would
/// overwrite an untracked file keeps the earlier pick staged, writes no sequence,
/// and the remaining commit can be picked again once the file is moved.
#[test]
fn test_cherry_pick_no_commit_partial_untracked_stop() {
    let (repo, ids) = feature_commits_repo(&["c1.txt", "new.txt"]);
    let p = repo.path();
    let head = cp_rev_parse(p, "HEAD");
    std::fs::write(p.join("new.txt"), "mine\n").unwrap();
    let out = run_libra_command(&["cherry-pick", "-n", &ids[0], &ids[1]], p);
    assert_eq!(
        out.status.code(),
        Some(128),
        "U17 exit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (human, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-001", "U17");
    assert!(
        human.contains("earlier picks of this '--no-commit' run stay staged"),
        "U17: {human}"
    );
    assert!(!human.contains("--continue"), "U17: {human}");
    assert_eq!(cp_rev_parse(p, "HEAD"), head, "U17: HEAD unchanged");
    assert_eq!(
        std::fs::read_to_string(p.join("new.txt")).unwrap(),
        "mine\n",
        "U17: file kept"
    );
    let staged = |p: &std::path::Path| {
        String::from_utf8_lossy(&run_libra_command(&["ls-files"], p).stdout).to_string()
    };
    let listed = staged(p);
    assert!(
        listed.lines().any(|l| l == "c1.txt") && !listed.lines().any(|l| l == "new.txt"),
        "U17: c1 staged, new.txt not: {listed}"
    );
    let abort = run_libra_command(&["cherry-pick", "--abort"], p);
    let (_human, report) = parse_cli_error_stderr(&abort.stderr);
    assert_eq!(report.error_code, "LBR-REPO-003", "U17: no sequence");

    std::fs::remove_file(p.join("new.txt")).unwrap();
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "-n", &ids[1]], p),
        "U17: pick the remaining commit again",
    );
    let listed = staged(p);
    assert!(
        listed.lines().any(|l| l == "c1.txt") && listed.lines().any(|l| l == "new.txt"),
        "U17: both picks staged: {listed}"
    );
    assert_eq!(cp_rev_parse(p, "HEAD"), head, "U17: still uncommitted");
}

fn run_interrupted_after_control_reset(args: &[&str], p: &std::path::Path) -> std::process::Output {
    let out = spawn_libra_command_with_env(
        args,
        p,
        &[
            ("LIBRA_TEST", "1"),
            ("LIBRA_TEST_CHERRY_PICK_FAIL_AFTER_CONTROL_RESET", "1"),
        ],
    )
    .wait_with_output()
    .expect("wait for interrupted libra");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("test-injected cherry-pick interruption after the control reset"),
        "failpoint must fire: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// M-CRASH X1-X4 (#477 HF-31): an interruption between a pick's HEAD move and
/// its sequence row, or between a `--skip`/`--abort` reset and its row change,
/// neither loses the remaining commits, replays a landed one, nor lets
/// `--continue` commit the reset index.
#[test]
fn test_cherry_pick_sequence_survives_interruption_matrix() {
    // X1 / X2: a fresh three-commit pick (plain, then `--ff` along a direct-child
    // chain) is interrupted right after its first commit lands.
    for (row, ff) in [("X1", false), ("X2", true)] {
        for finish in ["--continue", "--abort"] {
            let (repo, ids) = feature_commits_repo(&["c1.txt", "c2.txt", "c3.txt"]);
            let p = repo.path();
            let start = cp_rev_parse(p, "HEAD");
            let mut args = vec!["cherry-pick"];
            if ff {
                args.push("--ff");
            }
            args.extend(ids.iter().map(String::as_str));
            run_interrupted_after_head_move(&args, p);
            assert!(
                head_paths(p).contains("c1.txt"),
                "{row}: the first commit landed"
            );
            assert!(
                status_text(p).contains("cherry-pick in progress"),
                "{row}: the sequence survives the interruption"
            );
            assert_cli_success(
                &run_libra_command(&["cherry-pick", finish], p),
                &format!("{row} {finish}"),
            );
            if finish == "--continue" {
                if ff {
                    assert_eq!(
                        cp_rev_parse(p, "HEAD"),
                        ids[2],
                        "{row}: --continue keeps --ff, ending at the last commit itself"
                    );
                }
                let subjects = log_subjects(p);
                for file in ["c1.txt", "c2.txt", "c3.txt"] {
                    let subject = format!("adds {file}");
                    assert_eq!(
                        subjects.iter().filter(|s| **s == subject).count(),
                        1,
                        "{row}: {file} applied exactly once: {subjects:?}"
                    );
                }
            } else {
                assert_eq!(
                    cp_rev_parse(p, "HEAD"),
                    start,
                    "{row}: --abort restores the start"
                );
            }
            assert!(
                !status_text(p).contains("cherry-pick in progress"),
                "{row} {finish}: finished"
            );
        }
    }

    // X3: `--skip` is interrupted after resetting the conflicted pick.
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1, &f2], p)
            .status
            .code(),
        Some(128),
        "X3 conflict"
    );
    let before = log_subjects(p);
    run_interrupted_after_control_reset(&["cherry-pick", "--skip"], p);
    let refused = run_libra_command(&["cherry-pick", "--continue"], p);
    assert!(
        !refused.status.success(),
        "X3: --continue must refuse after an interrupted --skip: {}",
        String::from_utf8_lossy(&refused.stdout)
    );
    let (human, report) = parse_cli_error_stderr(&refused.stderr);
    assert_eq!(report.error_code, "LBR-REPO-003", "X3: {human}");
    assert!(human.contains("libra cherry-pick --skip"), "X3: {human}");
    assert_eq!(
        log_subjects(p),
        before,
        "X3: no commit is made from the reset index"
    );
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--skip"], p),
        "X3 re-run --skip",
    );
    assert!(head_paths(p).contains("extra.txt"), "X3: the rest applied");
    assert!(
        !log_subjects(p).iter().any(|s| s == "f1 edit"),
        "X3: the skipped commit is not recorded"
    );
    assert!(
        !status_text(p).contains("cherry-pick in progress"),
        "X3 done"
    );

    // X4: `--abort` is interrupted after resetting HEAD to the start.
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    let start = cp_rev_parse(p, "HEAD");
    assert_eq!(
        run_libra_command(&["cherry-pick", &f2, &f1], p)
            .status
            .code(),
        Some(128),
        "X4: f2 lands, then f1 conflicts"
    );
    assert_ne!(
        cp_rev_parse(p, "HEAD"),
        start,
        "X4: HEAD moved before the stop"
    );
    run_interrupted_after_control_reset(&["cherry-pick", "--abort"], p);
    let refused = run_libra_command(&["cherry-pick", "--continue"], p);
    assert!(
        !refused.status.success(),
        "X4: --continue must refuse after an interrupted --abort: {}",
        String::from_utf8_lossy(&refused.stdout)
    );
    let (human, report) = parse_cli_error_stderr(&refused.stderr);
    assert_eq!(report.error_code, "LBR-REPO-003", "X4: {human}");
    assert!(human.contains("libra cherry-pick --abort"), "X4: {human}");
    let skip = run_libra_command(&["cherry-pick", "--skip"], p);
    assert!(
        !skip.status.success(),
        "X4: --skip must refuse under an unfinished --abort: {}",
        String::from_utf8_lossy(&skip.stdout)
    );
    let (human, report) = parse_cli_error_stderr(&skip.stderr);
    assert_eq!(report.error_code, "LBR-REPO-003", "X4 --skip: {human}");
    assert!(
        human.contains("libra cherry-pick --abort"),
        "X4 --skip: {human}"
    );
    assert_eq!(
        cp_rev_parse(p, "HEAD"),
        start,
        "X4: no commit is made from the reset index"
    );
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--abort"], p),
        "X4 re-run --abort",
    );
    assert_eq!(cp_rev_parse(p, "HEAD"), start, "X4: restored");
    assert!(
        !status_text(p).contains("cherry-pick in progress"),
        "X4 done"
    );
}

fn set_sequence_payload(repo: &std::path::Path, payload: &str) {
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};

    let db = repo.join(".libra/libra.db");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build runtime");
    runtime.block_on(async {
        let conn = Database::connect(format!("sqlite://{}?mode=rw", db.display()))
            .await
            .expect("open repo db");
        let result = conn
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                "UPDATE sequence_state SET payload = ? WHERE kind = 'cherry_pick'",
                [payload.into()],
            ))
            .await
            .expect("rewrite sequence payload");
        assert_eq!(result.rows_affected(), 1, "exactly one cherry-pick row");
    });
}

/// M-CRASH X5-X8 (#477 HF-31): legacy, corrupt and marked rows; a final pick
/// dropped by `--empty=drop`; a concurrent start that claims first; and a HEAD
/// moved by hand during a non-conflict stop.
#[test]
fn test_cherry_pick_sequence_state_edge_cases() {
    // X5: rows written by binaries without the conflict flag and phase keys.
    for verb in ["--continue", "--skip", "--abort"] {
        let (repo, f1, f2) = conflict_sequence_repo();
        let p = repo.path();
        let start = cp_rev_parse(p, "HEAD");
        assert_eq!(
            run_libra_command(&["cherry-pick", &f1, &f2], p)
                .status
                .code(),
            Some(128),
            "X5 {verb} conflict"
        );
        set_sequence_payload(p, r#"{"signoff":false}"#);
        if verb == "--continue" {
            std::fs::write(p.join("shared.txt"), "resolved\n").unwrap();
            assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "resolve");
        }
        assert_cli_success(
            &run_libra_command(&["cherry-pick", verb], p),
            &format!("X5 legacy {verb}"),
        );
        match verb {
            "--continue" => {
                let subjects = log_subjects(p);
                assert_eq!(
                    subjects.iter().filter(|s| *s == "f1 edit").count(),
                    1,
                    "X5: a legacy row still finalizes the resolved commit: {subjects:?}"
                );
                assert!(head_paths(p).contains("extra.txt"), "X5: rest applied");
            }
            "--skip" => assert!(head_paths(p).contains("extra.txt"), "X5: skip continues"),
            _ => assert_eq!(cp_rev_parse(p, "HEAD"), start, "X5: abort restores"),
        }
        assert!(
            !status_text(p).contains("cherry-pick in progress"),
            "X5 legacy {verb}: finished"
        );
    }
    // X5: a corrupt payload keeps `--abort` and `--quit` usable.
    for verb in ["--abort", "--quit"] {
        let (repo, f1, f2) = conflict_sequence_repo();
        let p = repo.path();
        let start = cp_rev_parse(p, "HEAD");
        assert_eq!(
            run_libra_command(&["cherry-pick", &f1, &f2], p)
                .status
                .code(),
            Some(128),
            "X5 corrupt conflict"
        );
        set_sequence_payload(p, "not json");
        assert_cli_success(
            &run_libra_command(&["cherry-pick", verb], p),
            &format!("X5 corrupt {verb}"),
        );
        if verb == "--abort" {
            assert_eq!(cp_rev_parse(p, "HEAD"), start, "X5: corrupt abort restores");
        }
        assert!(
            !status_text(p).contains("cherry-pick in progress"),
            "X5 corrupt {verb}: cleared"
        );
    }
    // X5: `--quit` forgets a row an interrupted `--abort` marked.
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1, &f2], p)
            .status
            .code(),
        Some(128),
        "X5 marked conflict"
    );
    run_interrupted_after_control_reset(&["cherry-pick", "--abort"], p);
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--quit"], p),
        "X5 marked --quit",
    );
    assert!(
        !status_text(p).contains("cherry-pick in progress"),
        "X5 marked --quit: cleared"
    );

    // X6: the final pick of a fresh run is dropped by `--empty=drop`.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "branch",
    );
    for (file, content, msg) in [
        ("c1.txt", "c1\n", "adds c1"),
        ("same.txt", "same\n", "adds same"),
    ] {
        std::fs::write(p.join(file), content).unwrap();
        assert_cli_success(&run_libra_command(&["add", file], p), "add");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", msg, "--no-verify"], p),
            "commit",
        );
    }
    let c1 = cp_rev_parse(p, "HEAD~1");
    let c2 = cp_rev_parse(p, "HEAD");
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    std::fs::write(p.join("same.txt"), "same\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "same.txt"], p), "add same");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main has same", "--no-verify"], p),
        "commit same",
    );
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--empty=drop", &c1, &c2], p),
        "X6 pick",
    );
    assert!(
        head_paths(p).contains("c1.txt"),
        "X6: the first pick landed"
    );
    assert!(
        !status_text(p).contains("cherry-pick in progress"),
        "X6: a dropped final pick ends the sequence"
    );

    // X6: a pick dropped while `--continue` resumes the sequence also advances
    // the row, so an interruption right after the drop leaves no stale row.
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    std::fs::write(p.join("extra.txt"), "extra\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "extra.txt"], p), "add extra");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main has extra", "--no-verify"], p),
        "commit extra",
    );
    assert_eq!(
        run_libra_command(&["cherry-pick", "--empty=drop", &f1, &f2], p)
            .status
            .code(),
        Some(128),
        "X6 resume conflict"
    );
    std::fs::write(p.join("shared.txt"), "resolved\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "resolve");
    let cut = spawn_libra_command_with_env(
        &["cherry-pick", "--continue"],
        p,
        &[
            ("LIBRA_TEST", "1"),
            ("LIBRA_TEST_CHERRY_PICK_FAIL_AFTER_DROP", "1"),
        ],
    )
    .wait_with_output()
    .expect("wait for interrupted libra");
    assert!(
        String::from_utf8_lossy(&cut.stderr)
            .contains("test-injected cherry-pick interruption after a dropped pick"),
        "X6: the drop failpoint must fire: {}",
        String::from_utf8_lossy(&cut.stderr)
    );
    assert!(
        !status_text(p).contains("cherry-pick in progress"),
        "X6: the resumed drop cleared the row before the interruption"
    );

    // X7: a multi-commit run refused before anything lands releases its own
    // claim, so the refusal writes nothing and leaves no sequence.
    let (repo, ids) = feature_commits_repo(&["new.txt", "c2.txt"]);
    let p = repo.path();
    std::fs::write(p.join("new.txt"), "mine\n").unwrap();
    let before = refusal_snapshot(p);
    let mut args = vec!["cherry-pick"];
    args.extend(ids.iter().map(String::as_str));
    let out = run_libra_command(&args, p);
    assert_eq!(
        out.status.code(),
        Some(128),
        "X7 release: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        refusal_snapshot(p),
        before,
        "X7: a refused multi-commit start releases its claim and writes nothing"
    );

    // X7: when releasing that claim fails, the failure is reported instead of
    // the refusal, and the kept claim stays visible.
    let (repo, ids) = feature_commits_repo(&["new.txt", "c2.txt"]);
    let p = repo.path();
    std::fs::write(p.join("new.txt"), "mine\n").unwrap();
    let mut args = vec!["cherry-pick"];
    args.extend(ids.iter().map(String::as_str));
    let out = spawn_libra_command_with_env(
        &args,
        p,
        &[
            ("LIBRA_TEST", "1"),
            ("LIBRA_TEST_CHERRY_PICK_FAIL_RELEASE_CLAIM", "1"),
        ],
    )
    .wait_with_output()
    .expect("wait for libra");
    assert!(
        !out.status.success(),
        "X7: a failed release must not succeed"
    );
    let (human, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(
        report.error_code, "LBR-IO-002",
        "X7: a failed release is reported, not the refusal: {human}"
    );
    assert!(
        human.contains("releasing the sequence claim"),
        "X7: {human}"
    );
    assert!(
        status_text(p).contains("cherry-pick in progress"),
        "X7: the kept claim stays visible"
    );

    // X7: a concurrent `--quit` and a new start claim the sequence before a
    // refused run releases its own claim; the fenced release keeps the new row.
    let (repo, ids) = feature_commits_repo(&["new.txt", "c2.txt"]);
    let p = repo.path();
    std::fs::write(p.join("new.txt"), "mine\n").unwrap();
    let mut args = vec!["cherry-pick"];
    args.extend(ids.iter().map(String::as_str));
    let out = spawn_libra_command_with_env(
        &args,
        p,
        &[
            ("LIBRA_TEST", "1"),
            ("LIBRA_TEST_CHERRY_PICK_RECLAIM_BEFORE_RELEASE", "1"),
        ],
    )
    .wait_with_output()
    .expect("wait for libra");
    assert_eq!(
        out.status.code(),
        Some(128),
        "X7 reclaim: the refusal is still reported: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        repo_table_rows(p, "sequence_state").len(),
        1,
        "X7: the release must not erase the row a later start claimed"
    );

    // X7: a concurrent start claims the sequence before this run does.
    let (repo, ids) = feature_commits_repo(&["c1.txt", "c2.txt"]);
    let p = repo.path();
    let before = refusal_snapshot(p);
    let mut args = vec!["cherry-pick"];
    args.extend(ids.iter().map(String::as_str));
    let out = spawn_libra_command_with_env(
        &args,
        p,
        &[
            ("LIBRA_TEST", "1"),
            ("LIBRA_TEST_CHERRY_PICK_RACE_BEFORE_CLAIM", "1"),
        ],
    )
    .wait_with_output()
    .expect("wait for raced libra");
    assert!(
        !out.status.success(),
        "X7: the start that loses the claim must be refused: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let (human, report) = parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-002", "X7: {human}");
    let mut after = refusal_snapshot(p);
    assert_eq!(
        after.sequence_state.len(),
        1,
        "X7: only the competitor's row exists"
    );
    after.sequence_state = before.sequence_state.clone();
    assert_eq!(after, before, "X7: the loser writes nothing");

    // X8: HEAD moved by hand onto the stopped commit is not taken as landed.
    let (repo, ids) = feature_commits_repo(&["c1.txt", "new.txt"]);
    let p = repo.path();
    std::fs::write(p.join("new.txt"), "mine\n").unwrap();
    assert_eq!(
        run_libra_command(&["cherry-pick", &ids[0], &ids[1]], p)
            .status
            .code(),
        Some(128),
        "X8 stop"
    );
    std::fs::remove_file(p.join("new.txt")).unwrap();
    assert_cli_success(
        &run_libra_command(&["reset", "--hard", &ids[1]], p),
        "X8 move HEAD by hand",
    );
    let cont = run_libra_command(&["cherry-pick", "--continue"], p);
    assert!(
        !cont.status.success(),
        "X8: --continue must not silently skip the stopped commit: {}",
        String::from_utf8_lossy(&cont.stdout)
    );
    // The stop was on the last commit of the run, so the manual `reset --hard`
    // concluded the whole sequence (#477 HF-01, ADR-HF-03 item 1).
    assert!(
        !status_text(p).contains("cherry-pick in progress"),
        "X8: a whole-tree reset ends a sequence stopped on its last commit"
    );
}

/// M-SEQ S7a (#477 HF-01, ADR-HF-03): a whole-tree reset keeps a multi-commit
/// sequence, marks the stopped commit as concluded, and still blocks a new pick;
/// `--skip` drains the rest (HF-02 teaches `--continue` the same).
#[test]
fn test_reset_keeps_multi_pick_sequence() {
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1, &f2], p)
            .status
            .code(),
        Some(128),
        "S7a multi pick conflicts"
    );
    assert_cli_success(
        &run_libra_command(&["reset", "--hard"], p),
        "S7a reset --hard",
    );
    assert!(
        status_text(p).contains("cherry-pick in progress"),
        "S7a: the sequence survives the reset: {}",
        status_text(p)
    );
    let rows = repo_table_rows(p, "sequence_state");
    assert_eq!(rows.len(), 1, "S7a: the row is kept");
    assert!(
        rows[0].contains(r#""stop_concluded":true"#) && rows[0].contains(&f2),
        "S7a: the stopped commit is marked concluded in the payload and the todo is kept: {}",
        rows[0]
    );
    assert!(
        rows[0].contains(&f1),
        "S7a: `current_oid` still names the stopped commit, so older binaries read the row: {}",
        rows[0]
    );
    let blocked = run_libra_command(&["cherry-pick", &f1, &f2], p);
    let (human, report) = parse_cli_error_stderr(&blocked.stderr);
    assert_eq!(
        report.error_code, "LBR-CONFLICT-002",
        "S7a: a new pick is still refused: {human}"
    );
    // HF-02: `--continue` consumes the marker and applies the remaining picks.
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--continue"], p),
        "S7a continue",
    );
    assert!(
        head_paths(p).contains("extra.txt"),
        "S7a: the remaining commit is applied"
    );
    assert!(
        !status_text(p).contains("cherry-pick in progress"),
        "S7a: the sequence finished"
    );
}

/// #477 HF-01: a row whose options claim an externally concluded stop but have
/// no remaining commits is corrupt (no writer produces it). `--continue` and
/// `--skip` fail closed with `LBR-REPO-002` and change nothing, while `--abort`
/// still cleans up.
#[test]
fn test_cherry_pick_inconsistent_conclusion_marker_fails_closed() {
    // The second payload also carries a control phase: the corrupt-row check
    // must win over the "a control command is pending" one, or a concluded row
    // mid-`--skip` reports LBR-REPO-003 instead of failing closed.
    let payloads = [
        r#"{"stop_concluded":true}"#,
        r#"{"stop_concluded":true,"control_phase":"skip"}"#,
    ];
    for payload in payloads {
        for verb in ["--continue", "--skip"] {
            // A single-commit conflict stop: the row exists with an empty todo,
            // so marking it concluded is the contradiction this test drives.
            let (repo, f1, _f2) = conflict_sequence_repo();
            let p = repo.path();
            assert_eq!(
                run_libra_command(&["cherry-pick", &f1], p).status.code(),
                Some(128),
                "{verb} {payload}: the pick conflicts"
            );
            set_sequence_payload(p, payload);
            let before = repo_table_rows(p, "sequence_state");
            let out = run_libra_command(&["cherry-pick", verb], p);
            assert!(
                !out.status.success(),
                "{verb} {payload}: a corrupt row is refused"
            );
            let (human, report) = parse_cli_error_stderr(&out.stderr);
            assert_eq!(
                report.error_code, "LBR-REPO-002",
                "{verb} {payload}: {human}"
            );
            assert!(
                human.contains("libra cherry-pick --abort"),
                "{verb} {payload}: the hint names the way out: {human}"
            );
            assert_eq!(
                repo_table_rows(p, "sequence_state"),
                before,
                "{verb} {payload}: a corrupt row is left untouched"
            );
            assert_cli_success(
                &run_libra_command(&["cherry-pick", "--abort"], p),
                "--abort still cleans up",
            );
        }
    }
}

/// #477 HF-01 (Codex R4): a stopped multi-commit sequence whose options this
/// binary cannot parse must leave the row byte-identical AND warn — the marker
/// callback is fallible precisely so an unreadable payload is never reported as
/// "already marked" while the row silently survives the reset.
#[test]
fn test_reset_warns_when_the_stopped_sequence_payload_is_unreadable() {
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1, &f2], p)
            .status
            .code(),
        Some(128),
        "the multi-commit pick conflicts"
    );
    set_sequence_payload(p, "{not json");
    let before = repo_table_rows(p, "sequence_state");
    let out = run_libra_command(&["reset", "--hard"], p);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the reset itself still succeeds: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("stopped cherry-pick state could not be updated")
            && stderr.contains("libra cherry-pick --quit"),
        "the warning names the leftover state and its recovery command: {stderr}"
    );
    assert_eq!(
        repo_table_rows(p, "sequence_state"),
        before,
        "the unreadable row is left byte-identical"
    );
}

fn resolve_shared_and_commit(p: &std::path::Path, message: &str) {
    std::fs::write(p.join("shared.txt"), "resolved\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "shared.txt"], p),
        "stage the resolution",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", message, "--no-verify"], p),
        "commit the resolution",
    );
}

/// M-SEQ S3, S7b, S9b (#477 HF-29, ADR-HF-03): a real commit concludes a
/// stopped cherry-pick the same way reset does; `--dry-run` does not.
#[test]
fn test_commit_concludes_stopped_pick_matrix() {
    // S3: resolve + commit ends a stopped single-commit pick.
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1], p).status.code(),
        Some(128),
        "S3 pick conflicts"
    );
    resolve_shared_and_commit(p, "resolved pick");
    assert!(
        !status_text(p).contains("cherry-pick"),
        "S3: the concluding commit clears the pick: {}",
        status_text(p)
    );
    assert_cli_success(
        &run_libra_command(&["cherry-pick", &f2], p),
        "S3: the next pick runs",
    );

    // S7b: resolve + commit keeps a multi-commit sequence and marks the stop.
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1, &f2], p)
            .status
            .code(),
        Some(128),
        "S7b multi pick conflicts"
    );
    resolve_shared_and_commit(p, "resolved stop");
    assert!(
        status_text(p).contains("cherry-pick in progress"),
        "S7b: the sequence survives the commit: {}",
        status_text(p)
    );
    let rows = repo_table_rows(p, "sequence_state");
    assert_eq!(rows.len(), 1, "S7b: the row is kept");
    assert!(
        rows[0].contains(r#""stop_concluded":true"#) && rows[0].contains(&f2),
        "S7b: the stopped commit is marked concluded and the todo is kept: {}",
        rows[0]
    );
    assert!(
        rows[0].contains(&f1),
        "S7b: `current_oid` still names the stopped commit: {}",
        rows[0]
    );
    let blocked = run_libra_command(&["cherry-pick", &f1, &f2], p);
    let (human, report) = parse_cli_error_stderr(&blocked.stderr);
    assert_eq!(
        report.error_code, "LBR-CONFLICT-002",
        "S7b: a new pick is still refused: {human}"
    );

    // S9b: `--dry-run` changes no sequence state.
    let (repo, f1, _f2) = conflict_sequence_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1], p).status.code(),
        Some(128),
        "S9b pick conflicts"
    );
    let before = repo_table_rows(p, "sequence_state");
    assert_cli_success(
        &run_libra_command(&["commit", "--dry-run", "-m", "preview", "--no-verify"], p),
        "S9b dry-run",
    );
    assert_eq!(
        repo_table_rows(p, "sequence_state"),
        before,
        "S9b: dry-run leaves the sequence untouched"
    );
    assert!(
        status_text(p).contains("cherry-pick in progress"),
        "S9b: the pick is still in progress: {}",
        status_text(p)
    );
}

/// M-CONT C1, C2, C4, C5, C6a, C6b (#477 HF-02, ADR-HF-03).
#[test]
fn test_continue_after_external_conclusion_skips_stopped_pick_matrix() {
    // C1: reset --hard then --continue applies only the remaining pick.
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1, &f2], p)
            .status
            .code(),
        Some(128),
        "C1 pick conflicts"
    );
    assert_cli_success(&run_libra_command(&["reset", "--hard"], p), "C1 reset");
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--continue"], p),
        "C1 continue",
    );
    assert!(
        head_paths(p).contains("extra.txt"),
        "C1: the remaining commit is applied"
    );
    let subjects = log_subjects(p).join("\n");
    assert!(
        subjects.contains("f2 add extra") && subjects.contains("main edit"),
        "C1 log: {subjects}"
    );
    assert!(
        !subjects.contains("f1 edit"),
        "C1: the stopped pick is not re-committed: {subjects}"
    );
    assert!(!status_text(p).contains("cherry-pick in progress"));

    // C2: resolve + commit then --continue keeps the resolution and applies the rest.
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1, &f2], p)
            .status
            .code(),
        Some(128),
        "C2 pick conflicts"
    );
    resolve_shared_and_commit(p, "resolved");
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--continue"], p),
        "C2 continue",
    );
    let subjects = log_subjects(p).join("\n");
    assert!(
        subjects.contains("f2 add extra") && subjects.contains("resolved"),
        "C2 log: {subjects}"
    );
    assert!(
        !subjects.contains("f1 edit"),
        "C2: the stopped pick is not re-committed: {subjects}"
    );
    assert!(head_paths(p).contains("extra.txt"));
    assert!(!status_text(p).contains("cherry-pick in progress"));

    // C4: staged changes after reset make --continue refuse with no writes.
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1, &f2], p)
            .status
            .code(),
        Some(128),
        "C4 pick conflicts"
    );
    assert_cli_success(&run_libra_command(&["reset", "--hard"], p), "C4 reset");
    std::fs::write(p.join("staged.txt"), "keep me\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "staged.txt"], p), "C4 stage");
    let before_head = cp_rev_parse(p, "HEAD");
    let before_rows = repo_table_rows(p, "sequence_state");
    let blocked = run_libra_command(&["cherry-pick", "--continue"], p);
    assert_eq!(blocked.status.code(), Some(128), "C4 continue");
    let (human, report) = parse_cli_error_stderr(&blocked.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-001", "C4: {human}");
    assert!(
        human.contains("local changes would be overwritten"),
        "C4: {human}"
    );
    assert_eq!(cp_rev_parse(p, "HEAD"), before_head);
    assert_eq!(repo_table_rows(p, "sequence_state"), before_rows);
    assert!(p.join("staged.txt").exists());
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--skip"], p),
        "C4 skip still works",
    );

    // C5: the regular resolve + add + --continue path still records the stop.
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1, &f2], p)
            .status
            .code(),
        Some(128),
        "C5 pick conflicts"
    );
    std::fs::write(p.join("shared.txt"), "resolved\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "C5 add");
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--continue"], p),
        "C5 continue",
    );
    let subjects = log_subjects(p).join("\n");
    assert!(
        subjects.contains("f1 edit") && subjects.contains("f2 add extra"),
        "C5 records the original stop message: {subjects}"
    );

    // C6a: --skip after reset applies the rest.
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1, &f2], p)
            .status
            .code(),
        Some(128),
        "C6a pick conflicts"
    );
    assert_cli_success(&run_libra_command(&["reset", "--hard"], p), "C6a reset");
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--skip"], p),
        "C6a skip",
    );
    assert!(head_paths(p).contains("extra.txt"));
    assert!(!log_subjects(p).join("\n").contains("f1 edit"));
    assert!(!status_text(p).contains("cherry-pick in progress"));

    // C6b: --abort after reset restores the pre-sequence HEAD.
    let (repo, f1, f2) = conflict_sequence_repo();
    let p = repo.path();
    let orig = cp_rev_parse(p, "HEAD");
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1, &f2], p)
            .status
            .code(),
        Some(128),
        "C6b pick conflicts"
    );
    assert_cli_success(&run_libra_command(&["reset", "--hard"], p), "C6b reset");
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--abort"], p),
        "C6b abort",
    );
    assert_eq!(cp_rev_parse(p, "HEAD"), orig);
    assert!(!p.join("extra.txt").exists());
    assert!(!status_text(p).contains("cherry-pick in progress"));
}

/// M-LABEL L5 / L8b (#477 HF-04): cherry-pick labels include the subject.
#[test]
fn test_cherry_pick_conflict_label_includes_subject() {
    let (repo, feat) = conflict_repo();
    let p = repo.path();
    let pick = cherry_pick_subject_label(&feat, "feature edit");
    let parent = format!("parent of {pick}");

    let out = run_libra_command(&["cherry-pick", &feat], p);
    assert_eq!(out.status.code(), Some(128), "L5 conflict");
    let body = fs::read_to_string(p.join("shared.txt")).unwrap();
    assert!(
        body.contains("<<<<<<< HEAD\n") && body.contains(&format!(">>>>>>> {pick}\n")),
        "L5 subject form: {body}"
    );
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--abort"], p),
        "L5 abort",
    );

    assert_cli_success(
        &run_libra_command(&["config", "merge.conflictStyle", "diff3"], p),
        "L8b style",
    );
    let out = run_libra_command(&["cherry-pick", &feat], p);
    assert_eq!(out.status.code(), Some(128), "L8b conflict");
    let body = fs::read_to_string(p.join("shared.txt")).unwrap();
    assert!(
        body.contains(&format!("||||||| {parent}\n"))
            && body.contains(&format!(">>>>>>> {pick}\n")),
        "L8b parent-of base: {body}"
    );
}

/// FM-02 (M-MAT2 U1/U2): cherry-picking an executable addition materializes the
/// execute bit, and a conflicted executable file keeps it.
#[cfg(unix)]
#[test]
fn test_cherry_pick_materializes_executable_bit_including_conflicts() {
    use std::os::unix::fs::PermissionsExt;

    let repo = tempdir().expect("repo");
    let repo_path = repo.path();
    init_repo_via_cli(repo_path);
    configure_identity_via_cli(repo_path);
    fs::write(repo_path.join("base.txt"), "base\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "base.txt"], repo_path),
        "add base",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], repo_path),
        "commit base",
    );

    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], repo_path),
        "create feature",
    );
    let script = repo_path.join("run.sh");
    fs::write(&script, "#!/bin/sh\necho feature\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "run.sh"], repo_path),
        "add script",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "feature exec", "--no-verify"], repo_path),
        "commit feature",
    );

    assert_cli_success(
        &run_libra_command(&["switch", "main"], repo_path),
        "switch main",
    );
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "feature"], repo_path),
        "cherry-pick feature",
    );
    assert_eq!(
        fs::symlink_metadata(&script)
            .expect("script metadata")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "cherry-pick must materialize the execute bit"
    );

    // Conflict leg: both sides edit the same executable file.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "conflict-side"], repo_path),
        "create conflict side",
    );
    fs::write(&script, "#!/bin/sh\necho side\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "run.sh"], repo_path),
        "add side edit",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "side edit", "--no-verify"], repo_path),
        "commit side edit",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "main"], repo_path),
        "back to main",
    );
    fs::write(&script, "#!/bin/sh\necho main\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "run.sh"], repo_path),
        "add main edit",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main edit", "--no-verify"], repo_path),
        "commit main edit",
    );
    let output = run_libra_command(&["cherry-pick", "conflict-side"], repo_path);
    assert!(
        !output.status.success(),
        "cherry-pick should conflict: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::symlink_metadata(&script)
            .expect("conflicted script metadata")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "a conflicted executable file must keep the execute bit"
    );
}
