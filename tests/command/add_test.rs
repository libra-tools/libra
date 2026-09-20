//! Tests `libra add` behavior for staging files, refresh operations, and
//! edge cases via the in-process API (`add::execute`).
//!
//! **Layer:** L1 — deterministic, no external dependencies.
//!
//! Fixture convention: every test creates a `tempdir()`, calls
//! `test::setup_with_new_libra_in()` to bootstrap a fresh repo, holds a
//! `ChangeDirGuard` (hence `#[serial]`), then operates on plain text files
//! at the repo root or in nested subdirectories. Assertions inspect the
//! index via `changes_to_be_committed()` (staged) or
//! `changes_to_be_staged()` (working-tree-vs-index).

use std::{fs, io::Write};

use libra::{
    internal::{ai::automation::AutomationHistory, db::get_db_conn_instance},
    utils::{error::StableErrorCode, output::OutputConfig},
};
use sea_orm::{ConnectionTrait, Statement};

use super::*;

/// Scenario: smoke test for the simplest staging path — create one file,
/// run `add`, and confirm the path appears in the staged "new" set.
#[tokio::test]
#[serial(cwd)]
async fn test_add_single_file() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a new file
    let file_content = "Hello, World!";
    let file_path = "test_file.txt";
    let mut file = fs::File::create(file_path).unwrap();
    file.write_all(file_content.as_bytes()).unwrap();

    // Execute add command
    add::execute(AddArgs {
        pathspec: vec![String::from(file_path)],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    // Verify the file was added to index.
    let changes = changes_to_be_committed().await;

    assert!(changes.new.iter().any(|x| x.to_str().unwrap() == file_path));
}

#[tokio::test]
#[serial(cwd)]
async fn test_add_reports_marker_registration_failure_without_panicking() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());
    fs::write(
        test_dir.path().join(".libra/object-index-repair"),
        b"conflicting non-directory",
    )
    .unwrap();
    fs::write("marker-failure.txt", "content").unwrap();

    let error = add::execute_safe(
        AddArgs {
            pathspec: vec!["marker-failure.txt".to_string()],
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
            resolved: false,
            patch: false,
            auto_advance: false,
            no_auto_advance: false,
        },
        &OutputConfig::default(),
    )
    .await
    .expect_err("marker registration failure must be returned");

    assert_eq!(error.stable_code(), StableErrorCode::IoWriteFailed);
    // ADR-OI-05 item 2: the single-prefix canonical message explains that the
    // payloads are safe, nothing was staged, and a direct retry is enough.
    let message = error.to_string();
    assert!(
        message.contains("object payloads were stored safely"),
        "unexpected error: {message}"
    );
    assert!(
        message.contains("no paths were staged"),
        "unexpected error: {message}"
    );
    assert!(
        message.contains("retry the command directly"),
        "unexpected error: {message}"
    );

    fs::remove_file(test_dir.path().join(".libra/object-index-repair"))
        .expect("remove injected marker-directory conflict");
    add::execute_safe(
        AddArgs {
            pathspec: vec!["marker-failure.txt".to_string()],
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
            resolved: false,
            patch: false,
            auto_advance: false,
            no_auto_advance: false,
        },
        &OutputConfig::default(),
    )
    .await
    .expect("a normal retry should stage and re-register the existing blob");
    libra::utils::client_storage::ClientStorage::wait_for_background_tasks();
    let conn = get_db_conn_instance().await;
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS n FROM object_index WHERE o_type = 'blob' AND o_size = 7"
                .to_string(),
        ))
        .await
        .expect("query retried blob index row")
        .expect("count query should return one row");
    assert_eq!(
        row.try_get_by::<i64, _>("n")
            .expect("decode retried blob count"),
        1,
        "retry staged the existing blob without restoring its cloud index row"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_add_dispatches_vcs_automation_history() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());
    fs::write(
        test_dir.path().join(".libra").join("automations.toml"),
        r#"
        [[rules]]
        id = "index_summary"
        trigger = { kind = "vcs", event = "post_add" }
        action = { kind = "prompt", prompt = "summarize staged changes" }
    "#,
    )
    .unwrap();
    fs::write("automated.txt", "content").unwrap();

    add::execute_safe(
        AddArgs {
            pathspec: vec!["automated.txt".to_string()],
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
            resolved: false,
            patch: false,
            auto_advance: false,
            no_auto_advance: false,
        },
        &libra::utils::output::OutputConfig::default(),
    )
    .await
    .unwrap();

    let db = get_db_conn_instance().await;
    let rows = AutomationHistory::list_recent(&db, 10).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].rule_id, "index_summary");
    assert_eq!(rows[0].trigger_kind, "vcs");
    assert_eq!(rows[0].details["prompt"], "summarize staged changes");
}

#[tokio::test]
#[serial(cwd)]
async fn test_add_dry_run_does_not_dispatch_vcs_automation_history() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());
    fs::write(
        test_dir.path().join(".libra").join("automations.toml"),
        r#"
        [[rules]]
        id = "index_summary"
        trigger = { kind = "vcs", event = "post_add" }
        action = { kind = "prompt", prompt = "summarize staged changes" }
    "#,
    )
    .unwrap();
    fs::write("dry-run.txt", "content").unwrap();

    add::execute_safe(
        AddArgs {
            pathspec: vec!["dry-run.txt".to_string()],
            all: false,
            update: false,
            refresh: false,
            force: false,
            verbose: false,
            dry_run: true,
            ignore_errors: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
            resolved: false,
            patch: false,
            auto_advance: false,
            no_auto_advance: false,
        },
        &libra::utils::output::OutputConfig::default(),
    )
    .await
    .unwrap();

    let db = get_db_conn_instance().await;
    let rows = AutomationHistory::list_recent(&db, 10).await.unwrap();
    assert!(rows.is_empty());
}

