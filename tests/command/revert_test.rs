//! Tests revert command for reversing commits with and without auto-commit.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::{fs, path::PathBuf};

use libra::command::revert;
use serial_test::serial;
use tempfile::tempdir;

use super::*;

#[test]
fn test_revert_cli_outside_repository_returns_fatal_128() {
    let temp = tempdir().unwrap();
    let output = run_libra_command(&["revert", "HEAD"], temp.path());
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal: not a libra repository"),
        "unexpected stderr: {stderr}"
    );
}

/// Test basic revert functionality with file additions, modifications, and deletions
/// This test follows the workflow:
/// 1. C1: Add 1.txt with content1
/// 2. C2: Modify 1.txt (append content2)
/// 3. C3: Remove 1.txt, Add 2.txt
/// 4. Revert HEAD (C3) - should restore 1.txt and remove 2.txt
/// 5. Find C2 and revert it - should restore 1.txt to original content
#[tokio::test]
#[serial(cwd)]
async fn test_basic_revert() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    println!("===== SCENARIO 1: BASIC REVERT TEST =====");

    // --- 1. C1: Add 1.txt ---
    fs::write("1.txt", "content1").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["1.txt".to_string()],
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
        message: Some("C1: add 1.txt".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;
    println!("C1: Added 1.txt");

    // --- 2. C2: Modify 1.txt ---
    fs::write("1.txt", "content1\ncontent2").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["1.txt".to_string()],
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
        message: Some("C2: modify 1.txt".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;
    println!("C2: Modified 1.txt");

    // --- 3. C3: Remove 1.txt, Add 2.txt ---
    fs::remove_file("1.txt").unwrap();
    fs::write("2.txt", "content3").unwrap();
    add::execute(AddArgs {
        pathspec: vec![],
        all: true,
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
        message: Some("C3: remove 1.txt, add 2.txt".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;
    println!("C3: Removed 1.txt, Added 2.txt");

    // --- 4. Show initial state ---
    println!("\nBasic test repo is ready. Files before revert:");
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

    // --- 5. Test 1: Revert HEAD (C3) ---
    println!("\n--- Test 1: Revert HEAD (C3) ---");
    revert::execute(revert::RevertArgs {
        no_rerere_autoupdate: false,
        commit: vec!["HEAD".to_string()],
        no_commit: false,
        mainline: None,
        signoff: false,
        continue_revert: false,
        abort: false,
        skip: false,
        edit: false,
        no_edit: false,
        cleanup: None,
        strategy_option: Vec::new(),
    })
    .await;

    // Verify state after reverting C3
    println!("Files after reverting HEAD:");
    let files_after_revert: Vec<_> = fs::read_dir(".")
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
    for file in &files_after_revert {
        println!("{file}");
    }

    // Should have 1.txt back (modified version) and 2.txt should be gone
    assert!(
        PathBuf::from("1.txt").exists(),
        "1.txt should exist after reverting C3"
    );
    assert!(
        !PathBuf::from("2.txt").exists(),
        "2.txt should not exist after reverting C3"
    );

    // Check content of 1.txt should be the modified version
    let content = fs::read_to_string("1.txt").unwrap();
    assert_eq!(
        content, "content1\ncontent2",
        "1.txt should have modified content"
    );

    println!("Test 1 passed: HEAD revert successful");

    println!("\nAll basic revert tests passed!");
}

/// Test revert with no-commit flag
/// This test verifies that the --no-commit flag stages changes without creating a commit
#[tokio::test]
#[serial(cwd)]
async fn test_revert_no_commit() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create initial commits
    fs::write("test.txt", "original").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["test.txt".to_string()],
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
        message: Some("Add test.txt".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    fs::write("test.txt", "modified").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["test.txt".to_string()],
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
        message: Some("Modify test.txt".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    // Test revert with no-commit flag
    revert::execute(revert::RevertArgs {
        no_rerere_autoupdate: false,
        commit: vec!["HEAD".to_string()],
        no_commit: true,
        mainline: None,
        signoff: false,
        continue_revert: false,
        abort: false,
        skip: false,
        edit: false,
        no_edit: false,
        cleanup: None,
        strategy_option: Vec::new(),
    })
    .await;

    // File should be reverted but not committed
    let content = fs::read_to_string("test.txt").unwrap();
    assert_eq!(
        content, "original",
        "File should be reverted to original content"
    );

    // Check that we can still commit the staged changes
    commit::execute(CommitArgs {
        message: Some("Manual revert commit".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    println!("No-commit revert test passed");
}

/// Test reverting root commit
/// Root commits have no parents, so reverting them should create an empty repository state
#[tokio::test]
#[serial(cwd)]
async fn test_revert_root_commit() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create initial commit
    fs::write("initial.txt", "initial content").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["initial.txt".to_string()],
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
        message: Some("Initial commit".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    // Get the root commit hash - we need to implement this differently
    // since we can't call external libra command in tests
    let head = Head::current_commit()
        .await
        .expect("Should have current commit");
    let root_hash = head.to_string();

    // Revert root commit
    revert::execute(revert::RevertArgs {
        no_rerere_autoupdate: false,
        commit: vec![root_hash],
        no_commit: false,
        mainline: None,
        signoff: false,
        continue_revert: false,
        abort: false,
        skip: false,
        edit: false,
        no_edit: false,
        cleanup: None,
        strategy_option: Vec::new(),
    })
    .await;

    // All files should be removed
    let files: Vec<_> = fs::read_dir(".")
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with('.') {
                Some(name)
            } else {
                None
            }
        })
        .collect();

    assert!(
        files.is_empty(),
        "No files should exist after reverting root commit"
    );
    println!("Root commit revert test passed");
}

/// Regression: reverting a root commit (with auto-commit) must actually
/// produce an empty-tree revert commit rather than erroring out.
///
/// Previously `create_empty_revert_commit` built the empty tree via
/// `Tree::from_tree_items(Vec::new())`, which git-internal rejects with
/// "When export tree object to meta, the items is empty" (LBR-IO-002, exit
/// 128). Because the working tree is cleared *before* the commit is created,
/// the older `test_revert_root_commit` still passed even though no commit was
/// ever recorded. This test asserts on the commit itself: `execute_safe` must
/// succeed, HEAD must advance to a new commit whose parent is the root and
/// whose tree is Git's canonical empty tree.
#[tokio::test]
#[serial(cwd)]
async fn test_revert_root_commit_creates_empty_tree_commit() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    fs::write("initial.txt", "initial content").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["initial.txt".to_string()],
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
        message: Some("Initial commit".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    let root_hash = Head::current_commit()
        .await
        .expect("Should have current commit");

    // `execute_safe` surfaces the real error; before the fix this returned the
    // "items is empty" SaveObject failure instead of `Ok(())`.
    revert::execute_safe(
        revert::RevertArgs {
            no_rerere_autoupdate: false,
            commit: vec![root_hash.to_string()],
            no_commit: false,
            mainline: None,
            signoff: false,
            continue_revert: false,
            abort: false,
            skip: false,
            edit: false,
            no_edit: false,
            cleanup: None,
            strategy_option: Vec::new(),
        },
        &libra::utils::output::OutputConfig::default(),
    )
    .await
    .expect("reverting a root commit should produce an empty-tree commit");

    // HEAD must advance to a freshly created revert commit.
    let revert_hash = Head::current_commit()
        .await
        .expect("HEAD should point at the revert commit");
    assert_ne!(
        revert_hash, root_hash,
        "revert should create a new commit, not leave HEAD at the root"
    );

    let revert_commit: Commit = load_object(&revert_hash).expect("failed to load revert commit");
    assert_eq!(
        revert_commit.parent_commit_ids,
        vec![root_hash],
        "the revert commit's only parent should be the reverted root commit"
    );

    // The revert commit's tree is Git's canonical empty tree
    // (4b825dc642cb6eb9a060e54bf8d69288fbee4904 under SHA-1).
    let expected_empty_tree = ObjectHash::from_type_and_data(ObjectType::Tree, &[]);
    assert_eq!(
        revert_commit.tree_id, expected_empty_tree,
        "the revert commit should reference the canonical empty tree"
    );

    // The empty tree object must be retrievable and contain zero entries.
    let tree: Tree = load_object(&revert_commit.tree_id).expect("empty tree should be stored");
    assert!(
        tree.tree_items.is_empty(),
        "the stored revert tree should have no entries"
    );
}

#[test]
fn test_revert_json_output_reports_files_changed() {
    let repo = create_committed_repo_via_cli();
    let tracked_path = repo.path().join("tracked.txt");

    fs::write(&tracked_path, "updated\n").unwrap();
    let output = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert_cli_success(&output, "failed to stage modified tracked.txt");
    let output = run_libra_command(
        &["commit", "-m", "update tracked", "--no-verify"],
        repo.path(),
    );
    assert_cli_success(&output, "failed to commit modified tracked.txt");

    let output = run_libra_command(&["revert", "--json", "HEAD"], repo.path());
    assert_cli_success(&output, "revert --json should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "revert");
    assert_eq!(json["data"]["no_commit"], false);
    assert_eq!(json["data"]["files_changed"], 1);
    assert!(json["data"]["reverted_commit"].as_str().is_some());
    assert!(json["data"]["new_commit"].as_str().is_some());
    assert_eq!(
        fs::read_to_string(&tracked_path).unwrap(),
        "tracked\n",
        "revert should restore the previous file content"
    );
}

#[test]
fn test_revert_no_rerere_autoupdate_is_accepted_noop() {
    let repo = create_committed_repo_via_cli();
    let tracked_path = repo.path().join("tracked.txt");
    fs::write(&tracked_path, "updated\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], repo.path()),
        "stage modified tracked.txt",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "update", "--no-verify"], repo.path()),
        "commit modified tracked.txt",
    );

    // `--no-rerere-autoupdate` is accepted and a no-op: Libra has no rerere, so
    // the revert proceeds and creates a revert commit normally.
    let out = run_libra_command(&["revert", "--no-rerere-autoupdate", "HEAD"], repo.path());
    assert_cli_success(&out, "revert --no-rerere-autoupdate HEAD");
}

#[test]
fn test_revert_signoff_adds_trailer() {
    let repo = create_committed_repo_via_cli();
    let tracked_path = repo.path().join("tracked.txt");

    fs::write(&tracked_path, "updated\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], repo.path()),
        "stage modified tracked.txt",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "update tracked", "--no-verify"],
            repo.path(),
        ),
        "commit modified tracked.txt",
    );

    let out = run_libra_command(&["revert", "-s", "HEAD"], repo.path());
    assert_cli_success(&out, "revert -s HEAD");

    // The revert commit message should carry the Signed-off-by trailer.
    let show = run_libra_command(&["cat-file", "-p", "HEAD"], repo.path());
    assert_cli_success(&show, "cat-file -p HEAD");
    let body = String::from_utf8_lossy(&show.stdout);
    assert!(
        body.contains("Signed-off-by:"),
        "revert -s should append a Signed-off-by trailer: {body}"
    );
    assert!(
        body.contains("This reverts commit"),
        "revert message body should be present: {body}"
    );
}

#[test]
fn test_revert_multiple_commits_in_one_invocation() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    fs::write(p.join("a.txt"), "a\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add a");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1 add a", "--no-verify"], p),
        "commit c1",
    );
    let c1 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();

    fs::write(p.join("b.txt"), "b\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.txt"], p), "add b");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2 add b", "--no-verify"], p),
        "commit c2",
    );
    let c2 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();

    // Revert both commits in one invocation (newest first).
    let out = run_libra_command(&["revert", c2.as_str(), c1.as_str()], p);
    assert_cli_success(&out, "revert c2 c1");
    assert!(
        !p.join("b.txt").exists(),
        "reverting c2 should remove b.txt"
    );
    assert!(
        !p.join("a.txt").exists(),
        "reverting c1 should remove a.txt"
    );
}

