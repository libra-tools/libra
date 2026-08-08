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
#[serial]
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
    })
    .await;

    // Verify the file was added to index.
    let changes = changes_to_be_committed().await;

    assert!(changes.new.iter().any(|x| x.to_str().unwrap() == file_path));
}

#[tokio::test]
#[serial]
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
        },
        &OutputConfig::default(),
    )
    .await
    .expect_err("marker registration failure must be returned");

    assert_eq!(error.stable_code(), StableErrorCode::IoWriteFailed);
    assert!(
        error
            .to_string()
            .contains("failed to store object for 'marker-failure.txt'"),
        "unexpected error: {error}"
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
    })
    .await;

    // Verify the file was not actually added to index
    let changes = changes_to_be_staged().unwrap();
    assert!(changes.new.iter().any(|x| x.to_str().unwrap() == file_path));
}

/// Scenario: in-process `add::execute` with no pathspec and no `--all`
/// must not silently stage anything. The index should be empty after the
/// call. Boundary condition: the in-process API does not surface CLI exit
/// codes, so the assertion is on side effects only.
#[tokio::test]
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
async fn test_add_pathspec_from_file_newline_stages_listed_paths() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    fs::write("file1.txt", "one\n").unwrap();
    fs::write("file2.txt", "two\n").unwrap();
    fs::write("file3.txt", "three\n").unwrap();
    // file1 via the file list, file3 via the CLI pathspec; file2 in neither.
    fs::write("paths.txt", "file1.txt\n").unwrap();

    add::execute(AddArgs {
        pathspec: vec![String::from("file3.txt")],
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
    })
    .await;

    let changes = changes_to_be_committed().await;
    let staged = |name: &str| changes.new.iter().any(|x| x.to_str().unwrap() == name);
    assert!(
        staged("file1.txt"),
        "file1.txt (from file list) should be staged"
    );
    assert!(
        staged("file3.txt"),
        "file3.txt (from CLI pathspec) should be staged"
    );
    assert!(!staged("file2.txt"), "file2.txt should NOT be staged");
}

/// `--pathspec-from-file` with `--pathspec-file-nul` reads a NUL-separated list.
#[tokio::test]
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
#[serial]
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