/// Scenario: passing several pathspecs in one `add` call must stage every
/// listed file. Guards against accidental short-circuiting after the first
/// path.
#[tokio::test]
#[serial(cwd)]
async fn test_add_multiple_files() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create multiple files
    for i in 1..=3 {
        let file_content = format!("File content {i}");
        let file_path = format!("test_file_{i}.txt");
        let mut file = fs::File::create(&file_path).unwrap();
        file.write_all(file_content.as_bytes()).unwrap();
    }

    // Execute add command
    add::execute(AddArgs {
        pathspec: vec![
            String::from("test_file_1.txt"),
            String::from("test_file_2.txt"),
            String::from("test_file_3.txt"),
        ],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    // Verify all files were added to index
    let changes = changes_to_be_committed().await;
    assert!(
        changes
            .new
            .iter()
            .any(|x| x.to_str().unwrap() == "test_file_1.txt")
    );
    assert!(
        changes
            .new
            .iter()
            .any(|x| x.to_str().unwrap() == "test_file_2.txt")
    );
    assert!(
        changes
            .new
            .iter()
            .any(|x| x.to_str().unwrap() == "test_file_3.txt")
    );
}

/// Scenario: `--all` walks the working tree and stages every untracked
/// file even though no pathspec is supplied. Locks in the recursive
/// scan behavior of `-A`.
#[tokio::test]
#[serial(cwd)]
async fn test_add_all_flag() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create multiple files
    for i in 1..=3 {
        let file_content = format!("File content {i}");
        let file_path = format!("test_file_{i}.txt");
        let mut file = fs::File::create(&file_path).unwrap();
        file.write_all(file_content.as_bytes()).unwrap();
    }

    // Execute add command with --all flag
    add::execute(AddArgs {
        pathspec: vec![],
        all: true,
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    // Verify all files were added to index
    let changes = changes_to_be_committed().await;
    assert!(
        changes
            .new
            .iter()
            .any(|x| x.to_str().unwrap() == "test_file_1.txt")
    );
    assert!(
        changes
            .new
            .iter()
            .any(|x| x.to_str().unwrap() == "test_file_2.txt")
    );
    assert!(
        changes
            .new
            .iter()
            .any(|x| x.to_str().unwrap() == "test_file_3.txt")
    );
}

/// Scenario: `--update` (`-u`) must update tracked files only and never
/// promote untracked files to staged. Verifies that the previously-tracked
/// file ceases to show as modified (it was restaged) while the untracked
/// file remains in the "new" set.
#[tokio::test]
#[serial(cwd)]
async fn test_add_update_flag() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create files and add one to the index
    let tracked_file = "tracked_file.txt";
    let untracked_file = "untracked_file.txt";

    // Create and write initial content
    let mut file1 = fs::File::create(tracked_file).unwrap();
    file1.write_all(b"Initial content").unwrap();

    let mut file2 = fs::File::create(untracked_file).unwrap();
    file2.write_all(b"Initial content").unwrap();

    // Add only one file to the index
    add::execute(AddArgs {
        pathspec: vec![String::from(tracked_file)],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    // Modify both files
    let mut file1 = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(tracked_file)
        .unwrap();
    file1.write_all(b" - Modified").unwrap();

    let mut file2 = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(untracked_file)
        .unwrap();
    file2.write_all(b" - Modified").unwrap();

    // Execute add command with --update flag
    add::execute(AddArgs {
        pathspec: vec![String::from(".")],
        all: false,
        update: true,
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    // Verify only tracked file was updated
    let changes = changes_to_be_staged().unwrap();
    // Tracked file should not appear in changes (because it was updated in index)
    assert!(
        !changes
            .modified
            .iter()
            .any(|x| x.to_str().unwrap() == tracked_file)
    );
    // Untracked file should still be untracked and show as new
    assert!(
        changes
            .new
            .iter()
            .any(|x| x.to_str().unwrap() == untracked_file)
    );
}

/// Scenario: `.libraignore` patterns must filter both globbed file names
/// and entire directories. The non-ignored file must end up staged while
/// `ignored_*.txt` and `ignore_dir/**` remain hidden in both staged and
/// committed change lists. Pins ignore-glob semantics.
#[tokio::test]
#[serial(cwd)]
async fn test_add_with_ignore_patterns() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create .libraignore file
    let mut ignore_file = fs::File::create(".libraignore").unwrap();
    ignore_file
        .write_all(b"ignored_*.txt\nignore_dir/**")
        .unwrap();

    // Create files that should be ignored and not ignored
    let ignored_file = "ignored_file.txt";
    let tracked_file = "tracked_file.txt";

    // Create directory that should be ignored
    fs::create_dir("ignore_dir").unwrap();
    let ignored_dir_file = "ignore_dir/file.txt";

    // Create and write content
    let mut file1 = fs::File::create(ignored_file).unwrap();
    file1.write_all(b"Should be ignored").unwrap();

    let mut file2 = fs::File::create(tracked_file).unwrap();
    file2.write_all(b"Should be tracked").unwrap();

    let mut file3 = fs::File::create(ignored_dir_file).unwrap();
    file3.write_all(b"Should be ignored").unwrap();

    // Execute add command with all files
    add::execute(AddArgs {
        pathspec: vec![String::from(".")],
        all: true,
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    // Verify only non-ignored files were added
    let changes_staged = changes_to_be_staged().unwrap();
    let changes_committed = changes_to_be_committed().await;

    // Ignored files should not appear in any status (they are ignored)
    assert!(
        !changes_staged
            .new
            .iter()
            .any(|x| x.to_str().unwrap() == ignored_file)
    );
    assert!(
        !changes_staged
            .new
            .iter()
            .any(|x| x.to_str().unwrap() == ignored_dir_file)
    );
    assert!(
        !changes_committed
            .new
            .iter()
            .any(|x| x.to_str().unwrap() == ignored_file)
    );
    assert!(
        !changes_committed
            .new
            .iter()
            .any(|x| x.to_str().unwrap() == ignored_dir_file)
    );

    // Non-ignored file should not show as new in staged (was added) but should show in committed
    assert!(
        !changes_staged
            .new
            .iter()
            .any(|x| x.to_str().unwrap() == tracked_file)
    );
    assert!(
        changes_committed
            .new
            .iter()
            .any(|x| x.to_str().unwrap() == tracked_file)
    );
}

/// Scenario: `--force` lifts the ignore filter for a single path and once
/// that path is tracked, subsequent edits flow through without `--force`.
/// Validates the "force once, stay tracked" promise.
#[tokio::test]
#[serial(cwd)]
async fn test_add_force_tracks_ignored_file() {
    let repo = tempdir().unwrap();
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write(".libraignore", "ignored.txt\n").unwrap();
    fs::write("ignored.txt", "first").unwrap();

    let ignored_path = "ignored.txt";

    // Without --force the ignored file should stay hidden from staging
    let unstaged_initial = changes_to_be_staged().unwrap();
    assert!(
        !unstaged_initial
            .new
            .iter()
            .any(|p| p.to_str().unwrap() == ignored_path)
    );

    add::execute(AddArgs {
        pathspec: vec![ignored_path.into()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    let staged_without_force = changes_to_be_committed().await;
    assert!(
        !staged_without_force
            .new
            .iter()
            .any(|p| p.to_str().unwrap() == ignored_path)
    );

    // Force add should stage the ignored file
    add::execute(AddArgs {
        pathspec: vec![ignored_path.into()],
        all: false,
        update: false,
        refresh: false,
        force: true,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
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

    let staged_with_force = changes_to_be_committed().await;
    assert!(
        staged_with_force
            .new
            .iter()
            .any(|p| p.to_str().unwrap() == ignored_path)
    );

    // After being tracked, further updates should appear without --force
    fs::write("ignored.txt", "second").unwrap();

    let unstaged_after_edit = changes_to_be_staged().unwrap();
    assert!(
        unstaged_after_edit
            .modified
            .iter()
            .any(|p| p.to_str().unwrap() == ignored_path)
    );

    add::execute(AddArgs {
        pathspec: vec![ignored_path.into()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    let staged_after_update = changes_to_be_committed().await;
    assert!(
        staged_after_update
            .new
            .iter()
            .any(|p| p.to_str().unwrap() == ignored_path)
    );

    let unstaged_final = changes_to_be_staged().unwrap();
    assert!(
        !unstaged_final
            .modified
            .iter()
            .any(|p| p.to_str().unwrap() == ignored_path)
    );
}

/// Scenario: `add --force .` recursively includes the contents of an
/// ignored directory. Path separators are normalized to forward slashes
/// for cross-platform comparison. Pins the directory-level force semantic.
#[tokio::test]
#[serial(cwd)]
async fn test_add_force_dot_includes_ignored_directory() {
    let repo = tempdir().unwrap();
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = test::ChangeDirGuard::new(repo.path());

    fs::write(".libraignore", "ignored_dir/\n").unwrap();
    fs::create_dir_all("ignored_dir").unwrap();
    fs::write("ignored_dir/nested.txt", "ignored").unwrap();
    fs::write("visible.txt", "seen").unwrap();

    // Baseline: without --force the ignored directory stays hidden
    add::execute(AddArgs {
        pathspec: vec![".".into()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    let staged_without_force = changes_to_be_committed().await;
    assert!(
        !staged_without_force
            .new
            .iter()
            .any(|p| p.to_str().unwrap().replace("\\", "/") == "ignored_dir/nested.txt"),
        "ignored entries should not be staged when force is false"
    );
    assert!(
        staged_without_force
            .new
            .iter()
            .any(|p| p.to_str().unwrap() == "visible.txt"),
        "non-ignored files should still be staged"
    );

    // Re-run with --force to include ignored entries
    add::execute(AddArgs {
        pathspec: vec![".".into()],
        all: false,
        update: false,
        refresh: false,
        force: true,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
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

    let staged_with_force = changes_to_be_committed().await;
    assert!(
        staged_with_force
            .new
            .iter()
            .any(|p| p.to_str().unwrap().replace("\\", "/") == "ignored_dir/nested.txt"),
        "`add --force .` should surface ignored children"
    );
}

/// Scenario: `--dry-run` should leave the index unchanged. Note: this
/// test asserts that the path appears in `changes_to_be_staged().new` —
/// i.e. the file is detected as untracked in the working tree, confirming
/// it was not staged.
#[tokio::test]
#[serial(cwd)]
async fn test_add_dry_run() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a file.
    let file_path = "test_file.txt";
    let mut file = fs::File::create(file_path).unwrap();
    file.write_all(b"Test content").unwrap();

    // Execute add command with dry-run
    add::execute(AddArgs {
        pathspec: vec![String::from(file_path)],
        all: false,
        update: false,
        refresh: false,
        force: false,
        verbose: false,
        dry_run: true,
        ignore_errors: false,
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

    // Verify the file was not actually added to index
    let changes = changes_to_be_staged().unwrap();
    assert!(changes.new.iter().any(|x| x.to_str().unwrap() == file_path));
    // M-BATCH B6: preview commands publish no durable repair markers.
    assert!(
        !test_dir
            .path()
            .join(".libra")
            .join("object-index-repair")
            .exists(),
        "a dry-run must not publish object-index repair markers"
    );
}

/// Scenario: in-process `add::execute` with no pathspec and no `--all`
/// must not silently stage anything. The index should be empty after the
/// call. Boundary condition: the in-process API does not surface CLI exit
/// codes, so the assertion is on side effects only.
#[tokio::test]
#[serial(cwd)]
async fn test_add_without_path_should_error() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a file to ensure there's something that could be added
    let file_path = "existing_file.txt";
    let mut file = fs::File::create(file_path).unwrap();
    file.write_all(b"Some content").unwrap();

    // Try running `add` without any pathspec and without --all
    add::execute(AddArgs {
        pathspec: vec![], // Empty pathspec
        all: false,       // Not using --all
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    // Verify no files were added to the index
    let changes = changes_to_be_committed().await;
    assert!(
        changes.new.is_empty(),
        "Expected no files in index when no pathspec provided and --all not used"
    );
}

/// Scenario: passing a path that doesn't exist must not stage anything.
/// Pins the post-condition: the bogus path never appears in
/// `changes_to_be_committed().new`.
#[tokio::test]
#[serial(cwd)]
async fn test_add_nonexistent_file_should_error() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    let fake_path = "no_such_file.txt";

    // Try to add non-existent file
    add::execute(AddArgs {
        pathspec: vec![String::from(fake_path)],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    // The file should not be in the index
    let changes = changes_to_be_committed().await;
    let file_in_index = changes.new.iter().any(|x| x.to_str().unwrap() == fake_path);
    assert!(
        !file_in_index,
        "Non-existent file should not be added to index"
    );
}

/// Scenario: invoking `add` twice on the same path must not produce
/// duplicate index entries. Pins the idempotency invariant of the staging
/// pipeline.
#[tokio::test]
#[serial(cwd)]
async fn test_add_duplicate_file_should_not_duplicate_index() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    let file_path = "dup_test.txt";
    let mut file = fs::File::create(file_path).unwrap();
    file.write_all(b"content").unwrap();

    // Add same file twice
    for i in 0..2 {
        add::execute(AddArgs {
            pathspec: vec![String::from(file_path)],
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
            resolved: false,
            patch: false,
            auto_advance: false,
            no_auto_advance: false,
        })
        .await;

        // Check after each add operation
        let changes = changes_to_be_committed().await;
        let occurrences = changes
            .new
            .iter()
            .filter(|x| x.to_str().unwrap() == file_path)
            .count();
        assert_eq!(
            occurrences,
            1,
            "File should appear exactly once in index after {} add operation(s)",
            i + 1
        );
    }
}

/// Scenario: zero-byte files must be stageable. Regression guard against
/// "non-empty content required" assumptions in the blob hashing path.
#[tokio::test]
#[serial(cwd)]
async fn test_add_empty_file() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create an empty file
    let file_path = "empty.txt";
    fs::File::create(file_path).unwrap();

    // Execute add command
    add::execute(AddArgs {
        pathspec: vec![String::from(file_path)],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    // Verify the empty file was added to index
    let changes = changes_to_be_committed().await;
    assert!(
        changes.new.iter().any(|x| x.to_str().unwrap() == file_path),
        "Empty file should be added to index"
    );
}

/// Scenario: deeply nested paths (`a/b/c/deep.txt`) must be staged with
/// their full repository-relative path. Path separators are normalized to
/// `/` so the test passes on Windows.
#[tokio::test]
#[serial(cwd)]
async fn test_add_sub_directory_file() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create nested subdirectory structure
    let sub_dir = "a/b/c";
    fs::create_dir_all(sub_dir).unwrap();
    let file_path = "a/b/c/deep.txt";
    fs::write(file_path, "hello deep").unwrap();

    // Execute add command
    add::execute(AddArgs {
        pathspec: vec![String::from(file_path)],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    // Verify the file in nested directory was added to index
    let changes = changes_to_be_committed().await;
    assert!(
        changes
            .new
            .iter()
            .any(|x| x.to_str().unwrap().replace("\\", "/") == file_path),
        "File in nested subdirectory should be added to index"
    );
}

/// `--pathspec-from-file` (newline-separated) stages only the listed paths and
/// merges with any pathspecs passed on the command line.
#[tokio::test]
#[serial(cwd)]
async fn test_add_pathspec_from_file_newline_stages_listed_paths() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    fs::write("file1.txt", "one\n").unwrap();
    fs::write("file2.txt", "two\n").unwrap();
    fs::write("file3.txt", "three\n").unwrap();
    // Only file1 is listed; file2/file3 stay unstaged. (ADR-PSF-03: a
    // command-line pathspec cannot be combined with `--pathspec-from-file`.)
    fs::write("paths.txt", "file1.txt\n").unwrap();

    add::execute(AddArgs {
        pathspec: vec![],
        all: false,
        update: false,
        refresh: false,
        force: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        pathspec_from_file: Some(String::from("paths.txt")),
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

    let changes = changes_to_be_committed().await;
    let staged = |name: &str| changes.new.iter().any(|x| x.to_str().unwrap() == name);
    assert!(
        staged("file1.txt"),
        "file1.txt (from file list) should be staged"
    );
    assert!(!staged("file2.txt"), "file2.txt should NOT be staged");
    assert!(!staged("file3.txt"), "file3.txt should NOT be staged");
}

/// `--pathspec-from-file` with `--pathspec-file-nul` reads a NUL-separated list.
#[tokio::test]
#[serial(cwd)]
async fn test_add_pathspec_from_file_nul_stages_listed_paths() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    fs::write("keep.txt", "keep\n").unwrap();
    fs::write("skip.txt", "skip\n").unwrap();
    // NUL-separated list naming only keep.txt.
    fs::write("paths.bin", b"keep.txt\0").unwrap();

    add::execute(AddArgs {
        pathspec: vec![],
        all: false,
        update: false,
        refresh: false,
        force: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        pathspec_from_file: Some(String::from("paths.bin")),
        pathspec_file_nul: true,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    let changes = changes_to_be_committed().await;
    let staged = |name: &str| changes.new.iter().any(|x| x.to_str().unwrap() == name);
    assert!(
        staged("keep.txt"),
        "keep.txt (from NUL list) should be staged"
    );
    assert!(!staged("skip.txt"), "skip.txt should NOT be staged");
}

/// `--pathspec-file-nul` requires `--pathspec-from-file` (clap `requires`); using
/// it alone is a usage error.
#[test]
fn test_add_pathspec_file_nul_requires_from_file() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["add", "--pathspec-file-nul", "."], repo.path());
    assert!(
        !output.status.success(),
        "--pathspec-file-nul without --pathspec-from-file should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("pathspec-from-file"),
        "error should mention the required --pathspec-from-file, got: {stderr}"
    );
}

#[test]
fn test_add_dry_run_short_n_and_d_alias() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("new.txt"), "x\n").unwrap();

    // `-n` (Git's short for --dry-run) previews without staging.
    let dry = run_libra_command(&["add", "-n", "new.txt"], p);
    assert_cli_success(&dry, "add -n");
    let status = run_libra_command(&["status", "--short"], p);
    assert!(
        !String::from_utf8_lossy(&status.stdout).contains("A  new.txt"),
        "add -n does not stage the file"
    );

    // `-d` remains a working back-compat alias for --dry-run.
    let dry_d = run_libra_command(&["add", "-d", "new.txt"], p);
    assert_cli_success(&dry_d, "add -d (alias)");
    let status2 = run_libra_command(&["status", "--short"], p);
    assert!(
        !String::from_utf8_lossy(&status2.stdout).contains("A  new.txt"),
        "add -d also does not stage the file"
    );
}

/// `--chmod=+x` sets the executable bit (index mode 100755) on the matched
/// file and `--chmod=-x` clears it (100644), without changing the blob.
#[tokio::test]
#[serial(cwd)]
async fn test_add_chmod_sets_and_clears_exec_bit() {
    let dir = tempdir().unwrap();
    test::setup_with_new_libra_in(dir.path()).await;
    let p = dir.path();
    let _guard = test::ChangeDirGuard::new(p);

    fs::write(p.join("f.txt"), "x\n").unwrap();
    assert!(run_libra_command(&["add", "f.txt"], p).status.success());
    assert!(
        run_libra_command(&["commit", "-m", "base", "--no-verify"], p)
            .status
            .success()
    );

    let mode = |p: &std::path::Path| -> String {
        let out = run_libra_command(&["ls-files", "-s", "f.txt"], p);
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string()
    };

    assert!(
        run_libra_command(&["add", "--chmod=+x", "f.txt"], p)
            .status
            .success()
    );
    assert_eq!(mode(p), "100755", "--chmod=+x sets the executable bit");
    assert!(
        run_libra_command(&["add", "--chmod=-x", "f.txt"], p)
            .status
            .success()
    );
    assert_eq!(mode(p), "100644", "--chmod=-x clears the executable bit");
}

/// An invalid `--chmod` value is a usage error (exit 129), not a panic.
#[tokio::test]
#[serial(cwd)]
async fn test_add_chmod_invalid_value_errors() {
    let dir = tempdir().unwrap();
    test::setup_with_new_libra_in(dir.path()).await;
    let p = dir.path();
    let _guard = test::ChangeDirGuard::new(p);

    fs::write(p.join("f.txt"), "x\n").unwrap();
    assert!(run_libra_command(&["add", "f.txt"], p).status.success());

    let out = run_libra_command(&["add", "--chmod=bogus", "f.txt"], p);
    assert_eq!(out.status.code(), Some(129), "invalid --chmod exits 129");
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid --chmod value"));
}

/// `--renormalize` re-stages tracked files and never stages an untracked file
/// (it implies `-u`).
#[tokio::test]
#[serial(cwd)]
async fn test_add_renormalize_only_tracked() {
    let dir = tempdir().unwrap();
    test::setup_with_new_libra_in(dir.path()).await;
    let p = dir.path();
    let _guard = test::ChangeDirGuard::new(p);

    fs::write(p.join("tracked.txt"), "x\n").unwrap();
    assert!(
        run_libra_command(&["add", "tracked.txt"], p)
            .status
            .success()
    );
    assert!(
        run_libra_command(&["commit", "-m", "base", "--no-verify"], p)
            .status
            .success()
    );
    fs::write(p.join("untracked.txt"), "u\n").unwrap();

    assert!(
        run_libra_command(&["add", "--renormalize"], p)
            .status
            .success()
    );
    let status = run_libra_command(&["status", "--short"], p);
    let s = String::from_utf8_lossy(&status.stdout);
    assert!(
        s.lines()
            .any(|l| l.contains("untracked.txt") && l.trim_start().starts_with("??")),
        "untracked file must remain untracked under --renormalize: {s}"
    );
}

/// `--renormalize` stages the deletion of a tracked file removed from the
/// working tree.
#[tokio::test]
#[serial(cwd)]
async fn test_add_renormalize_stages_tracked_deletion() {
    let dir = tempdir().unwrap();
    test::setup_with_new_libra_in(dir.path()).await;
    let p = dir.path();
    let _guard = test::ChangeDirGuard::new(p);

    fs::write(p.join("gone.txt"), "x\n").unwrap();
    assert!(run_libra_command(&["add", "gone.txt"], p).status.success());
    assert!(
        run_libra_command(&["commit", "-m", "base", "--no-verify"], p)
            .status
            .success()
    );
    fs::remove_file(p.join("gone.txt")).unwrap();

    assert!(
        run_libra_command(&["add", "--renormalize"], p)
            .status
            .success()
    );
    let status = run_libra_command(&["status", "--short"], p);
    let s = String::from_utf8_lossy(&status.stdout);
    assert!(
        s.lines()
            .any(|l| l.contains("gone.txt") && l.starts_with("D")),
        "deletion of a tracked file must be staged under --renormalize: {s}"
    );
}

/// `--dry-run --ignore-missing` skips a pathspec that does not exist instead of
/// failing; `--ignore-missing` without `--dry-run` is rejected (Git requires it).
#[tokio::test]
#[serial(cwd)]
async fn test_add_ignore_missing_dry_run_skips() {
    let dir = tempdir().unwrap();
    test::setup_with_new_libra_in(dir.path()).await;
    let p = dir.path();
    let _guard = test::ChangeDirGuard::new(p);

    fs::write(p.join("real.txt"), "x\n").unwrap();
    assert!(run_libra_command(&["add", "real.txt"], p).status.success());

    let skip = run_libra_command(&["add", "--dry-run", "--ignore-missing", "nope.txt"], p);
    assert!(
        skip.status.success(),
        "missing pathspec must be skipped under --dry-run --ignore-missing"
    );
    assert!(String::from_utf8_lossy(&skip.stderr).contains("--ignore-missing"));

    // Without --dry-run the flag is rejected up front.
    let bad = run_libra_command(&["add", "--ignore-missing", "nope.txt"], p);
    assert_eq!(
        bad.status.code(),
        Some(129),
        "--ignore-missing requires --dry-run"
    );
}

/// Regression: a chmod-only change (same blob, new mode) is detected by
/// `status` as staged and can be committed — the committed tree carries 100755.
#[tokio::test]
#[serial(cwd)]
async fn test_add_chmod_only_change_is_committable() {
    let dir = tempdir().unwrap();
    test::setup_with_new_libra_in(dir.path()).await;
    let p = dir.path();
    let _guard = test::ChangeDirGuard::new(p);

    fs::write(p.join("s.sh"), "echo hi\n").unwrap();
    assert!(run_libra_command(&["add", "s.sh"], p).status.success());
    assert!(
        run_libra_command(&["commit", "-m", "base", "--no-verify"], p)
            .status
            .success()
    );

    assert!(
        run_libra_command(&["add", "--chmod=+x", "s.sh"], p)
            .status
            .success()
    );
    // status must surface the mode-only change as staged...
    let status = run_libra_command(&["status", "--short"], p);
    let s = String::from_utf8_lossy(&status.stdout);
    assert!(
        s.lines().any(|l| l.contains("s.sh") && l.starts_with('M')),
        "chmod-only change must show as staged-modified: {s}"
    );
    // ...and commit must accept it (not "nothing to commit").
    let commit = run_libra_command(&["commit", "-m", "chmod", "--no-verify"], p);
    assert!(
        commit.status.success(),
        "chmod-only change must be committable: {}",
        String::from_utf8_lossy(&commit.stderr)
    );
    let mode = run_libra_command(&["ls-files", "-s", "s.sh"], p);
    assert!(
        String::from_utf8_lossy(&mode.stdout).starts_with("100755"),
        "committed entry carries the executable bit: {}",
        String::from_utf8_lossy(&mode.stdout)
    );
}

/// `--json --dry-run --ignore-missing` exposes the skipped pathspec as a
/// machine-readable `missing` list (not just a stderr warning).
#[tokio::test]
#[serial(cwd)]
async fn test_add_ignore_missing_json_exposes_skipped() {
    let dir = tempdir().unwrap();
    test::setup_with_new_libra_in(dir.path()).await;
    let p = dir.path();
    let _guard = test::ChangeDirGuard::new(p);

    fs::write(p.join("real.txt"), "x\n").unwrap();
    assert!(run_libra_command(&["add", "real.txt"], p).status.success());

    let out = run_libra_command(
        &["add", "--json", "--dry-run", "--ignore-missing", "nope.txt"],
        p,
    );
    assert!(out.status.success());
    let json = parse_json_stdout(&out);
    let missing = &json["data"]["missing"];
    assert_eq!(
        missing.as_array().map(|a| a.len()),
        Some(1),
        "missing list has the skipped pathspec: {json}"
    );
    assert_eq!(missing[0], "nope.txt");
}

/// `--exit-code-on-warning` must honor an `--ignore-missing` skip: a skipped
/// pathspec is a warning, so the process exits non-zero under that contract.
#[tokio::test]
#[serial(cwd)]
async fn test_add_ignore_missing_triggers_warning_exit() {
    let dir = tempdir().unwrap();
    test::setup_with_new_libra_in(dir.path()).await;
    let p = dir.path();
    let _guard = test::ChangeDirGuard::new(p);

    fs::write(p.join("real.txt"), "x\n").unwrap();
    assert!(run_libra_command(&["add", "real.txt"], p).status.success());

    // A skip under --ignore-missing is a warning -> non-zero exit.
    let warned = run_libra_command(
        &[
            "--exit-code-on-warning",
            "add",
            "--dry-run",
            "--ignore-missing",
            "nope.txt",
        ],
        p,
    );
    assert!(
        !warned.status.success(),
        "a skipped pathspec must trip --exit-code-on-warning"
    );
    // No skip -> clean exit under the same contract.
    let clean = run_libra_command(
        &["--exit-code-on-warning", "add", "--dry-run", "real.txt"],
        p,
    );
    assert!(clean.status.success(), "no warning -> success exit");
}

fn t2207_conflicted_repo() -> tempfile::TempDir {
    let repo = tempdir().expect("tempdir");
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    for name in ["file1.txt", "file2.txt", "file3.txt", "file4.txt"] {
        fs::write(p.join(name), "base\n").unwrap();
    }
    assert_cli_success(
        &run_libra_command(
            &["add", "file1.txt", "file2.txt", "file3.txt", "file4.txt"],
            p,
        ),
        "add base files",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "initial", "--no-verify"], p),
        "commit initial",
    );
    assert_cli_success(&run_libra_command(&["branch", "topic"], p), "branch topic");
    fs::write(p.join("file1.txt"), "ours 1\n").unwrap();
    fs::write(p.join("file2.txt"), "ours 2\n").unwrap();
    fs::write(p.join("file3.txt"), "ours 3\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "file1.txt", "file2.txt", "file3.txt"], p),
        "add ours",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ours", "--no-verify"], p),
        "commit ours",
    );
    assert_cli_success(&run_libra_command(&["switch", "topic"], p), "switch topic");
    fs::write(p.join("file1.txt"), "theirs 1\n").unwrap();
    fs::write(p.join("file2.txt"), "theirs 2\n").unwrap();
    fs::write(p.join("file3.txt"), "theirs 3\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "file1.txt", "file2.txt", "file3.txt"], p),
        "add theirs",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "theirs", "--no-verify"], p),
        "commit theirs",
    );
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    let merge = run_libra_command(&["merge", "topic"], p);
    assert!(
        !merge.status.success(),
        "expected a content conflict, stderr:\n{}",
        String::from_utf8_lossy(&merge.stderr)
    );
    repo
}

fn ls_unmerged(p: &std::path::Path, path: &str) -> String {
    let out = if path.is_empty() {
        run_libra_command(&["ls-files", "-u"], p)
    } else {
        run_libra_command(&["ls-files", "-u", path], p)
    };
    assert_cli_success(&out, "ls-files -u");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn ls_staged(p: &std::path::Path, path: &str) -> String {
    let out = run_libra_command(&["ls-files", "-s", path], p);
    assert_cli_success(&out, "ls-files -s");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn unmerged_line_count(text: &str) -> usize {
    text.lines().filter(|line| !line.is_empty()).count()
}

fn index_bytes(p: &std::path::Path) -> Vec<u8> {
    fs::read(p.join(".libra/index")).expect("read index")
}

/// Port of git `t/t2207-add-resolved.sh` (R1–R10 / AU-06).
#[test]
fn test_t2207_add_resolved_matrix() {
    // R6: option conflict does not need a merge; still run inside a repo.
    {
        let repo = tempdir().unwrap();
        init_repo_via_cli(repo.path());
        let with_u = run_libra_command(&["add", "--resolved", "-u"], repo.path());
        assert_eq!(with_u.status.code(), Some(129), "resolved -u exits 129");
        let err = String::from_utf8_lossy(&with_u.stderr);
        assert!(
            err.contains("cannot be used together"),
            "resolved -u diagnostic: {err}"
        );
        let with_a = run_libra_command(&["add", "--resolved", "-A"], repo.path());
        assert_eq!(with_a.status.code(), Some(129), "resolved -A exits 129");
        let err = String::from_utf8_lossy(&with_a.stderr);
        assert!(
            err.contains("cannot be used together"),
            "resolved -A diagnostic: {err}"
        );
        let with_p = run_libra_command(&["add", "--resolved", "-p"], repo.path());
        assert_eq!(with_p.status.code(), Some(129), "resolved -p exits 129");
        let err = String::from_utf8_lossy(&with_p.stderr);
        assert!(
            err.contains("cannot be used together"),
            "resolved -p diagnostic: {err}"
        );
    }

    // R7: no unmerged entries → success, no index write.
    {
        let repo = tempdir().unwrap();
        let p = repo.path();
        init_repo_via_cli(p);
        configure_identity_via_cli(p);
        fs::write(p.join("clean.txt"), "ok\n").unwrap();
        assert_cli_success(&run_libra_command(&["add", "clean.txt"], p), "add clean");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "clean", "--no-verify"], p),
            "commit clean",
        );
        let before = index_bytes(p);
        let out = run_libra_command(&["add", "--resolved"], p);
        assert_cli_success(&out, "resolved with no unmerged paths");
        assert_eq!(index_bytes(p), before, "R7 must not rewrite the index");
    }

    // R1: leftover markers refuse the whole operation and leave the index.
    {
        let repo = t2207_conflicted_repo();
        let p = repo.path();
        fs::write(p.join("file1.txt"), "resolved 1\n").unwrap();
        let before = index_bytes(p);
        let out = run_libra_command(&["add", "--resolved"], p);
        assert!(!out.status.success(), "R1 must fail");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("the following paths still have conflict markers:"),
            "R1 message: {err}"
        );
        assert!(err.contains("file2.txt"), "R1 lists file2: {err}");
        assert!(err.contains("file3.txt"), "R1 lists file3: {err}");
        assert_eq!(index_bytes(p), before, "R1 zero index writes");
        assert_eq!(unmerged_line_count(&ls_unmerged(p, "file1.txt")), 3);
    }

    // R8 dry-run with leftover markers still fails; with all resolved, no write.
    {
        let repo = t2207_conflicted_repo();
        let p = repo.path();
        let dry_fail = run_libra_command(&["add", "--dry-run", "--resolved"], p);
        assert!(
            !dry_fail.status.success(),
            "dry-run still checks markers: {}",
            String::from_utf8_lossy(&dry_fail.stderr)
        );
        fs::write(p.join("file1.txt"), "resolved 1\n").unwrap();
        fs::write(p.join("file2.txt"), "resolved 2\n").unwrap();
        fs::write(p.join("file3.txt"), "resolved 3\n").unwrap();
        let before = index_bytes(p);
        let dry_ok = run_libra_command(&["add", "--dry-run", "--resolved"], p);
        assert_cli_success(&dry_ok, "dry-run resolved after markers removed");
        assert_eq!(index_bytes(p), before, "R8 dry-run must not write");
        assert!(!ls_unmerged(p, "").trim().is_empty(), "still unmerged");
    }

    // R2: all markers gone → stages collapse to stage 0.
    {
        let repo = t2207_conflicted_repo();
        let p = repo.path();
        fs::write(p.join("file1.txt"), "resolved 1\n").unwrap();
        fs::write(p.join("file2.txt"), "resolved 2\n").unwrap();
        fs::write(p.join("file3.txt"), "resolved 3\n").unwrap();
        assert_cli_success(
            &run_libra_command(&["add", "--resolved"], p),
            "resolved all files",
        );
        assert!(ls_unmerged(p, "").trim().is_empty(), "R2 ls-files -u empty");
        assert_eq!(unmerged_line_count(&ls_staged(p, "file1.txt")), 1);
        assert_eq!(unmerged_line_count(&ls_staged(p, "file2.txt")), 1);
        assert_eq!(unmerged_line_count(&ls_staged(p, "file3.txt")), 1);
    }

    // R3: unconflicted dirty file is left unstaged.
    {
        let repo = t2207_conflicted_repo();
        let p = repo.path();
        let mut file4 = fs::read_to_string(p.join("file4.txt")).unwrap();
        file4.push_str("unconflicted local change\n");
        fs::write(p.join("file4.txt"), &file4).unwrap();
        fs::write(p.join("file1.txt"), "resolved 1\n").unwrap();
        fs::write(p.join("file2.txt"), "resolved 2\n").unwrap();
        fs::write(p.join("file3.txt"), "resolved 3\n").unwrap();
        assert_cli_success(
            &run_libra_command(&["add", "--resolved"], p),
            "resolved ignoring file4",
        );
        assert!(ls_unmerged(p, "").trim().is_empty());
        let diff = run_libra_command(&["diff", "file4.txt"], p);
        assert_cli_success(&diff, "diff file4");
        let diff_text = String::from_utf8_lossy(&diff.stdout);
        assert!(
            diff_text.contains("unconflicted local change"),
            "file4 stays unstaged: {diff_text}"
        );
        let cached = run_libra_command(&["diff", "--cached", "file4.txt"], p);
        assert_cli_success(&cached, "diff --cached file4");
        assert!(
            String::from_utf8_lossy(&cached.stdout).trim().is_empty(),
            "file4 must not be cached"
        );
    }

    // R4: deleted conflict file is removed from the index.
    {
        let repo = t2207_conflicted_repo();
        let p = repo.path();
        fs::write(p.join("file1.txt"), "resolved 1\n").unwrap();
        fs::remove_file(p.join("file2.txt")).unwrap();
        fs::write(p.join("file3.txt"), "resolved 3\n").unwrap();
        assert_cli_success(
            &run_libra_command(&["add", "--resolved"], p),
            "resolved with deletion",
        );
        assert!(
            ls_staged(p, "file2.txt").trim().is_empty(),
            "R4 file2 gone from index"
        );
    }

    // R5: pathspec limits which unmerged path is resolved.
    {
        let repo = t2207_conflicted_repo();
        let p = repo.path();
        fs::write(p.join("file1.txt"), "resolved 1\n").unwrap();
        assert_cli_success(
            &run_libra_command(&["add", "--resolved", "file1.txt"], p),
            "resolved pathspec file1",
        );
        assert!(
            ls_unmerged(p, "file1.txt").trim().is_empty(),
            "file1 resolved"
        );
        assert_eq!(unmerged_line_count(&ls_unmerged(p, "file2.txt")), 3);
    }

    // R9: binary worktree content (NUL before any marker) is not treated as
    // leftover markers. R10: --json reports resolved paths as modified.
    {
        let repo = t2207_conflicted_repo();
        let p = repo.path();
        fs::write(p.join("file1.txt"), b"\0binary-resolved").unwrap();
        fs::write(p.join("file2.txt"), "resolved 2\n").unwrap();
        fs::write(p.join("file3.txt"), "resolved 3\n").unwrap();
        let json = run_libra_command(&["--json", "add", "--resolved"], p);
        assert_cli_success(&json, "json resolved");
        let value: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json stdout");
        let modified = value["data"]["modified"]
            .as_array()
            .expect("modified array");
        let names: Vec<&str> = modified.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            names.contains(&"file1.txt")
                && names.contains(&"file2.txt")
                && names.contains(&"file3.txt"),
            "json modified: {names:?}"
        );
        assert!(
            value["data"]["added"]
                .as_array()
                .map(|a| a.is_empty())
                .unwrap_or(false),
            "resolved paths are modified, not added"
        );
        assert!(ls_unmerged(p, "").trim().is_empty());
    }
}

fn run_libra_env(
    args: &[&str],
    cwd: &std::path::Path,
    extra: &[(&str, &str)],
) -> std::process::Output {
    spawn_libra_command_with_env(args, cwd, extra)
        .wait_with_output()
        .expect("wait libra")
}

fn committed_top_and_untracked_baz() -> tempfile::TempDir {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("top"), "tracked\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "top"], p), "add top");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    fs::write(p.join("top"), "tracked\nmodified\n").unwrap();
    fs::write(p.join("baz"), "untracked\n").unwrap();
    repo
}

/// AU-01 / M-UPD: `add -u` pathspec must name index-known paths.
#[test]
fn test_add_update_untracked_pathspec_fails_atomically_matrix() {
    // U1/U2: untracked `baz` fails the whole `add -u` and leaves the index.
    {
        let repo = committed_top_and_untracked_baz();
        let p = repo.path();
        let before = index_bytes(p);
        let out = run_libra_command(&["add", "-u", "baz", "top"], p);
        assert!(!out.status.success(), "U1 must fail");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("pathspec 'baz' did not match any file(s) known to the index"),
            "U1 diagnostic: {err}"
        );
        assert_eq!(index_bytes(p), before, "U1 zero index writes");
        let cached = run_libra_command(&["diff", "--cached", "--name-only"], p);
        assert!(
            String::from_utf8_lossy(&cached.stdout).trim().is_empty(),
            "U1 nothing staged"
        );

        let out = run_libra_command(&["add", "-u", "baz"], p);
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains("did not match any file(s) known to the index")
        );
        let out = run_libra_command(&["add", "-u", "top", "baz"], p);
        assert!(!out.status.success());
        assert_eq!(index_bytes(p), before);
    }

    // U3: glob that matches nothing in the index.
    {
        let repo = committed_top_and_untracked_baz();
        let p = repo.path();
        let before = index_bytes(p);
        let out = run_libra_command(&["add", "-u", "b*", "top"], p);
        assert!(!out.status.success(), "U3 glob must fail");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("pathspec 'b*' did not match any files"),
            "U3 diagnostic: {err}"
        );
        assert_eq!(index_bytes(p), before);
    }

    // U4: missing pathspec.
    {
        let repo = committed_top_and_untracked_baz();
        let p = repo.path();
        let before = index_bytes(p);
        let out = run_libra_command(&["add", "-u", "nothere"], p);
        assert!(!out.status.success(), "U4 must fail");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("did not match any files"),
            "U4 diagnostic"
        );
        assert_eq!(index_bytes(p), before);
    }

    // U5: dry-run still fails before preview / writes.
    {
        let repo = committed_top_and_untracked_baz();
        let p = repo.path();
        let before = index_bytes(p);
        let out = run_libra_command(&["add", "-u", "-n", "baz", "top"], p);
        assert!(!out.status.success(), "U5 dry-run must fail");
        assert!(
            String::from_utf8_lossy(&out.stdout).trim().is_empty(),
            "U5 no preview before failure: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert_eq!(index_bytes(p), before);
    }

    // U6: --ignore-errors skips the unknown pathspec and stages the rest.
    {
        let repo = committed_top_and_untracked_baz();
        let p = repo.path();
        let out = run_libra_command(&["add", "-u", "--ignore-errors", "baz", "top"], p);
        assert_cli_success(&out, "U6 ignore-errors");
        let cached = run_libra_command(&["diff", "--cached", "--name-only"], p);
        let names = String::from_utf8_lossy(&cached.stdout);
        assert!(names.contains("top"), "U6 staged top: {names}");
        assert!(!names.contains("baz"), "U6 did not stage baz: {names}");
    }

    // U7: without -u, untracked baz is a valid add candidate.
    {
        let repo = committed_top_and_untracked_baz();
        let p = repo.path();
        assert_cli_success(
            &run_libra_command(&["add", "baz", "top"], p),
            "U7 add without -u",
        );
        let cached = run_libra_command(&["diff", "--cached", "--name-only"], p);
        let names = String::from_utf8_lossy(&cached.stdout);
        assert!(names.contains("baz") && names.contains("top"), "{names}");
    }

    // U9: JSON envelope uses LBR-CLI-003.
    {
        let repo = committed_top_and_untracked_baz();
        let p = repo.path();
        let out = run_libra_command(&["--json", "add", "-u", "baz", "top"], p);
        assert!(!out.status.success());
        let blob = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(blob.contains("LBR-CLI-003"), "U9 error code: {blob}");
    }
}

fn one_file_conflict_repo() -> tempfile::TempDir {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("c.txt"), "base\n").unwrap();
    fs::write(p.join("bystander.txt"), "side\n").unwrap();
    fs::write(p.join("k.txt"), "keep\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "c.txt", "bystander.txt", "k.txt"], p),
        "add base",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    assert_cli_success(&run_libra_command(&["branch", "other"], p), "branch other");
    fs::write(p.join("c.txt"), "ours\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "c.txt"], p), "add ours");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ours", "--no-verify"], p),
        "commit ours",
    );
    assert_cli_success(&run_libra_command(&["switch", "other"], p), "switch other");
    fs::write(p.join("c.txt"), "theirs\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "c.txt"], p), "add theirs");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "theirs", "--no-verify"], p),
        "commit theirs",
    );
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    let merge = run_libra_command(&["merge", "other"], p);
    assert!(!merge.status.success(), "expected conflict");
    repo
}

/// AU-02 / M-UNM: staging an unmerged path writes stage 0 and drops 1–3.
#[test]
fn test_add_resolves_unmerged_entries_matrix() {
    // N1: explicit add of a resolved conflict path.
    {
        let repo = one_file_conflict_repo();
        let p = repo.path();
        fs::write(p.join("c.txt"), "resolved\n").unwrap();
        assert_cli_success(&run_libra_command(&["add", "c.txt"], p), "N1 add c.txt");
        assert!(ls_unmerged(p, "c.txt").trim().is_empty(), "N1 no UU");
        assert_eq!(unmerged_line_count(&ls_staged(p, "c.txt")), 1);
        let json = run_libra_command(&["--json", "status", "--short"], p);
        // status --short after resolve should not show UU
        let short = run_libra_command(&["status", "--short"], p);
        let text = String::from_utf8_lossy(&short.stdout);
        assert!(!text.contains("UU c.txt"), "N1 status after add: {text}");
        let _ = json;
    }

    // N2/N3: -A / . / -u also resolve unmerged paths.
    for args in [vec!["add", "-A"], vec!["add", "."], vec!["add", "-u"]] {
        let repo = one_file_conflict_repo();
        let p = repo.path();
        fs::write(p.join("c.txt"), "resolved\n").unwrap();
        assert_cli_success(&run_libra_command(&args, p), &format!("N2/N3 {args:?}"));
        assert!(
            ls_unmerged(p, "c.txt").trim().is_empty(),
            "{args:?} left unmerged"
        );
    }

    // N4: leftover markers are still staged by ordinary add.
    {
        let repo = one_file_conflict_repo();
        let p = repo.path();
        assert_cli_success(
            &run_libra_command(&["add", "-u", "c.txt"], p),
            "N4 add -u with markers",
        );
        assert!(ls_unmerged(p, "c.txt").trim().is_empty());
    }

    // N6: deleted conflict path is removed from the index.
    {
        let repo = one_file_conflict_repo();
        let p = repo.path();
        fs::remove_file(p.join("c.txt")).unwrap();
        assert_cli_success(&run_libra_command(&["add", "-u"], p), "N6 add -u delete");
        assert!(ls_staged(p, "c.txt").trim().is_empty());
    }

    // N7: dry-run previews the unmerged path and does not write.
    {
        let repo = one_file_conflict_repo();
        let p = repo.path();
        fs::write(p.join("c.txt"), "resolved\n").unwrap();
        let before = index_bytes(p);
        let out = run_libra_command(&["add", "-u", "-n"], p);
        assert_cli_success(&out, "N7 dry-run");
        assert_eq!(index_bytes(p), before);
        assert!(!ls_unmerged(p, "c.txt").trim().is_empty());
        let preview = String::from_utf8_lossy(&out.stdout);
        assert!(
            preview.contains("c.txt"),
            "N7 preview includes unmerged path: {preview}"
        );
    }

    // N9: JSON classifies the resolved path as modified.
    {
        let repo = one_file_conflict_repo();
        let p = repo.path();
        fs::write(p.join("c.txt"), "resolved\n").unwrap();
        let json = run_libra_command(&["--json", "add", "c.txt"], p);
        assert_cli_success(&json, "N9 json add");
        let value: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json");
        let modified = value["data"]["modified"].as_array().expect("modified");
        assert!(
            modified.iter().any(|v| v.as_str() == Some("c.txt")),
            "N9 modified: {modified:?}"
        );
        assert!(
            value["data"]["added"]
                .as_array()
                .is_some_and(|a| a.is_empty()),
            "N9 not added"
        );
    }
}

/// AU-02 N5: deleting a bystander during a conflict does not rename-pair.
#[test]
fn test_t2200_add_u_avoids_rename_pairing_on_unmerged_paths() {
    let repo = one_file_conflict_repo();
    let p = repo.path();
    fs::write(p.join("c.txt"), "resolved\n").unwrap();
    fs::remove_file(p.join("bystander.txt")).unwrap();
    assert_cli_success(&run_libra_command(&["add", "-u"], p), "N5 add -u");
    assert!(ls_unmerged(p, "").trim().is_empty(), "N5 no unmerged");
    let listed = run_libra_command(&["ls-files", "bystander.txt", "c.txt"], p);
    assert_cli_success(&listed, "ls-files");
    let text = String::from_utf8_lossy(&listed.stdout);
    assert!(text.contains("c.txt"), "{text}");
    assert!(!text.contains("bystander.txt"), "{text}");
}

/// AU-03 / M-OUT: default add is silent when stdout is not a terminal.
#[test]
fn test_add_default_output_silent_when_not_terminal_matrix() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("a.txt"), "a\n").unwrap();

    // O1: piped CLI stdout is empty on a successful default add.
    let out = run_libra_command(&["add", "a.txt"], p);
    assert_cli_success(&out, "O1 add");
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "O1 stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // O3: -v still prints.
    fs::write(p.join("b.txt"), "b\n").unwrap();
    let verbose = run_libra_command(&["add", "-v", "b.txt"], p);
    assert_cli_success(&verbose, "O3 -v");
    assert!(
        !String::from_utf8_lossy(&verbose.stdout).trim().is_empty(),
        "O3 -v should print"
    );

    // O4: dry-run still prints.
    fs::write(p.join("c.txt"), "c\n").unwrap();
    let dry = run_libra_command(&["add", "-n", "c.txt"], p);
    assert_cli_success(&dry, "O4 dry-run");
    assert!(
        String::from_utf8_lossy(&dry.stdout).contains("c.txt"),
        "O4 dry-run preview"
    );

    // O2: forcing the TTY helper emits the existing summary.
    fs::write(p.join("d.txt"), "d\n").unwrap();
    let tty = run_libra_env(&["add", "d.txt"], p, &[("LIBRA_ADD_TTY", "1")]);
    assert_cli_success(&tty, "O2 LIBRA_ADD_TTY");
    assert!(
        String::from_utf8_lossy(&tty.stdout).contains("d.txt"),
        "O2 tty summary: {}",
        String::from_utf8_lossy(&tty.stdout)
    );
}

/// AU-04 / M-LIT: global `--literal-pathspecs` for `add`.
#[test]
fn test_literal_pathspecs_global_add_matrix() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("x.txt"), "x\n").unwrap();
    fs::write(p.join("*.txt"), "star\n").unwrap();

    // L4: literal mode only stages the file named `*.txt`.
    let out = run_libra_command(&["--literal-pathspecs", "add", "--", "*.txt"], p);
    assert_cli_success(&out, "L4 literal add");
    let cached = run_libra_command(&["diff", "--cached", "--name-only"], p);
    let names = String::from_utf8_lossy(&cached.stdout);
    assert!(names.contains("*.txt"), "{names}");
    assert!(!names.contains("x.txt"), "{names}");

    // L8: flag after the subcommand is accepted.
    let after = run_libra_command(&["add", "--literal-pathspecs", "-n", "--", "x.txt"], p);
    assert_cli_success(&after, "L8 flag after subcommand");

    // L7: --no-literal-pathspecs restores glob.
    let restored = run_libra_command(
        &[
            "--literal-pathspecs",
            "--no-literal-pathspecs",
            "add",
            "-n",
            "--",
            "*.txt",
        ],
        p,
    );
    assert_cli_success(&restored, "L7 restore glob");
    let preview = String::from_utf8_lossy(&restored.stdout);
    assert!(
        preview.contains("x.txt") || preview.contains("*.txt"),
        "L7 glob preview: {preview}"
    );
}

/// M-GUARD G1: with the background index consumer slowed (50ms per update),
/// one `add` of 300 modified files still stages everything without a lock
/// timeout and without a drain warning. The regression it guards (issue #469):
/// the consumer used to hold the repository-wide generation lock while
/// applying queued updates, starving foreground marker publishers.
#[test]
fn test_add_batch_survives_slow_index_consumer() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    for i in 0..300 {
        fs::write(p.join(format!("f{i:03}.txt")), "base\n").unwrap();
    }
    assert_cli_success(&run_libra_command(&["add", "."], p), "stage base files");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base files",
    );
    for i in 0..300 {
        fs::write(p.join(format!("f{i:03}.txt")), "modified\n").unwrap();
    }

    let out = run_libra_env(
        &["add", "."],
        p,
        &[("LIBRA_TEST_OBJECT_INDEX_UPDATE_DELAY_MS", "50")],
    );
    assert_cli_success(&out, "batch add with slow index consumer");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("timed out"),
        "no lock timeout may be reported: {stderr}"
    );
    assert!(
        !stderr.contains("did not drain"),
        "the queued updates must drain within the child's budget: {stderr}"
    );

    let staged = run_libra_command(&["diff", "--cached", "--name-only"], p);
    assert_cli_success(&staged, "list staged files");
    let names = String::from_utf8_lossy(&staged.stdout);
    assert_eq!(
        names.lines().count(),
        300,
        "all 300 modified files must be staged: {names}"
    );
}

/// M-GUARD G3: the issue's original shape — 132 stale zero-byte lock files
/// (one generation lock plus 131 shard locks) — must not break a 33-file
/// batch add. Lock files that merely exist (no live holder) never block.
#[test]
fn test_add_batch_with_stale_lock_files() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    let locks_dir = p.join(".libra").join("object-index-repair-locks");
    fs::create_dir_all(&locks_dir).unwrap();
    fs::write(locks_dir.join("object-index-repair-generation.lock"), b"").unwrap();
    for i in 0..131u32 {
        fs::write(locks_dir.join(format!("{i:04x}.lock")), b"").unwrap();
    }
    // `libra init` leaves an untracked `.libraignore`; commit it so the staged
    // assertion below counts exactly the 33 new files.
    assert_cli_success(
        &run_libra_command(&["add", ".libraignore"], p),
        "stage init ignore file",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    for i in 0..33 {
        fs::write(p.join(format!("g{i:02}.txt")), format!("content {i}\n")).unwrap();
    }

    let out = run_libra_command(&["add", "."], p);
    assert_cli_success(&out, "batch add with 132 stale lock files");
    let staged = run_libra_command(&["diff", "--cached", "--name-only"], p);
    assert_cli_success(&staged, "list staged files");
    let names = String::from_utf8_lossy(&staged.stdout);
    assert_eq!(
        names.lines().count(),
        33,
        "all 33 files must be staged: {names}"
    );
}

/// M-DIAG D1 (R2' shape): when another live process holds the generation
/// lock, `add` times out with a diagnostic that names the holder (pid and
/// purpose) and explains that lock files must not be deleted.
#[test]
fn test_add_lock_timeout_names_foreign_holder() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("held.txt"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "held.txt"], p), "stage base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    fs::write(p.join("held.txt"), "modified\n").unwrap();

    let gen_lock = p
        .join(".libra")
        .join("object-index-repair-locks")
        .join("object-index-repair-generation.lock");
    let mut helper = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "command::add_test::test_add_foreign_lock_holder_helper",
            "--exact",
            "--nocapture",
        ])
        .env("LIBRA_TEST_ADD_LOCK_HOLD_PATH", &gen_lock)
        .spawn()
        .expect("spawn foreign lock holder helper");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while fs::read_to_string(&gen_lock).map_or(true, |c| !c.contains("marker_publication")) {
        assert!(
            std::time::Instant::now() < deadline,
            "helper never wrote holder metadata"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let out = run_libra_command(&["add", "held.txt"], p);
    assert!(
        !out.status.success(),
        "add must fail while the foreign holder holds the generation lock"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("another live Libra process (pid"),
        "{stderr}"
    );
    assert!(stderr.contains("marker_publication"), "{stderr}");
    assert!(
        stderr.contains("must not be deleted"),
        "the D4 lock-file note must be present: {stderr}"
    );

    let _ = helper.kill();
    let _ = helper.wait();
}

/// Helper process for `test_add_lock_timeout_names_foreign_holder`: holds the
/// generation lock with `marker_publication` metadata until killed. Regular
/// test runs return immediately (no env var set).
#[test]
fn test_add_foreign_lock_holder_helper() {
    let Ok(path) = std::env::var("LIBRA_TEST_ADD_LOCK_HOLD_PATH") else {
        return;
    };
    #[cfg(unix)]
    {
        use std::{io::Write, os::fd::AsRawFd};

        let path = std::path::PathBuf::from(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        // SAFETY: flock on an owned descriptor held until the process is killed.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(result, 0, "integration test helper flock must succeed");
        let metadata = format!(
            "{{\"pid\":{},\"purpose\":\"marker_publication\",\"started_at_ms\":{},\"invocation\":\"helper\"}}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        );
        file.set_len(0).unwrap();
        file.write_all(metadata.as_bytes()).unwrap();
        file.flush().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
    #[cfg(not(unix))]
    {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

/// Helper process for the M-WAIT integration tests: holds the generation lock
/// with `marker_publication` metadata for LIBRA_TEST_ADD_LOCK_HOLD_SECS
/// seconds (default 5), then exits and releases the lock. Regular test runs
/// return immediately (no env var set).
#[test]
fn test_add_releasing_lock_holder_helper() {
    let Ok(path) = std::env::var("LIBRA_TEST_ADD_LOCK_HOLD_PATH") else {
        return;
    };
    let hold_secs: u64 = std::env::var("LIBRA_TEST_ADD_LOCK_HOLD_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5);
    #[cfg(unix)]
    {
        use std::{io::Write, os::fd::AsRawFd};

        let path = std::path::PathBuf::from(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        // SAFETY: flock on an owned descriptor held until process exit.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(result, 0, "releasing helper flock must succeed");
        let metadata = format!(
            "{{\"pid\":{},\"purpose\":\"marker_publication\",\"started_at_ms\":{},\"invocation\":\"helper\"}}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        );
        file.set_len(0).unwrap();
        file.write_all(metadata.as_bytes()).unwrap();
        file.flush().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(hold_secs));
    }
    #[cfg(not(unix))]
    {
        std::thread::sleep(std::time::Duration::from_secs(hold_secs));
    }
}

/// M-WAIT W1: a foreign holder that releases after 5 seconds must make `add`
/// wait (10-second budget) and then succeed without a lock timeout.
#[test]
fn test_add_waits_for_foreign_generation_lock_holder() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("held.txt"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "held.txt"], p), "stage base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    fs::write(p.join("held.txt"), "modified\n").unwrap();

    let gen_lock = p
        .join(".libra")
        .join("object-index-repair-locks")
        .join("object-index-repair-generation.lock");
    let mut helper = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "command::add_test::test_add_releasing_lock_holder_helper",
            "--exact",
            "--nocapture",
        ])
        .env("LIBRA_TEST_ADD_LOCK_HOLD_PATH", &gen_lock)
        .env("LIBRA_TEST_ADD_LOCK_HOLD_SECS", "5")
        .spawn()
        .expect("spawn releasing lock holder helper");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while fs::read_to_string(&gen_lock).map_or(true, |c| !c.contains("marker_publication")) {
        assert!(
            std::time::Instant::now() < deadline,
            "helper never wrote holder metadata"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let started = std::time::Instant::now();
    let out = run_libra_command(&["add", "held.txt"], p);
    assert_cli_success(&out, "add must wait out a short foreign holder");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= std::time::Duration::from_secs(3),
        "add must actually have waited for the holder: {elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(12),
        "add must succeed inside the 10s budget: {elapsed:?}"
    );

    let staged = run_libra_command(&["diff", "--cached", "--name-only"], p);
    assert_cli_success(&staged, "list staged files");
    let names = String::from_utf8_lossy(&staged.stdout);
    assert!(names.contains("held.txt"), "{names}");
    let _ = helper.wait();
}

/// M-WAIT W4/W5: while a foreign holder keeps the generation lock busy, the
/// read-only `status` command must not wait for it and must not emit a replay
/// warning; after the holder releases, `add` succeeds.
#[test]
fn test_status_loop_during_batch_add_emits_no_replay_warning() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("held.txt"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "held.txt"], p), "stage base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    fs::write(p.join("held.txt"), "modified\n").unwrap();

    let gen_lock = p
        .join(".libra")
        .join("object-index-repair-locks")
        .join("object-index-repair-generation.lock");
    let mut helper = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "command::add_test::test_add_releasing_lock_holder_helper",
            "--exact",
            "--nocapture",
        ])
        .env("LIBRA_TEST_ADD_LOCK_HOLD_PATH", &gen_lock)
        .env("LIBRA_TEST_ADD_LOCK_HOLD_SECS", "8")
        .spawn()
        .expect("spawn holding lock holder helper");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while fs::read_to_string(&gen_lock).map_or(true, |c| !c.contains("marker_publication")) {
        assert!(
            std::time::Instant::now() < deadline,
            "helper never wrote holder metadata"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    for _ in 0..2 {
        let started = std::time::Instant::now();
        let out = run_libra_command(&["status"], p);
        assert_cli_success(&out, "status must succeed while the lock is busy");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("replay") && !stderr.contains("repair"),
            "no replay warning while the lock is busy: {stderr}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "status must not wait for the busy generation lock"
        );
    }

    let _ = helper.wait();
    let out = run_libra_command(&["add", "held.txt"], p);
    assert_cli_success(&out, "add succeeds after the holder released");
}

/// M-BATCH B1: a 3000-file batch `add` publishes markers in bounded batches —
/// the generation lock acquisition count must stay at ⌈3000/256⌉ + a small
/// constant, not one lock per object.
#[test]
fn test_add_3000_files_bounded_generation_lock_acquisitions() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    for i in 0..3000 {
        fs::write(p.join(format!("f{i:04}.txt")), "base\n").unwrap();
    }
    assert_cli_success(&run_libra_command(&["add", "."], p), "stage base files");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base files",
    );
    for i in 0..3000 {
        fs::write(p.join(format!("f{i:04}.txt")), "modified\n").unwrap();
    }

    let count_path = p.join("generation-lock-count.txt");
    let out = spawn_libra_command_with_env(
        &["add", "."],
        p,
        &[(
            "LIBRA_TEST_OBJECT_INDEX_GENERATION_LOCK_COUNT_PATH",
            count_path.to_str().unwrap(),
        )],
    )
    .wait_with_output()
    .expect("run libra add with the count hook");
    assert_cli_success(&out, "3000-file batch add");
    let count: usize = fs::read_to_string(&count_path)
        .expect("count hook output")
        .trim()
        .parse()
        .expect("count is a number");
    assert!(
        count > 1,
        "batching must acquire more than one generation lock overall: {count}"
    );
    assert!(
        count <= 32,
        "3000 markers must publish in ≤ 32 generation locks, got {count}"
    );
    let staged = run_libra_command(&["diff", "--cached", "--name-only"], p);
    assert_cli_success(&staged, "list staged files");
    let names = String::from_utf8_lossy(&staged.stdout);
    assert_eq!(names.lines().count(), 3000, "all 3000 files must be staged");
}