#[test]
fn test_revert_multiple_commits_rejects_no_commit() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // --no-commit with multiple commits needs the sequencer; it is rejected.
    let out = run_libra_command(&["revert", "--no-commit", "HEAD", "HEAD~1"], p);
    assert!(
        !out.status.success(),
        "revert --no-commit with multiple commits should be rejected"
    );
}

/// Build a repo where reverting `c2` conflicts with a later change in `c3`,
/// returning (repo, c2_hash).
fn setup_revert_conflict() -> (tempfile::TempDir, String) {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("f.txt"), "line1\nline2\nline3\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add c1");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    fs::write(p.join("f.txt"), "line1\nCHANGED\nline3\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add c2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );
    let c2 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    fs::write(p.join("f.txt"), "line1\nDIVERGED\nline3\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add c3");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c3", "--no-verify"], p),
        "commit c3",
    );
    (repo, c2)
}

#[test]
fn test_revert_conflict_then_continue() {
    let (repo, c2) = setup_revert_conflict();
    let p = repo.path();

    // Reverting c2 conflicts with c3's overlapping change.
    let out = run_libra_command(&["revert", c2.as_str()], p);
    assert!(
        !out.status.success(),
        "conflicting revert should fail and pause"
    );
    assert!(
        p.join(".libra/revert-state.json").exists(),
        "revert state should be recorded"
    );
    assert!(
        fs::read_to_string(p.join("f.txt"))
            .unwrap()
            .contains("<<<<<<<"),
        "worktree should carry conflict markers"
    );

    // Resolve and continue.
    fs::write(p.join("f.txt"), "line1\nRESOLVED\nline3\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add resolved");
    let cont = run_libra_command(&["revert", "--continue"], p);
    assert_cli_success(&cont, "revert --continue");
    assert!(
        !p.join(".libra/revert-state.json").exists(),
        "state should be cleared after --continue"
    );
    assert_eq!(
        fs::read_to_string(p.join("f.txt")).unwrap(),
        "line1\nRESOLVED\nline3\n"
    );
}