/// M-EXIT E1: with an ignored path in the mix, `add` stages the rest and
/// exits 1 (after rendering and event dispatch).
#[test]
fn test_add_ignored_with_others_stages_others_and_fails() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join(".libraignore"), "*.log\n").unwrap();
    fs::write(p.join("other.txt"), "untracked\n").unwrap();
    fs::write(p.join("top.log"), "ignored\n").unwrap();

    let out = run_libra_command(&["add", "other.txt", "top.log"], p);
    assert_eq!(
        out.status.code(),
        Some(1),
        "mixed ignored add must exit 1: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let staged = run_libra_command(&["diff", "--cached", "--name-only"], p);
    assert_cli_success(&staged, "list staged files");
    let names = String::from_utf8_lossy(&staged.stdout);
    assert!(names.contains("other.txt"), "{names}");
    assert!(!names.contains("top.log"), "{names}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("the following paths are ignored"),
        "{stderr}"
    );
    assert!(stderr.contains("top.log"), "{stderr}");
}

/// M-EXIT E2: dry-run with a mixed ignored pathspec exits 1 and reports the
/// addable path without touching the index.
#[test]
fn test_add_dry_run_ignored_with_others_fails() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join(".libraignore"), "*.log\n").unwrap();
    fs::write(p.join("other.txt"), "untracked\n").unwrap();
    fs::write(p.join("top.log"), "ignored\n").unwrap();

    let out = run_libra_command(&["add", "-n", "other.txt", "top.log"], p);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("other.txt"), "{stdout}");
    let staged = run_libra_command(&["diff", "--cached", "--name-only"], p);
    assert_cli_success(&staged, "list staged files");
    assert!(
        String::from_utf8_lossy(&staged.stdout).trim().is_empty(),
        "dry-run must leave the index unchanged"
    );
}

/// M-EXIT E4-E7: forms that must NOT flip to exit 1 (directory preview,
/// whole-tree preview, -A/-u preview, forced preview).
#[test]
fn test_add_ignored_report_unaffected_forms() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join(".libraignore"), "*.log\nbuild/\n").unwrap();
    fs::create_dir_all(p.join("dir")).unwrap();
    fs::write(p.join("dir/new.txt"), "untracked\n").unwrap();
    fs::write(p.join("dir/inner.log"), "ignored\n").unwrap();
    fs::write(p.join("track-this"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "track-this"], p), "stage base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    fs::write(p.join("track-this"), "modified\n").unwrap();

    for args in [
        vec!["add", "-n", "dir"],
        vec!["add", "-n", "."],
        vec!["add", "-n", "-A"],
        vec!["add", "-n", "-u"],
        vec!["add", "-f", "-n", "dir", "dir/inner.log"],
    ] {
        let out = run_libra_command(&args, p);
        assert_cli_success(&out, &format!("{args:?} must stay exit 0"));
    }
}

/// M-EXIT E8/E10/E11: --ignore-errors keeps exit 1; --exit-code-on-warning
/// still exits 1 (not 9); running from a subdirectory reports the ignored
/// path and exits 1. Each row uses its own repo so earlier stagings cannot
/// change later rows' candidate sets.
#[test]
fn test_add_ignored_report_flags_matrix() {
    // E8: --ignore-errors
    {
        let repo = tempdir().unwrap();
        let p = repo.path();
        init_repo_via_cli(p);
        configure_identity_via_cli(p);
        fs::write(p.join(".libraignore"), "*.log\n").unwrap();
        fs::write(p.join("other.txt"), "untracked\n").unwrap();
        fs::write(p.join("top.log"), "ignored\n").unwrap();
        let out = run_libra_command(&["add", "--ignore-errors", "other.txt", "top.log"], p);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let staged = run_libra_command(&["diff", "--cached", "--name-only"], p);
        assert!(String::from_utf8_lossy(&staged.stdout).contains("other.txt"));
    }

    // E10: --exit-code-on-warning (dry-run keeps index untouched)
    {
        let repo = tempdir().unwrap();
        let p = repo.path();
        init_repo_via_cli(p);
        configure_identity_via_cli(p);
        fs::write(p.join(".libraignore"), "*.log\n").unwrap();
        fs::write(p.join("other.txt"), "untracked\n").unwrap();
        fs::write(p.join("top.log"), "ignored\n").unwrap();
        let out = run_libra_command(
            &[
                "--exit-code-on-warning",
                "add",
                "-n",
                "other.txt",
                "top.log",
            ],
            p,
        );
        assert_eq!(
            out.status.code(),
            Some(1),
            "exit 1 must beat 9: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // E11: from sub/, mixed ignored path
    {
        let repo = tempdir().unwrap();
        let p = repo.path();
        init_repo_via_cli(p);
        configure_identity_via_cli(p);
        fs::write(p.join(".libraignore"), "*.log\n").unwrap();
        fs::write(p.join("other.txt"), "untracked\n").unwrap();
        fs::create_dir_all(p.join("sub")).unwrap();
        fs::write(p.join("sub/.libraignore"), "local.tmp\n").unwrap();
        fs::write(p.join("sub/local.tmp"), "ignored\n").unwrap();
        let out = run_libra_command(&["add", "-n", "local.tmp", "../other.txt"], &p.join("sub"));
        assert_eq!(
            out.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("local.tmp"), "{stderr}");
    }
}

/// M-IGN O1-O3: the human ignored block has the title, one path per line and
/// the -f hint only (no restore --staged hint); --quiet suppresses stdout but
/// keeps the stderr block and exit 1.
#[test]
fn test_add_ignored_block_hints_and_quiet() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join(".libraignore"), "*.log\n").unwrap();
    fs::write(p.join("other.txt"), "untracked\n").unwrap();
    fs::write(p.join("top.log"), "ignored\n").unwrap();

    let out = run_libra_command(&["add", "-n", "other.txt", "top.log"], p);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning: the following paths are ignored by configured ignore rules:"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Hint: use -f if you really want to add them."),
        "{stderr}"
    );
    assert!(
        !stderr.contains("libra restore --staged"),
        "the stale restore hint must be gone: {stderr}"
    );

    let quiet = run_libra_command(&["add", "--quiet", "-n", "other.txt", "top.log"], p);
    assert_eq!(quiet.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&quiet.stdout).trim().is_empty(),
        "quiet must suppress stdout"
    );
    assert!(
        String::from_utf8_lossy(&quiet.stderr).contains("the following paths are ignored"),
        "quiet must keep the stderr block"
    );
}