#[test]
fn test_revert_conflict_then_abort() {
    let (repo, c2) = setup_revert_conflict();
    let p = repo.path();

    let out = run_libra_command(&["revert", c2.as_str()], p);
    assert!(!out.status.success(), "conflicting revert should pause");
    assert!(p.join(".libra/revert-state.json").exists());

    let ab = run_libra_command(&["revert", "--abort"], p);
    assert_cli_success(&ab, "revert --abort");
    assert!(
        !p.join(".libra/revert-state.json").exists(),
        "state should be cleared after --abort"
    );
    assert_eq!(
        fs::read_to_string(p.join("f.txt")).unwrap(),
        "line1\nDIVERGED\nline3\n",
        "--abort should restore the pre-revert content"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_revert_json_output_skips_noop_paths_in_files_changed() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    fs::write("added.txt", "temporary\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "added.txt"], repo.path()),
        "failed to stage added.txt",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "add temporary", "--no-verify"],
            repo.path(),
        ),
        "failed to commit added.txt",
    );
    let added_commit = Head::current_commit()
        .await
        .expect("expected added.txt commit");

    fs::remove_file("added.txt").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "-A"], repo.path()),
        "failed to stage added.txt removal",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "remove temporary", "--no-verify"],
            repo.path(),
        ),
        "failed to commit added.txt removal",
    );

    let output = run_libra_command(
        &["revert", "--json", &added_commit.to_string()],
        repo.path(),
    );
    assert_cli_success(
        &output,
        "revert of already-removed add commit should succeed",
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "revert");
    assert_eq!(json["data"]["files_changed"], 0);
    assert!(json["data"]["new_commit"].as_str().is_some());
    assert!(
        !repo.path().join("added.txt").exists(),
        "reverting an already-undone add should keep the file absent"
    );
}

/// Test error cases for revert command
/// This ensures the command handles invalid input gracefully
#[tokio::test]
#[serial(cwd)]
async fn test_revert_errors() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Test reverting non-existent commit should fail gracefully
    revert::execute(revert::RevertArgs {
        no_rerere_autoupdate: false,
        commit: vec!["nonexistent".to_string()],
        no_commit: false,
        mainline: None,
        signoff: false,
        continue_revert: false,
        abort: false,
        skip: false,
        edit: false,
        no_edit: false,
        cleanup: None,
        strategy_option: Vec::new(),
    })
    .await;

    println!("Error handling test completed");
}

// ---------------------------------------------------------------------------
// Merge-commit revert via -m/--mainline.
// ---------------------------------------------------------------------------

fn commit_file_revert(repo: &std::path::Path, file: &str, content: &str, msg: &str) {
    fs::write(repo.join(file), content).expect("write file");
    assert_cli_success(&run_libra_command(&["add", file], repo), "add file");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", msg, "--no-verify"], repo),
        "commit file",
    );
}

/// HEAD on main is a 2-parent merge of `feature` (added feature.txt) into main
/// (added mainfile.txt): parent 1 = main pre-merge, parent 2 = feature.
fn build_revert_merge_repo() -> tempfile::TempDir {
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
    commit_file_revert(p, "feature.txt", "feature\n", "feature");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], p),
        "checkout main",
    );
    commit_file_revert(p, "mainfile.txt", "main\n", "main change");
    assert_cli_success(
        &run_libra_command(&["merge", "feature"], p),
        "merge feature",
    );
    repo
}

#[test]
fn test_revert_merge_without_mainline_errors_128() {
    let repo = build_revert_merge_repo();
    let out = run_libra_command(&["revert", "HEAD"], repo.path());
    assert_eq!(out.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("is a merge but no -m option was given"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn test_revert_merge_with_mainline_removes_feature_side() {
    let repo = build_revert_merge_repo();
    let p = repo.path();
    assert!(p.join("feature.txt").exists());
    assert!(p.join("mainfile.txt").exists());
    let out = run_libra_command(&["revert", "-m", "1", "HEAD"], p);
    assert_cli_success(&out, "revert -m 1 HEAD");
    // Reverting relative to parent 1 (main pre-merge) undoes feature's addition.
    assert!(
        !p.join("feature.txt").exists(),
        "feature.txt should be reverted away"
    );
    assert!(p.join("mainfile.txt").exists(), "mainfile.txt stays");
}

#[test]
fn test_revert_mainline_on_non_merge_errors_128() {
    let repo = create_committed_repo_via_cli();
    let out = run_libra_command(&["revert", "-m", "1", "HEAD"], repo.path());
    assert_eq!(out.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("is not a merge"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn test_revert_mainline_out_of_range_errors_128() {
    let repo = build_revert_merge_repo();
    let out = run_libra_command(&["revert", "-m", "5", "HEAD"], repo.path());
    assert_eq!(out.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("does not have a parent number 5"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn revert_no_edit_is_accepted() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("rev.txt"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "rev.txt"], p), "add base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    std::fs::write(p.join("rev.txt"), "change\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "rev.txt"], p), "stage change");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "the change", "--no-verify"], p),
        "commit change",
    );

    // `--no-edit` is accepted (Libra never opens an editor for revert) and the
    // revert is applied normally.
    let revert = run_libra_command(&["revert", "HEAD", "--no-edit"], p);
    assert_cli_success(&revert, "revert HEAD --no-edit");
    assert_eq!(
        std::fs::read_to_string(p.join("rev.txt")).unwrap(),
        "base\n",
        "revert restored the file content"
    );
}

/// `revert --edit` opens the configured editor on the generated revert message
/// and commits the edited result; `--edit` and `--no-edit` are mutually
/// exclusive. (Uses `core.editor` so no process-global env is touched.)
#[test]
fn test_revert_edit_opens_editor() {
    let repo = tempdir().expect("repo dir");
    let p = repo.path();
    assert!(run_libra_command(&["init"], p).status.success(), "init");
    run_libra_command(&["config", "set", "user.name", "t"], p);
    run_libra_command(&["config", "set", "user.email", "t@t"], p);
    fs::write(p.join("f.txt"), "one\n").expect("write f");
    assert!(
        run_libra_command(&["add", "f.txt"], p).status.success(),
        "add"
    );
    assert!(
        run_libra_command(&["commit", "-m", "first", "--no-verify"], p)
            .status
            .success(),
        "commit first"
    );
    fs::write(p.join("f.txt"), "two\n").expect("modify f");
    assert!(
        run_libra_command(&["add", "f.txt"], p).status.success(),
        "add 2"
    );
    assert!(
        run_libra_command(&["commit", "-m", "second", "--no-verify"], p)
            .status
            .success(),
        "commit second"
    );

    // An editor script that replaces the revert message with a fixed line.
    let editor = p.join("fake-editor.sh");
    fs::write(
        &editor,
        "#!/bin/sh\necho 'EDITED revert subject' > \"$1\"\n",
    )
    .expect("write editor");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&editor, fs::Permissions::from_mode(0o755)).expect("chmod editor");
    }
    run_libra_command(
        &["config", "set", "core.editor", editor.to_str().unwrap()],
        p,
    );

    let out = run_libra_command(&["revert", "HEAD", "--edit"], p);
    assert!(
        out.status.success(),
        "revert --edit should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let subject = run_libra_command(&["log", "-1", "--pretty=%s"], p);
    assert_eq!(
        String::from_utf8_lossy(&subject.stdout).trim(),
        "EDITED revert subject",
        "the edited message is committed"
    );

    // `--edit` and `--no-edit` are mutually exclusive (clap conflict).
    let conflict = run_libra_command(&["revert", "HEAD", "--edit", "--no-edit"], p);
    assert!(
        !conflict.status.success(),
        "--edit conflicts with --no-edit"
    );
}