/// M-IGN O4: with a mixed ignored report and a real staging, the POST_ADD
/// automation event is dispatched before the non-zero return.
#[tokio::test]
#[serial(cwd)]
async fn test_add_ignored_dispatch_before_exit_one() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());
    fs::write(
        test_dir.path().join(".libra").join("automations.toml"),
        r#"
        [[rules]]
        id = "index_summary"
        trigger = { kind = "vcs", event = "post_add" }
        action = { kind = "prompt", prompt = "summarize staged changes" }
    "#,
    )
    .unwrap();
    fs::write(".libraignore", "*.log\n").unwrap();
    fs::write("other.txt", "content").unwrap();
    fs::write("top.log", "ignored").unwrap();

    let error = add::execute_safe(
        AddArgs {
            pathspec: vec!["other.txt".to_string(), "top.log".to_string()],
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
            resolved: false,
            patch: false,
            auto_advance: false,
            no_auto_advance: false,
        },
        &libra::utils::output::OutputConfig::default(),
    )
    .await
    .expect_err("mixed ignored add must return the silent exit-1 error");
    assert_eq!(error.exit_code(), 1, "{error}");

    let db = get_db_conn_instance().await;
    let rows = AutomationHistory::list_recent(&db, 10).await.unwrap();
    assert!(
        rows.iter().any(|row| row.rule_id == "index_summary"),
        "POST_ADD must be dispatched before the non-zero return: {rows:?}"
    );
}