/// `revert --edit` is carried through a conflict: after resolving and running
/// `revert --continue`, the editor opens again (via `RevertState.edit`) and the
/// edited message is committed.
#[test]
fn test_revert_edit_carried_through_continue() {
    let repo = tempdir().expect("repo dir");
    let p = repo.path();
    assert!(run_libra_command(&["init"], p).status.success(), "init");
    run_libra_command(&["config", "set", "user.name", "t"], p);
    run_libra_command(&["config", "set", "user.email", "t@t"], p);
    let commit = |msg: &str, body: &str| {
        fs::write(p.join("f.txt"), body).expect("write f");
        assert!(
            run_libra_command(&["add", "f.txt"], p).status.success(),
            "add"
        );
        assert!(
            run_libra_command(&["commit", "-m", msg, "--no-verify"], p)
                .status
                .success(),
            "commit {msg}"
        );
    };
    commit("c1", "a\nb\nc\n");
    commit("c2", "a\nB\nc\n"); // changes line 2
    commit("c3", "a\nZ\nc\n"); // changes line 2 again → reverting c2 will conflict

    let editor = p.join("fake-editor.sh");
    fs::write(&editor, "#!/bin/sh\necho 'EDITED via continue' > \"$1\"\n").expect("write editor");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&editor, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    run_libra_command(
        &["config", "set", "core.editor", editor.to_str().unwrap()],
        p,
    );

    // Reverting c2 (HEAD~1) conflicts with c3's change to line 2.
    let conflicted = run_libra_command(&["revert", "HEAD~1", "--edit"], p);
    assert!(
        !conflicted.status.success(),
        "reverting HEAD~1 should conflict: {}",
        String::from_utf8_lossy(&conflicted.stdout)
    );

    // Resolve and continue: the editor opens (RevertState carried `--edit`).
    fs::write(p.join("f.txt"), "a\nRESOLVED\nc\n").expect("resolve");
    assert!(
        run_libra_command(&["add", "f.txt"], p).status.success(),
        "add resolved"
    );
    let cont = run_libra_command(&["revert", "--continue"], p);
    assert!(
        cont.status.success(),
        "revert --continue should succeed: {}",
        String::from_utf8_lossy(&cont.stderr)
    );
    let subject = run_libra_command(&["log", "-1", "--pretty=%s"], p);
    assert_eq!(
        String::from_utf8_lossy(&subject.stdout).trim(),
        "EDITED via continue",
        "the edited message is committed after --continue"
    );
}