/// TC-0004 / M-MISS M1: `--dry-run --ignore-missing` classifies a non-existent
/// but ignored pathspec as ignored (exit 1) while leaving the index untouched.
#[test]
fn test_add_dry_run_ignore_missing_ignored_path_tc0004() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join(".libraignore"), "*.log\nignored-file\n").unwrap();
    fs::write(p.join("track-this"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "track-this"], p), "stage base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    fs::write(p.join("track-this"), "modified\n").unwrap();

    let out = run_libra_command(
        &[
            "add",
            "-n",
            "--ignore-missing",
            "track-this",
            "ignored-file",
        ],
        p,
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "ignored missing pathspec must exit 1: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let staged = run_libra_command(&["diff", "--cached", "--name-only"], p);
    assert_cli_success(&staged, "list staged files");
    assert!(
        String::from_utf8_lossy(&staged.stdout).trim().is_empty(),
        "dry-run must leave the index unchanged"
    );
}

/// TC-0005 / M-MISS M1: stdout reports the addable path, stderr's ignored block
/// lists the ignored pathspec, and no `did not match any files` skip line leaks.
#[test]
fn test_add_dry_run_ignore_missing_output_tc0005() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join(".libraignore"), "*.log\nignored-file\n").unwrap();
    fs::write(p.join("track-this"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "track-this"], p), "stage base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    fs::write(p.join("track-this"), "modified\n").unwrap();

    let out = run_libra_command(
        &[
            "add",
            "-n",
            "--ignore-missing",
            "track-this",
            "ignored-file",
        ],
        p,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("track-this"),
        "stdout must report the addable path: {stdout}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("the following paths are ignored"),
        "{stderr}"
    );
    assert!(
        stderr.contains("ignored-file"),
        "ignored block must list the ignored pathspec: {stderr}"
    );
    assert!(
        !stderr.contains("did not match any files"),
        "a spec classified as ignored must not surface a skip line: {stderr}"
    );
}

/// M-MISS M2/M3/M8/M9/M11: rule source (`.libraignore` vs `.gitignore`),
/// negation (`!keep.log`), literal magic, and wildcard pathspecs each classify
/// a non-existent pathspec the same way Git does.
#[test]
fn test_add_ignore_missing_rule_matrix() {
    let setup = |ignore: &str, gitignore: &str| {
        let repo = tempdir().unwrap();
        let p = repo.path();
        init_repo_via_cli(p);
        configure_identity_via_cli(p);
        fs::write(p.join(".libraignore"), ignore).unwrap();
        if !gitignore.is_empty() {
            fs::write(p.join(".gitignore"), gitignore).unwrap();
        }
        fs::write(p.join("track-this"), "base\n").unwrap();
        assert_cli_success(&run_libra_command(&["add", "track-this"], p), "stage base");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
            "commit base",
        );
        fs::write(p.join("track-this"), "modified\n").unwrap();
        repo
    };

    // M2: `*.log` matches a missing `x.log` -> ignored, exit 1.
    {
        let repo = setup("*.log\n", "");
        let p = repo.path();
        let out = run_libra_command(&["add", "-n", "--ignore-missing", "track-this", "x.log"], p);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stderr).contains("x.log"));
    }

    // M3: `!keep.log` negation makes the missing spec addable-or-skipped (not
    // ignored) -> skip warning, exit 0.
    {
        let repo = setup("*.log\n!keep.log\n", "");
        let p = repo.path();
        let out = run_libra_command(
            &["add", "-n", "--ignore-missing", "track-this", "keep.log"],
            p,
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stderr).contains("--ignore-missing"));
    }

    // M8: `.gitignore` source also classifies a missing spec as ignored.
    {
        let repo = setup("", "gi.txt\n");
        let p = repo.path();
        let out = run_libra_command(
            &["add", "-n", "--ignore-missing", "track-this", "gi.txt"],
            p,
        );
        assert_eq!(
            out.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stderr).contains("gi.txt"));
    }

    // M9: `:(literal)x.log` strips magic and still matches `*.log`.
    {
        let repo = setup("*.log\n", "");
        let p = repo.path();
        let out = run_libra_command(
            &[
                "add",
                "-n",
                "--ignore-missing",
                "track-this",
                ":(literal)x.log",
            ],
            p,
        );
        assert_eq!(
            out.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stderr).contains("x.log"));
    }

    // M11: a wildcard pathspec (`ign*`) with no ignore-rule match is skipped,
    // not ignored -> exit 0.
    {
        let repo = setup("*.log\n", "");
        let p = repo.path();
        for spec in ["ign*", ":(glob)ign*"] {
            let out = run_libra_command(&["add", "-n", "--ignore-missing", "track-this", spec], p);
            assert_eq!(
                out.status.code(),
                Some(0),
                "{spec}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

/// M-MISS M12/M13: `--force` skips the ignore classification; a mixed
/// force-free batch still reports x.log/y.log as ignored while `nope` stays a
/// skip warning.
#[test]
fn test_add_ignore_missing_force_and_mixed() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join(".libraignore"), "*.log\n").unwrap();
    fs::write(p.join("track-this"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "track-this"], p), "stage base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    fs::write(p.join("track-this"), "modified\n").unwrap();

    // M12: with -f, x.log is NOT ignore-classified -> skip warning, exit 0.
    let forced = run_libra_command(
        &["add", "-f", "-n", "--ignore-missing", "track-this", "x.log"],
        p,
    );
    assert_eq!(
        forced.status.code(),
        Some(0),
        "force must skip ignore classification: {}",
        String::from_utf8_lossy(&forced.stderr)
    );

    // M13: without -f, x.log/y.log are ignored; nope stays a skip warning.
    let mixed = run_libra_command(
        &[
            "add",
            "-n",
            "--ignore-missing",
            "track-this",
            "x.log",
            "y.log",
            "nope",
        ],
        p,
    );
    assert_eq!(
        mixed.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&mixed.stderr)
    );
    let stderr = String::from_utf8_lossy(&mixed.stderr);
    assert!(stderr.contains("x.log"), "{stderr}");
    assert!(stderr.contains("y.log"), "{stderr}");
    assert!(
        stderr.contains("did not match any files"),
        "un-ignored missing path keeps its skip warning: {stderr}"
    );
}

/// M-MISS M14: only ignored missing pathspecs (nothing addable) collapse into
/// the `LBR-ADD-001` / exit 128 contract.
#[test]
fn test_add_ignore_missing_only_ignored_is_add_001() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join(".libraignore"), "*.log\n").unwrap();
    fs::create_dir_all(p.join("sub")).unwrap();
    fs::write(p.join("sub/.libraignore"), "local.tmp\n").unwrap();

    let out = run_libra_command(
        &["add", "-n", "--ignore-missing", "local.tmp", "../x.log"],
        &p.join("sub"),
    );
    assert_eq!(
        out.status.code(),
        Some(128),
        "only-ignored must be LBR-ADD-001/128: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------
// CH-01 (plan-20260918): `add --chmod` refuses non-regular index entries.
// ---------------------------------------------------------------------------

/// M-CHMOD R1-R3 / TC-0008: a symlink index entry has no executable bit, so
/// `--chmod` refuses it (exit 1, `cannot chmod` on stderr) and the index is left
/// unchanged — dry-run and real mode alike.
#[cfg(unix)]
#[test]
fn test_add_chmod_rejects_nonregular_dry_run_tc0008() {
    use std::os::unix::fs::symlink;

    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    symlink("target", p.join("foo4")).unwrap();
    assert_cli_success(&run_libra_command(&["add", "foo4"], p), "stage symlink");

    let before = run_libra_command(&["ls-files", "-s", "foo4"], p);
    assert_cli_success(&before, "ls-files before");
    let before_out = String::from_utf8_lossy(&before.stdout).to_string();
    assert!(
        before_out.starts_with("120000"),
        "fixture must be a symlink: {before_out}"
    );

    // R1/R2: dry-run refuses with a per-path, per-flip error and no index write.
    for (mode_arg, flip) in [("--chmod=+x", "+x"), ("--chmod=-x", "-x")] {
        let out = run_libra_command(&["add", mode_arg, "--dry-run", "foo4"], p);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{mode_arg}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("foo4"), "{stderr}");
        assert!(stderr.contains(flip), "{stderr}");
        assert!(stderr.contains("cannot chmod"), "{stderr}");
    }

    // R3: real mode refuses too and the entry stays 120000.
    let real = run_libra_command(&["add", "--chmod=+x", "foo4"], p);
    assert_eq!(
        real.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&real.stderr)
    );
    assert!(
        String::from_utf8_lossy(&real.stderr).contains("cannot chmod +x"),
        "{}",
        String::from_utf8_lossy(&real.stderr)
    );
    let after = run_libra_command(&["ls-files", "-s", "foo4"], p);
    assert_cli_success(&after, "ls-files after");
    assert_eq!(
        String::from_utf8_lossy(&after.stdout),
        before_out,
        "a refused entry must be unchanged"
    );
}

/// M-CHMOD R4-R6: a refusal never blocks the regular files in the same
/// pathspec — they still get the mode, while the symlink is reported.
#[cfg(unix)]
#[test]
fn test_add_chmod_rejects_nonregular_but_updates_others() {
    use std::os::unix::fs::symlink;

    // Each row gets its own repo so an earlier mode flip cannot change a later
    // row's expected candidate set (same convention as the M-EXIT flag matrix).
    let setup = || {
        let repo = tempdir().unwrap();
        let p = repo.path();
        init_repo_via_cli(p);
        configure_identity_via_cli(p);
        fs::write(p.join("reg"), "content\n").unwrap();
        symlink("target", p.join("foo4")).unwrap();
        fs::create_dir_all(p.join("dir")).unwrap();
        fs::write(p.join("dir/a"), "a\n").unwrap();
        symlink("target", p.join("dir/l")).unwrap();
        for spec in ["reg", "foo4", "dir"] {
            assert_cli_success(&run_libra_command(&["add", spec], p), "stage fixture");
        }
        repo
    };

    // R4: mixed regular + symlink in real mode.
    {
        let repo = setup();
        let p = repo.path();
        let out = run_libra_command(&["add", "--chmod=+x", "reg", "foo4"], p);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("foo4"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let reg = run_libra_command(&["ls-files", "-s", "reg"], p);
        assert!(
            String::from_utf8_lossy(&reg.stdout).starts_with("100755"),
            "reg must become executable: {}",
            String::from_utf8_lossy(&reg.stdout)
        );
    }

    // R5: dry-run mixed reports `reg` and refuses `foo4`.
    {
        let repo = setup();
        let p = repo.path();
        let dry = run_libra_command(&["add", "--chmod=+x", "--dry-run", "reg", "foo4"], p);
        assert_eq!(
            dry.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&dry.stderr)
        );
        assert!(
            String::from_utf8_lossy(&dry.stdout).contains("reg"),
            "{}",
            String::from_utf8_lossy(&dry.stdout)
        );
    }

    // R6: a directory pathspec updates the regular child and refuses the symlink.
    {
        let repo = setup();
        let p = repo.path();
        let dir = run_libra_command(&["add", "--chmod=+x", "dir"], p);
        assert_eq!(
            dir.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&dir.stderr)
        );
        assert!(
            String::from_utf8_lossy(&dir.stderr).contains("dir/l"),
            "{}",
            String::from_utf8_lossy(&dir.stderr)
        );
        let dir_a = run_libra_command(&["ls-files", "-s", "dir/a"], p);
        assert!(
            String::from_utf8_lossy(&dir_a.stdout).starts_with("100755"),
            "dir/a must become executable: {}",
            String::from_utf8_lossy(&dir_a.stdout)
        );
    }
}

/// M-CHMOD R7-R8: a typechange staged in the same command is judged by the
/// post-staging index — dry-run leaves the unindexed link alone (exit 0), the
/// real run stages the symlink and then refuses to chmod it (exit 1).
#[cfg(unix)]
#[test]
fn test_add_chmod_staged_symlink_is_rejected() {
    use std::os::unix::fs::symlink;

    // R7: untracked symlink.
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    symlink("target", p.join("newlink")).unwrap();

    let dry = run_libra_command(&["add", "--chmod=+x", "--dry-run", "newlink"], p);
    assert_eq!(
        dry.status.code(),
        Some(0),
        "dry-run must not judge an unindexed link: {}",
        String::from_utf8_lossy(&dry.stderr)
    );
    let real = run_libra_command(&["add", "--chmod=+x", "newlink"], p);
    assert_eq!(
        real.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&real.stderr)
    );
    let entry = run_libra_command(&["ls-files", "-s", "newlink"], p);
    assert!(
        String::from_utf8_lossy(&entry.stdout).starts_with("120000"),
        "newlink must be staged as a symlink: {}",
        String::from_utf8_lossy(&entry.stdout)
    );

    // R8: tracked regular file swapped for a symlink in the worktree.
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("swap"), "regular\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "swap"], p), "stage regular");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit",
    );
    fs::remove_file(p.join("swap")).unwrap();
    symlink("target", p.join("swap")).unwrap();

    let dry = run_libra_command(&["add", "--chmod=+x", "--dry-run", "swap"], p);
    assert_eq!(
        dry.status.code(),
        Some(0),
        "dry-run typechange is silent: {}",
        String::from_utf8_lossy(&dry.stderr)
    );
    let real = run_libra_command(&["add", "--chmod=+x", "swap"], p);
    assert_eq!(
        real.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&real.stderr)
    );
    let entry = run_libra_command(&["ls-files", "-s", "swap"], p);
    assert!(
        String::from_utf8_lossy(&entry.stdout).starts_with("120000"),
        "swap must be staged as a symlink: {}",
        String::from_utf8_lossy(&entry.stdout)
    );
}

/// M-CHMOD R9: an index gitlink entry (mode 160000) is refused like a symlink.
#[test]
fn test_add_chmod_rejects_gitlink_entry() {
    use git_internal::{
        hash::ObjectHash,
        internal::index::{Index, IndexEntry},
    };

    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);

    let index_path = p.join(".libra/index");
    let mut index = Index::load(&index_path).expect("load index");
    let mut entry = IndexEntry::new_from_blob("gl".to_string(), ObjectHash::default(), 0);
    entry.mode = 0o160000;
    index.add(entry);
    index.save(&index_path).expect("save index");

    let before = run_libra_command(&["ls-files", "-s", "gl"], p);
    assert!(
        String::from_utf8_lossy(&before.stdout).starts_with("160000"),
        "fixture must be a gitlink: {}",
        String::from_utf8_lossy(&before.stdout)
    );

    let out = run_libra_command(&["add", "--chmod=+x", "--dry-run", "gl"], p);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("gl"), "{stderr}");
    assert!(stderr.contains("cannot chmod +x"), "{stderr}");
}