/// A failing/empty editor on a CLEAN `revert --edit` must leave the working
/// tree and HEAD unchanged (the message is resolved before the worktree is
/// mutated), and must NOT leave a stray in-progress revert.
#[test]
fn test_revert_edit_failure_leaves_worktree_clean() {
    let repo = tempdir().expect("repo dir");
    let p = repo.path();
    assert!(run_libra_command(&["init"], p).status.success(), "init");
    run_libra_command(&["config", "set", "user.name", "t"], p);
    run_libra_command(&["config", "set", "user.email", "t@t"], p);
    fs::write(p.join("f.txt"), "one\n").expect("write f");
    assert!(
        run_libra_command(&["add", "f.txt"], p).status.success(),
        "add"
    );
    assert!(
        run_libra_command(&["commit", "-m", "first", "--no-verify"], p)
            .status
            .success(),
        "commit first"
    );
    fs::write(p.join("f.txt"), "two\n").expect("modify f");
    assert!(
        run_libra_command(&["add", "f.txt"], p).status.success(),
        "add 2"
    );
    assert!(
        run_libra_command(&["commit", "-m", "second", "--no-verify"], p)
            .status
            .success(),
        "commit second"
    );

    // An editor that exits non-zero (failure).
    let editor = p.join("bad-editor.sh");
    fs::write(&editor, "#!/bin/sh\nexit 1\n").expect("write editor");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&editor, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    run_libra_command(
        &["config", "set", "core.editor", editor.to_str().unwrap()],
        p,
    );

    let out = run_libra_command(&["revert", "HEAD", "--edit"], p);
    assert!(!out.status.success(), "a failing editor aborts the revert");
    // The working tree is untouched (revert was not applied), HEAD is unchanged,
    // and there is no in-progress revert to clean up.
    assert_eq!(
        fs::read_to_string(p.join("f.txt")).unwrap(),
        "two\n",
        "worktree unchanged"
    );
    assert!(
        !p.join(".libra/revert-state.json").exists(),
        "no stray revert state on a clean-path editor failure"
    );
    let subject = run_libra_command(&["log", "-1", "--pretty=%s"], p);
    assert_eq!(
        String::from_utf8_lossy(&subject.stdout).trim(),
        "second",
        "HEAD unchanged (no revert commit)"
    );
}

/// A failing editor during `revert --continue` must leave `revert-state.json`
/// in place so the revert stays recoverable (`--abort`/retry).
#[test]
fn test_revert_edit_failure_during_continue_keeps_state() {
    let repo = tempdir().expect("repo dir");
    let p = repo.path();
    assert!(run_libra_command(&["init"], p).status.success(), "init");
    run_libra_command(&["config", "set", "user.name", "t"], p);
    run_libra_command(&["config", "set", "user.email", "t@t"], p);
    let commit = |msg: &str, body: &str| {
        fs::write(p.join("f.txt"), body).expect("write f");
        assert!(
            run_libra_command(&["add", "f.txt"], p).status.success(),
            "add"
        );
        assert!(
            run_libra_command(&["commit", "-m", msg, "--no-verify"], p)
                .status
                .success(),
            "commit {msg}"
        );
    };
    commit("c1", "a\nb\nc\n");
    commit("c2", "a\nB\nc\n");
    commit("c3", "a\nZ\nc\n");

    let editor = p.join("bad-editor.sh");
    fs::write(&editor, "#!/bin/sh\nexit 1\n").expect("write editor");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&editor, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    run_libra_command(
        &["config", "set", "core.editor", editor.to_str().unwrap()],
        p,
    );

    // Conflicting revert with --edit (editor not reached yet at conflict time).
    let conflicted = run_libra_command(&["revert", "HEAD~1", "--edit"], p);
    assert!(!conflicted.status.success(), "revert should conflict");
    assert!(
        p.join(".libra/revert-state.json").exists(),
        "conflict records state"
    );

    // Resolve, then --continue: the editor runs and FAILS.
    fs::write(p.join("f.txt"), "a\nRESOLVED\nc\n").expect("resolve");
    assert!(
        run_libra_command(&["add", "f.txt"], p).status.success(),
        "add resolved"
    );
    let cont = run_libra_command(&["revert", "--continue"], p);
    assert!(!cont.status.success(), "a failing editor aborts --continue");
    // State persists so the user can retry or --abort.
    assert!(
        p.join(".libra/revert-state.json").exists(),
        "revert state remains after a failed --continue editor"
    );
}

/// Build a repo where reverting `c2` conflicts (f.txt's middle line is changed
/// again by c4 after c2) but `c3` (which adds g.txt) reverts cleanly. Returns
/// `(c2_hash, c3_hash)`.
fn setup_conflict_then_clean(p: &std::path::Path) -> (String, String) {
    use super::run_libra_command;
    assert!(run_libra_command(&["init"], p).status.success(), "init");
    run_libra_command(&["config", "set", "user.name", "t"], p);
    run_libra_command(&["config", "set", "user.email", "t@t"], p);
    let commit = |body: Option<&str>, g: Option<&str>, msg: &str| {
        if let Some(b) = body {
            fs::write(p.join("f.txt"), b).expect("write f");
            run_libra_command(&["add", "f.txt"], p);
        }
        if let Some(gc) = g {
            fs::write(p.join("g.txt"), gc).expect("write g");
            run_libra_command(&["add", "g.txt"], p);
        }
        assert!(
            run_libra_command(&["commit", "-m", msg, "--no-verify"], p)
                .status
                .success(),
            "commit {msg}"
        );
    };
    commit(Some("L1\nL2\nL3\n"), None, "c1");
    commit(Some("L1\nL2mod\nL3\n"), None, "c2");
    let c2 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    commit(None, Some("g1\n"), "c3");
    let c3 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    commit(Some("L1\nL2again\nL3\n"), None, "c4");
    (c2, c3)
}

/// `revert <c2> <c3>` where c2 conflicts: after resolving and `--continue`, the
/// remaining commit c3 must ALSO be reverted (regression: previously the pending
/// commits behind a conflict were silently dropped).
#[test]
fn test_revert_continue_drains_remaining_commits() {
    use super::run_libra_command;
    let repo = tempdir().expect("repo");
    let p = repo.path();
    let (c2, c3) = setup_conflict_then_clean(p);

    let out = run_libra_command(&["revert", &c2, &c3], p);
    assert!(!out.status.success(), "c2 revert conflicts");
    assert!(p.join("g.txt").exists(), "c3 not reverted yet");

    // Resolve the conflict and continue.
    fs::write(p.join("f.txt"), "L1\nRESOLVED\nL3\n").expect("resolve");
    run_libra_command(&["add", "f.txt"], p);
    let cont = run_libra_command(&["revert", "--continue"], p);
    assert!(
        cont.status.success(),
        "--continue: {}",
        String::from_utf8_lossy(&cont.stderr)
    );
    // c3 was drained from the sequence and reverted -> g.txt removed.
    assert!(
        !p.join("g.txt").exists(),
        "remaining commit c3 reverted on --continue"
    );
    assert!(
        !p.join(".libra/revert-state.json").exists(),
        "state cleared after the whole sequence completes"
    );
}

/// `revert --skip` discards the current (conflicted) commit and continues with
/// the remaining ones: c2 stays un-reverted, c3 is reverted.
#[test]
fn test_revert_skip_continues_with_remaining() {
    use super::run_libra_command;
    let repo = tempdir().expect("repo");
    let p = repo.path();
    let (c2, c3) = setup_conflict_then_clean(p);

    let out = run_libra_command(&["revert", &c2, &c3], p);
    assert!(!out.status.success(), "c2 revert conflicts");

    let skip = run_libra_command(&["revert", "--skip"], p);
    assert!(
        skip.status.success(),
        "--skip: {}",
        String::from_utf8_lossy(&skip.stderr)
    );
    // c2 was skipped (f.txt keeps its latest content, no conflict markers).
    let f = fs::read_to_string(p.join("f.txt")).unwrap();
    assert_eq!(
        f, "L1\nL2again\nL3\n",
        "skipped commit's changes discarded cleanly"
    );
    assert!(!f.contains("<<<<<<<"), "no conflict markers remain");
    // c3 (the remaining commit) was still reverted.
    assert!(
        !p.join("g.txt").exists(),
        "remaining commit c3 reverted after --skip"
    );
    assert!(
        !p.join(".libra/revert-state.json").exists(),
        "state cleared"
    );
}

/// `revert --skip` when nothing remains after the skipped commit clears the
/// in-progress state and creates no commit (HEAD unchanged).
#[test]
fn test_revert_skip_with_nothing_remaining() {
    use super::run_libra_command;
    let repo = tempdir().expect("repo");
    let p = repo.path();
    let (c2, _c3) = setup_conflict_then_clean(p);

    let head_before = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();

    let out = run_libra_command(&["revert", &c2], p);
    assert!(!out.status.success(), "single c2 revert conflicts");

    let skip = run_libra_command(&["revert", "--skip"], p);
    assert!(skip.status.success(), "--skip with nothing left succeeds");
    let head_after = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    assert_eq!(head_before, head_after, "no revert commit created");
    assert!(
        !fs::read_to_string(p.join("f.txt"))
            .unwrap()
            .contains("<<<<<<<"),
        "conflict markers discarded"
    );
    assert!(
        !p.join(".libra/revert-state.json").exists(),
        "state cleared"
    );
}

/// Build a repo like `setup_conflict_then_clean` but also create a 2-parent
/// merge commit `M` (merging a side branch into main). Reverting `c2` conflicts;
/// `M` is a merge, so reverting it without `-m` during a sequence drain fails
/// with a non-conflict error. Returns `(c2_hash, merge_hash)`.
fn setup_conflict_then_merge(p: &std::path::Path) -> (String, String) {
    use super::run_libra_command;
    assert!(run_libra_command(&["init"], p).status.success(), "init");
    run_libra_command(&["config", "set", "user.name", "t"], p);
    run_libra_command(&["config", "set", "user.email", "t@t"], p);
    let rev_parse = |p: &std::path::Path| {
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
            .trim()
            .to_string()
    };

    fs::write(p.join("f.txt"), "L1\nL2\nL3\n").expect("write f");
    run_libra_command(&["add", "f.txt"], p);
    run_libra_command(&["commit", "-m", "c1", "--no-verify"], p);
    // A side branch off c1 with a disjoint file, for a clean merge later.
    run_libra_command(&["branch", "side"], p);

    fs::write(p.join("f.txt"), "L1\nL2mod\nL3\n").expect("modify f");
    run_libra_command(&["add", "f.txt"], p);
    run_libra_command(&["commit", "-m", "c2", "--no-verify"], p);
    let c2 = rev_parse(p);

    fs::write(p.join("f.txt"), "L1\nL2again\nL3\n").expect("re-modify f");
    run_libra_command(&["add", "f.txt"], p);
    run_libra_command(&["commit", "-m", "c3", "--no-verify"], p);

    run_libra_command(&["switch", "side"], p);
    fs::write(p.join("s.txt"), "s\n").expect("write s");
    run_libra_command(&["add", "s.txt"], p);
    run_libra_command(&["commit", "-m", "sideC", "--no-verify"], p);
    run_libra_command(&["switch", "main"], p);
    assert!(
        run_libra_command(&["merge", "side"], p).status.success(),
        "merge side into main"
    );
    let merge = rev_parse(p);
    (c2, merge)
}