/// M-CHMOD R10: `--ignore-errors` does not suppress the refusal,
/// `--quiet` keeps the stderr error line, and `--exit-code-on-warning` yields 1
/// (not 9).
#[cfg(unix)]
#[test]
fn test_add_chmod_rejection_flag_matrix() {
    use std::os::unix::fs::symlink;

    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("reg"), "content\n").unwrap();
    symlink("target", p.join("foo4")).unwrap();
    for spec in ["reg", "foo4"] {
        assert_cli_success(&run_libra_command(&["add", spec], p), "stage fixture");
    }

    let ignored = run_libra_command(&["add", "--chmod=+x", "--ignore-errors", "reg", "foo4"], p);
    assert_eq!(
        ignored.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&ignored.stderr)
    );
    let reg = run_libra_command(&["ls-files", "-s", "reg"], p);
    assert!(String::from_utf8_lossy(&reg.stdout).starts_with("100755"));

    let quiet = run_libra_command(&["add", "--chmod=+x", "--dry-run", "--quiet", "foo4"], p);
    assert_eq!(quiet.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&quiet.stderr).contains("cannot chmod +x"),
        "{}",
        String::from_utf8_lossy(&quiet.stderr)
    );

    let warned = run_libra_command(
        &[
            "--exit-code-on-warning",
            "add",
            "--chmod=+x",
            "--dry-run",
            "foo4",
        ],
        p,
    );
    assert_eq!(
        warned.status.code(),
        Some(1),
        "the refusal outranks the warning exit: {}",
        String::from_utf8_lossy(&warned.stderr)
    );
}

/// M-CHMOD R12: no-op and invalid-value forms keep their existing behavior.
#[cfg(unix)]
#[test]
fn test_add_chmod_noop_cases_unchanged() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("exe"), "exe\n").unwrap();
    fs::write(p.join("reg"), "reg\n").unwrap();
    fs::write(p.join("gone"), "gone\n").unwrap();
    for spec in ["exe", "reg", "gone"] {
        assert_cli_success(&run_libra_command(&["add", spec], p), "stage fixture");
    }
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit",
    );

    // Make `exe` executable, then a second `+x` is a no-op.
    assert_cli_success(
        &run_libra_command(&["add", "--chmod=+x", "exe"], p),
        "set exe +x",
    );
    assert_cli_success(
        &run_libra_command(&["add", "--chmod=+x", "exe"], p),
        "already-executable no-op",
    );
    // `-x` against an already-100644 entry is a no-op.
    assert_cli_success(
        &run_libra_command(&["add", "--chmod=-x", "--dry-run", "reg"], p),
        "clear-bit no-op",
    );
    // A tracked path deleted from the worktree stages its deletion, no refusal.
    fs::remove_file(p.join("gone")).unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "--chmod=+x", "gone"], p),
        "deleted tracked path",
    );
    // An invalid value keeps the existing usage error.
    let bogus = run_libra_command(&["add", "--chmod=bogus", "reg"], p);
    assert_eq!(bogus.status.code(), Some(129), "invalid --chmod value");
}

/// CH-02 (plan-20260918): `add --chmod=+x` / `-x` with no pathspec is a
/// successful no-op — exit 0, no index write, no object write, and no
/// `chmod_rejected` in the JSON envelope. An invalid value is still a usage
/// error.
#[test]
fn test_add_chmod_empty_pathspec_is_noop() {
    fn object_file_count(root: &std::path::Path) -> usize {
        let mut count = 0usize;
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    count += 1;
                }
            }
        }
        count
    }

    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("tracked.txt"), "content\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "tracked.txt"], p), "stage base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit",
    );

    // Warm up once: `commit` leaves a few objects pending in the storage
    // batch, which the next `add` invocation flushes. Settle that before taking
    // the baseline so the assertion isolates the no-op behavior itself.
    assert_cli_success(
        &run_libra_command(&["add", "--chmod=+x"], p),
        "warm-up no-op",
    );

    let objects_root = p.join(".libra/objects");
    let objects_before = object_file_count(&objects_root);
    let index_before = fs::read(p.join(".libra/index")).unwrap();

    for args in [
        vec!["add", "--chmod=+x"],
        vec!["add", "--chmod=-x"],
        vec!["add", "--chmod=+x", "--dry-run"],
    ] {
        let out = run_libra_command(&args, p);
        assert!(
            out.status.success(),
            "{args:?} must be a no-op success: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    assert_eq!(
        object_file_count(&objects_root),
        objects_before,
        "no object writes"
    );
    assert_eq!(
        fs::read(p.join(".libra/index")).unwrap(),
        index_before,
        "no index writes"
    );

    // JSON: ok, and no `chmod_rejected` key for a no-op.
    let json = run_libra_command(&["--json", "add", "--chmod=+x"], p);
    assert!(
        json.status.success(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    let parsed = parse_json_stdout(&json);
    assert_eq!(parsed["ok"], true);
    assert!(
        parsed["data"].get("chmod_rejected").is_none(),
        "no chmod_rejected for a no-op: {parsed}"
    );

    // An invalid value without a pathspec stays a usage error.
    let bogus = run_libra_command(&["add", "--chmod=bogus"], p);
    assert_eq!(
        bogus.status.code(),
        Some(129),
        "{}",
        String::from_utf8_lossy(&bogus.stderr)
    );
}

/// PSF-01 (plan-20260918) / M-PSF P1/P4/P5/P10/P11: `--pathspec-from-file=-`
/// reads stdin, the delimiter modes match Git, and every failure path stays
/// zero-write with the documented exit code.
#[test]
fn test_add_pathspec_from_file_stdin_and_delimiters() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("a.txt"), "a\n").unwrap();
    fs::write(p.join("b.txt"), "b\n").unwrap();
    // A worktree file literally named `-`: stdin must win over it.
    fs::write(p.join("-"), "b.txt\n").unwrap();

    // P1: LF stdin stages a.txt and never opens the `-` file.
    let p1 = run_libra_command_with_stdin(&["add", "--pathspec-from-file=-"], p, "a.txt\n");
    assert!(
        p1.status.success(),
        "{}",
        String::from_utf8_lossy(&p1.stderr)
    );
    let staged = run_libra_command(&["diff", "--cached", "--name-only"], p);
    let staged_out = String::from_utf8_lossy(&staged.stdout).to_string();
    assert!(staged_out.contains("a.txt"), "{staged_out}");
    assert!(
        !staged_out.contains("b.txt"),
        "the `-` file must not be read: {staged_out}"
    );

    // P4: CRLF is accepted and the CR is stripped.
    let p4 = run_libra_command_with_stdin(&["add", "--pathspec-from-file=-"], p, "b.txt\r\n");
    assert!(
        p4.status.success(),
        "{}",
        String::from_utf8_lossy(&p4.stderr)
    );

    // P5: NUL mode keeps the CR, so the whole line is one unmatched path.
    let p5 = run_libra_command_with_stdin(
        &["add", "--pathspec-from-file=-", "--pathspec-file-nul"],
        p,
        "a.txt\r\n",
    );
    assert_ne!(p5.status.code(), Some(0), "an unmatched CR path must fail");

    // P10: an empty list falls through to the empty-pathspec usage error.
    let p10 = run_libra_command_with_stdin(&["add", "--pathspec-from-file=-"], p, "");
    assert_eq!(
        p10.status.code(),
        Some(129),
        "{}",
        String::from_utf8_lossy(&p10.stderr)
    );

    // P11: a missing file keeps its 128 + LBR-IO-001 contract.
    let p11 = run_libra_command(&["add", "--pathspec-from-file=nope.list"], p);
    assert_eq!(
        p11.status.code(),
        Some(128),
        "{}",
        String::from_utf8_lossy(&p11.stderr)
    );
    assert!(String::from_utf8_lossy(&p11.stderr).contains("LBR-IO-001"));

    // Non-UTF-8 content is a hard 128 + LBR-IO-001, never a silent skip.
    fs::write(p.join("bad.list"), b"\xff\xfe").unwrap();
    let bad = run_libra_command(&["add", "--pathspec-from-file=bad.list"], p);
    assert_eq!(
        bad.status.code(),
        Some(128),
        "{}",
        String::from_utf8_lossy(&bad.stderr)
    );
    assert!(String::from_utf8_lossy(&bad.stderr).contains("LBR-IO-001"));

    // P3 (PSF-01 review P1-2): `-u` combined with `--pathspec-from-file`.
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit fixture",
    );
    fs::write(p.join("a.txt"), "a2\n").unwrap();
    let p3 = run_libra_command_with_stdin(&["add", "-u", "--pathspec-from-file=-"], p, "a.txt\n");
    assert!(
        p3.status.success(),
        "{}",
        String::from_utf8_lossy(&p3.stderr)
    );
    let staged = run_libra_command(&["diff", "--cached", "--name-only"], p);
    assert!(
        String::from_utf8_lossy(&staged.stdout).contains("a.txt"),
        "{}",
        String::from_utf8_lossy(&staged.stdout)
    );
}

/// PSF-02 (plan-20260918) / M-PSF P6/P7: non-NUL `--pathspec-from-file`
/// decodes C-style quoted lines; an unterminated quote fails closed with a
/// from-file diagnostic and zero writes.
#[test]
fn test_add_pathspec_from_file_cquote() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("qu\"ote.txt"), "q\n").unwrap();
    fs::write(p.join("we ird.txt"), "w\n").unwrap();

    // P6: C-quoted lines decode to the real file names.
    let out = run_libra_command_with_stdin(
        &["add", "--pathspec-from-file=-"],
        p,
        "\"qu\\\"ote.txt\"\n\"we ird.txt\"\n",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let index_bytes = fs::read(p.join(".libra/index")).unwrap();
    let staged = |needle: &[u8]| index_bytes.windows(needle.len()).any(|w| w == needle);
    assert!(staged(b"qu\"ote.txt"), "the quoted name must be staged");
    assert!(staged(b"we ird.txt"), "the spaced name must be staged");

    // P7: an unterminated quote is 128 with a from-file diagnostic, zero writes.
    let before = run_libra_command(&["diff", "--cached", "--name-only"], p);
    let before_out = String::from_utf8_lossy(&before.stdout).to_string();
    let bad = run_libra_command_with_stdin(&["add", "--pathspec-from-file=-"], p, "\"we ird.txt\n");
    assert_eq!(
        bad.status.code(),
        Some(128),
        "{}",
        String::from_utf8_lossy(&bad.stderr)
    );
    let stderr = String::from_utf8_lossy(&bad.stderr);
    assert!(stderr.contains("badly quoted"), "{stderr}");
    assert!(stderr.contains("--pathspec-from-file"), "{stderr}");
    let after = run_libra_command(&["diff", "--cached", "--name-only"], p);
    assert_eq!(
        String::from_utf8_lossy(&after.stdout),
        before_out,
        "malformed quoting must write nothing"
    );
}

/// PSF-03 (plan-20260918) / M-PSF P8/P9: `--pathspec-from-file` refuses an
/// interactive patch mode or command-line pathspecs with 129 + `LBR-CLI-002`,
/// before any write.
#[test]
fn test_add_pathspec_from_file_rejects_interactive() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("a.txt"), "a\n").unwrap();
    fs::write(p.join("list.txt"), "a.txt\n").unwrap();

    // `-p/--patch` is mutually exclusive.
    let patch = run_libra_command(&["add", "--pathspec-from-file=list.txt", "-p"], p);
    assert_eq!(
        patch.status.code(),
        Some(129),
        "{}",
        String::from_utf8_lossy(&patch.stderr)
    );
    let stderr = String::from_utf8_lossy(&patch.stderr);
    assert!(stderr.contains("cannot be used together"), "{stderr}");
    assert!(stderr.contains("LBR-CLI-002"), "{stderr}");

    // Command-line pathspecs are mutually exclusive, with Git's wording.
    let positional = run_libra_command(&["add", "--pathspec-from-file=list.txt", "a.txt"], p);
    assert_eq!(
        positional.status.code(),
        Some(129),
        "{}",
        String::from_utf8_lossy(&positional.stderr)
    );
    let stderr = String::from_utf8_lossy(&positional.stderr);
    assert!(
        stderr.contains("'--pathspec-from-file' and pathspec arguments cannot be used together"),
        "{stderr}"
    );
    assert!(stderr.contains("LBR-CLI-002"), "{stderr}");

    // `--edit` is unknown for `add`, so clap rejects it as a usage error (129).
    let edit = run_libra_command(&["add", "--pathspec-from-file=list.txt", "--edit"], p);
    assert_eq!(
        edit.status.code(),
        Some(129),
        "{}",
        String::from_utf8_lossy(&edit.stderr)
    );

    // `--interactive` keeps its own declined-flag refusal (ADR-PSF-03 item 3).
    let interactive = run_libra_command(
        &["add", "--pathspec-from-file=list.txt", "--interactive"],
        p,
    );
    assert_eq!(
        interactive.status.code(),
        Some(128),
        "the declined-interactive refusal fires first: {}",
        String::from_utf8_lossy(&interactive.stderr)
    );
    assert!(
        String::from_utf8_lossy(&interactive.stderr).contains("not supported"),
        "{}",
        String::from_utf8_lossy(&interactive.stderr)
    );

    // Every rejected combination left the index untouched.
    let staged = run_libra_command(&["diff", "--cached", "--name-only"], p);
    assert!(
        String::from_utf8_lossy(&staged.stdout).trim().is_empty(),
        "rejected combinations must write nothing"
    );
}