/// `revert` resolves every commit spec up front: a bad ref later in the list
/// fails before any revert is applied (no partial work, no in-progress state).
#[test]
fn test_revert_rejects_bad_ref_up_front() {
    use super::run_libra_command;
    let repo = tempdir().expect("repo");
    let p = repo.path();
    let (c2, _c3) = setup_conflict_then_clean(p);
    let f_before = fs::read_to_string(p.join("f.txt")).unwrap();

    let out = run_libra_command(&["revert", &c2, "this-ref-does-not-exist"], p);
    assert!(
        !out.status.success(),
        "a bad ref in the list fails the whole revert"
    );
    // c2 must NOT have been reverted (no partial application).
    assert_eq!(
        fs::read_to_string(p.join("f.txt")).unwrap(),
        f_before,
        "no commit reverted when a later ref is invalid"
    );
    assert!(
        !p.join(".libra/revert-state.json").exists(),
        "no in-progress state from an up-front validation failure"
    );
}

/// The pending queue persists resolved commit IDs, not the raw refs, so a ref
/// that moves during the conflict pause cannot change what gets reverted.
#[test]
fn test_revert_remaining_persists_resolved_ids() {
    use super::run_libra_command;
    let repo = tempdir().expect("repo");
    let p = repo.path();
    let (c2, c3) = setup_conflict_then_clean(p);
    // A branch pointing at c3, used as the pending spec.
    run_libra_command(&["branch", "target", &c3], p);

    let out = run_libra_command(&["revert", &c2, "target"], p);
    assert!(!out.status.success(), "c2 conflicts");
    let state = fs::read_to_string(p.join(".libra/revert-state.json")).expect("state");
    // The stored remaining entry is c3's resolved hash, not the branch name.
    assert!(
        state.contains(&c3),
        "remaining stores the resolved id {c3}, got: {state}"
    );
    assert!(
        !state.contains("target"),
        "remaining must not store the raw ref name, got: {state}"
    );
}

/// Regression: a non-conflict error while draining the remaining queue on
/// `--continue` (here a merge commit needing `-m`) must clear the state, so the
/// already-finished conflict is not left lingering as in-progress.
#[test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
fn test_revert_continue_clears_state_on_drain_error() {
    use super::run_libra_command;
    let repo = tempdir().expect("repo");
    let p = repo.path();
    let (c2, merge) = setup_conflict_then_merge(p);

    let out = run_libra_command(&["revert", &c2, &merge], p);
    assert!(!out.status.success(), "c2 revert conflicts");
    assert!(
        p.join(".libra/revert-state.json").exists(),
        "conflict records state"
    );

    fs::write(p.join("f.txt"), "L1\nRESOLVED\nL3\n").expect("resolve");
    run_libra_command(&["add", "f.txt"], p);
    // --continue finishes c2, then fails to revert the merge commit (needs -m).
    let cont = run_libra_command(&["revert", "--continue"], p);
    assert!(
        !cont.status.success(),
        "the merge commit in the queue fails the drain"
    );
    assert!(
        !p.join(".libra/revert-state.json").exists(),
        "stale state must not point at the already-committed conflict"
    );
    let retry = run_libra_command(&["revert", "--continue"], p);
    assert!(
        String::from_utf8_lossy(&retry.stderr).contains("no revert in progress"),
        "retry reports no in-progress revert"
    );
}

/// Regression (skip side): a non-conflict drain error after `--skip` must also
/// clear the state.
#[test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
fn test_revert_skip_clears_state_on_drain_error() {
    use super::run_libra_command;
    let repo = tempdir().expect("repo");
    let p = repo.path();
    let (c2, merge) = setup_conflict_then_merge(p);

    let out = run_libra_command(&["revert", &c2, &merge], p);
    assert!(!out.status.success(), "c2 revert conflicts");

    let skip = run_libra_command(&["revert", "--skip"], p);
    assert!(
        !skip.status.success(),
        "the merge commit in the queue fails the drain"
    );
    assert!(
        !p.join(".libra/revert-state.json").exists(),
        "skip must not leave stale state after a drain error"
    );
    let retry = run_libra_command(&["revert", "--skip"], p);
    assert!(
        String::from_utf8_lossy(&retry.stderr).contains("no revert in progress"),
        "retry reports no in-progress revert"
    );
}
