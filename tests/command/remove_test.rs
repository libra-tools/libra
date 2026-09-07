//! Tests rm command removing files from the index and working tree while respecting flags.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::{fs, io::Write, path::PathBuf};

use super::*;

// Except for the force test, all tests must also include checking for the presence of
// a force situation. Because under normal circumstances, if the commit stage and the working
// area are not consistent, deletion is prohibited.

/// Helper function to create a file with content.
fn create_file(path: &str, content: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut file = fs::File::create(&path).unwrap();
    file.write_all(content.as_bytes()).unwrap();
    path
}

#[test]
fn test_remove_cli_missing_pathspec_returns_cli_exit_code() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["rm", "no-such.txt"], repo.path());

    assert_eq!(output.status.code(), Some(129));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("fatal: pathspec 'no-such.txt' did not match any files")
    );
}

#[test]
fn test_remove_json_reports_successful_file_removal() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "rm", "tracked.txt"], repo.path());

    assert_cli_success(&output, "json rm tracked file");
    assert!(
        output.stderr.is_empty(),
        "json rm should keep stderr clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed = parse_json_stdout(&output);
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["command"], "rm");
    assert_eq!(
        parsed["data"]["pathspecs"],
        serde_json::json!(["tracked.txt"])
    );
    assert_eq!(parsed["data"]["cached"], false);
    assert_eq!(parsed["data"]["recursive"], false);
    assert_eq!(parsed["data"]["forced"], false);
    assert_eq!(parsed["data"]["dry_run"], false);
    let paths = parsed["data"]["paths"]
        .as_array()
        .expect("paths should be an array");
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0]["path"], "tracked.txt");
    assert_eq!(paths[0]["removed_from_index"], true);
    assert_eq!(paths[0]["removed_from_disk"], true);
    assert!(
        parsed["data"]["directories"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "file removal should not report directories: {parsed}",
    );
    assert!(
        !repo.path().join("tracked.txt").exists(),
        "rm without --cached should remove the tracked file from disk"
    );
}

#[test]
fn test_remove_machine_dry_run_reports_single_json_line_without_side_effects() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(
        &["--machine", "rm", "--dry-run", "tracked.txt"],
        repo.path(),
    );

    assert_cli_success(&output, "machine rm dry-run");
    assert!(
        output.stderr.is_empty(),
        "machine rm should keep stderr clean: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 1, "machine output should be one JSON line");
    let parsed: serde_json::Value = serde_json::from_str(lines[0])
        .unwrap_or_else(|e| panic!("expected machine JSON line, got: {}\nerror: {e}", lines[0]));
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["command"], "rm");
    assert_eq!(parsed["data"]["dry_run"], true);
    let paths = parsed["data"]["paths"]
        .as_array()
        .expect("paths should be an array");
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0]["path"], "tracked.txt");
    assert_eq!(paths[0]["removed_from_index"], false);
    assert_eq!(paths[0]["removed_from_disk"], false);
    assert!(
        repo.path().join("tracked.txt").exists(),
        "rm --dry-run must keep the tracked file on disk"
    );
}

#[tokio::test]
#[serial(cwd)]
/// Tests the basic remove functionality by removing a single file
async fn test_remove_single_file() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a file and add it to index
    let file_path = create_file("test_file.txt", "Test content");

    add::execute(AddArgs {
        pathspec: vec![String::from("test_file.txt")],
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

    // Make sure the file exists
    assert!(file_path.exists(), "File should exist before removal");

    // Remove the file
    let mut args = RemoveArgs {
        pathspec: vec![String::from("test_file.txt")],
        cached: false,
        recursive: false,
        force: false,
        dry_run: false,
        ignore_unmatch: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        sparse: false,
    };
    remove::execute(args.clone()).await;
    assert!(
        file_path.exists(),
        "File should exist after removal if force is false"
    );
    args.force = true;
    remove::execute(args).await;
    // Verify the file was removed from the filesystem
    assert!(
        !file_path.exists(),
        "File should be removed from filesystem"
    );

    // Verify file is no longer in the index
    let changes = changes_to_be_staged().unwrap();
    assert!(
        !changes
            .new
            .iter()
            .any(|x| x.to_str().unwrap() == "test_file.txt"),
        "File should not appear in changes as new"
    );
    assert!(
        !changes
            .modified
            .iter()
            .any(|x| x.to_str().unwrap() == "test_file.txt"),
        "File should not appear in changes as modified"
    );
    assert!(
        !changes
            .deleted
            .iter()
            .any(|x| x.to_str().unwrap() == "test_file.txt"),
        "File should not appear in changes as deleted"
    );
}

#[tokio::test]
#[serial(cwd)]
/// Tests removing a file with --cached flag, which only removes from the index but keeps the file
async fn test_remove_cached() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a file and add it to index
    let file_path = create_file("test_file.txt", "Test content");

    add::execute(AddArgs {
        pathspec: vec![String::from("test_file.txt")],
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

    // Make sure the file exists
    assert!(file_path.exists(), "File should exist before removal");

    // Remove the file with --cached flag
    let args = RemoveArgs {
        pathspec: vec![String::from("test_file.txt")],
        cached: true,
        recursive: false,
        force: false,
        dry_run: false,
        ignore_unmatch: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        sparse: false,
    };
    remove::execute(args).await;

    // Verify the file still exists in the filesystem
    assert!(file_path.exists(), "File should still exist in filesystem");

    // Verify file appears as new (untracked) in the index
    let changes = changes_to_be_staged().unwrap();
    assert!(
        changes
            .new
            .iter()
            .any(|x| x.to_str().unwrap() == "test_file.txt"),
        "File should appear in changes as new/untracked"
    );
}

#[tokio::test]
#[serial(cwd)]
/// Tests recursive removal of a directory
async fn test_remove_directory_recursive() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a directory with files
    let file1 = create_file("test_dir/file1.txt", "File 1 content");
    let file2 = create_file("test_dir/file2.txt", "File 2 content");
    let file3 = create_file("test_dir/subdir/file3.txt", "File 3 content");

    // Add all files to the index
    add::execute(AddArgs {
        pathspec: vec![String::from("test_dir")],
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

    // Make sure the directory and files exist
    assert!(fs::metadata("test_dir").is_ok(), "Directory should exist");
    assert!(file1.exists(), "File 1 should exist");
    assert!(file2.exists(), "File 2 should exist");
    assert!(file3.exists(), "File 3 should exist");

    // Remove the directory recursively
    let mut args = RemoveArgs {
        pathspec: vec![String::from("test_dir")],
        cached: false,
        recursive: true,
        force: false,
        dry_run: false,
        ignore_unmatch: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        sparse: false,
    };
    remove::execute(args.clone()).await;
    // Verify the directory and files still exists if force is false
    assert!(
        fs::metadata("test_dir").is_ok(),
        "Directory should still exist"
    );
    assert!(file1.exists(), "File 1 should still exist");
    assert!(file2.exists(), "File 2 should still exist");
    assert!(file3.exists(), "File 3 should still exist");

    args.force = true;
    remove::execute(args).await;
    // Verify the directory and files were removed if force is true
    assert!(
        fs::metadata("test_dir").is_err(),
        "Directory should be removed"
    );
    assert!(!file1.exists(), "File 1 should be removed");
    assert!(!file2.exists(), "File 2 should be removed");
    assert!(!file3.exists(), "File 3 should be removed");

    // Verify files are no longer in the index
    let changes = changes_to_be_staged().unwrap();
    for file in &[file1, file2, file3] {
        let file_str = file.to_str().unwrap();
        assert!(
            !changes.new.iter().any(|x| x.to_str().unwrap() == file_str),
            "File should not appear in changes as new"
        );
        assert!(
            !changes
                .modified
                .iter()
                .any(|x| x.to_str().unwrap() == file_str),
            "File should not appear in changes as modified"
        );
        assert!(
            !changes
                .deleted
                .iter()
                .any(|x| x.to_str().unwrap() == file_str),
            "File should not appear in changes as deleted"
        );
    }
}

#[tokio::test]
#[serial(cwd)]
/// Tests attempting to remove a directory without -r flag should fail
async fn test_remove_directory_without_recursive() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a directory with files
    let file1 = create_file("test_dir/file1.txt", "File 1 content");
    let file2 = create_file("test_dir/file2.txt", "File 2 content");

    // Add all files to the index
    add::execute(AddArgs {
        pathspec: vec![String::from("test_dir")],
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

    // Make sure the directory and files exist
    assert!(fs::metadata("test_dir").is_ok(), "Directory should exist");
    assert!(file1.exists(), "File 1 should exist");
    assert!(file2.exists(), "File 2 should exist");

    // Attempt to remove the directory without recursive flag
    let args = RemoveArgs {
        pathspec: vec![String::from("test_dir")],
        cached: false,
        recursive: false,
        force: false,
        dry_run: false,
        ignore_unmatch: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        sparse: false,
    };
    remove::execute(args).await;
    // Removing a directory without recursive should fail - the function should handle this internally

    // Verify the directory and files still exist
    assert!(
        fs::metadata("test_dir").is_ok(),
        "Directory should still exist"
    );
    assert!(file1.exists(), "File 1 should still exist");
    assert!(file2.exists(), "File 2 should still exist");
}

#[tokio::test]
#[serial(cwd)]
/// Tests removing a file that does not exist in the index
async fn test_remove_untracked_file() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a file but don't add it to the index
    let file_path = create_file("untracked_file.txt", "Untracked content");

    // Make sure the file exists
    assert!(file_path.exists(), "File should exist");

    // Attempt to remove the untracked file (should fail/do nothing)
    let mut args = RemoveArgs {
        pathspec: vec![String::from("untracked_file.txt")],
        cached: false,
        recursive: false,
        force: false,
        dry_run: false,
        ignore_unmatch: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        sparse: false,
    };
    remove::execute(args.clone()).await;
    // Removing an untracked file should return an error - the function should handle this internally

    // Verify the file still exists
    assert!(file_path.exists(), "File should still exist");
    args.force = true;
    remove::execute(args).await;
    assert!(file_path.exists(), "File should still exist");
}

#[tokio::test]
#[serial(cwd)]
/// Tests removing a file that has been modified after being added to the index
async fn test_remove_modified_file() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a file and add it to index
    let file_path = create_file("test_file.txt", "Initial content");

    add::execute(AddArgs {
        pathspec: vec![String::from("test_file.txt")],
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

    // Modify the file
    let mut file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&file_path)
        .unwrap();
    file.write_all(b" - Modified").unwrap();

    // Remove the file
    let args = RemoveArgs {
        pathspec: vec![String::from("test_file.txt")],
        cached: false,
        recursive: false,
        force: true,
        dry_run: false,
        ignore_unmatch: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        sparse: false,
    };
    remove::execute(args).await;

    // Verify the file was removed.
    assert!(!file_path.exists(), "File should be removed");

    // Verify file is not in the index.
    let changes = changes_to_be_staged().unwrap();
    assert!(
        !changes
            .new
            .iter()
            .any(|x| x.to_str().unwrap() == "test_file.txt"),
        "File should not appear in changes as new"
    );
    assert!(
        !changes
            .modified
            .iter()
            .any(|x| x.to_str().unwrap() == "test_file.txt"),
        "File should not appear in changes as modified"
    );
    assert!(
        !changes
            .deleted
            .iter()
            .any(|x| x.to_str().unwrap() == "test_file.txt"),
        "File should not appear in changes as deleted"
    );
}

#[tokio::test]
#[serial(cwd)]
/// Tests removing multiple files at once
async fn test_remove_multiple_files() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create multiple files
    let file1 = create_file("file1.txt", "File 1 content");
    let file2 = create_file("file2.txt", "File 2 content");
    let file3 = create_file("file3.txt", "File 3 content");

    // Add all files to the index
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

    // Make sure all files exist
    assert!(file1.exists(), "File 1 should exist");
    assert!(file2.exists(), "File 2 should exist");
    assert!(file3.exists(), "File 3 should exist");

    // Remove multiple files at once
    let mut args = RemoveArgs {
        pathspec: vec![String::from("file1.txt"), String::from("file3.txt")],
        cached: false,
        recursive: false,
        force: false,
        dry_run: false,
        ignore_unmatch: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        sparse: false,
    };
    remove::execute(args.clone()).await;
    // Verify the specified files were removed
    assert!(file1.exists(), "File 1 should still exist");
    assert!(file2.exists(), "File 2 should still exist");
    assert!(file3.exists(), "File 3 should still exist");
    args.force = true;
    remove::execute(args).await;
    // Verify the specified files were removed
    assert!(!file1.exists(), "File 1 should be removed");
    assert!(file2.exists(), "File 2 should still exist");
    assert!(!file3.exists(), "File 3 should be removed");
}

#[tokio::test]
#[serial(cwd)]
/// Tests the --dry-run flag which shows what would be removed without actually removing anything
async fn test_remove_dry_run() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create multiple files
    let file1 = create_file("file1.txt", "File 1 content");
    let file2 = create_file("file2.txt", "File 2 content");
    let file3 = create_file("subdir/file3.txt", "File 3 content");

    // Add all files to the index
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

    // Make sure all files exist before dry-run
    assert!(file1.exists(), "File 1 should exist before dry-run");
    assert!(file2.exists(), "File 2 should exist before dry-run");
    assert!(file3.exists(), "File 3 should exist before dry-run");

    // Run rm with --dry-run flag
    let args = RemoveArgs {
        pathspec: vec![String::from("file1.txt"), String::from("file2.txt")],
        cached: false,
        recursive: false,
        force: false,
        dry_run: true,
        ignore_unmatch: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        sparse: false,
    };
    remove::execute(args).await;

    // Verify that no files were actually removed
    assert!(file1.exists(), "File 1 should still exist after dry-run");
    assert!(file2.exists(), "File 2 should still exist after dry-run");
    assert!(file3.exists(), "File 3 should still exist after dry-run");

    // Verify files are still in the index by checking they don't appear as deleted
    let changes = changes_to_be_staged().unwrap();
    assert!(
        !changes
            .deleted
            .iter()
            .any(|x| x.to_str().unwrap() == "file1.txt"),
        "File 1 should not appear as deleted"
    );
    assert!(
        !changes
            .deleted
            .iter()
            .any(|x| x.to_str().unwrap() == "file2.txt"),
        "File 2 should not appear as deleted"
    );
}

#[tokio::test]
#[serial(cwd)]
/// Tests --dry-run with --cached flag
async fn test_remove_dry_run_cached() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a file and add it to index
    let file_path = create_file("test_file.txt", "Test content");

    add::execute(AddArgs {
        pathspec: vec![String::from("test_file.txt")],
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

    // Run rm with --dry-run and --cached flags
    let args = RemoveArgs {
        pathspec: vec![String::from("test_file.txt")],
        cached: true,
        recursive: false,
        force: false,
        dry_run: true,
        ignore_unmatch: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        sparse: false,
    };
    remove::execute(args).await;

    // Verify the file still exists in both filesystem and index
    assert!(file_path.exists(), "File should still exist in filesystem");

    // Verify file doesn't appear as deleted in changes
    let changes = changes_to_be_staged().unwrap();
    assert!(
        !changes
            .deleted
            .iter()
            .any(|x| x.to_str().unwrap() == "test_file.txt"),
        "File should not appear as deleted"
    );
}

#[tokio::test]
#[serial(cwd)]
/// Tests --dry-run with recursive directory removal
async fn test_remove_dry_run_recursive() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a directory with files
    let file1 = create_file("test_dir/file1.txt", "File 1 content");
    let file2 = create_file("test_dir/file2.txt", "File 2 content");
    let file3 = create_file("test_dir/subdir/file3.txt", "File 3 content");

    // Add all files to the index
    add::execute(AddArgs {
        pathspec: vec![String::from("test_dir")],
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

    // Run rm with --dry-run and --recursive flags
    let args = RemoveArgs {
        pathspec: vec![String::from("test_dir")],
        cached: false,
        recursive: true,
        force: false,
        dry_run: true,
        ignore_unmatch: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        sparse: false,
    };
    remove::execute(args).await;

    // Verify that no files or directories were actually removed
    assert!(file1.exists(), "File 1 should still exist after dry-run");
    assert!(file2.exists(), "File 2 should still exist after dry-run");
    assert!(file3.exists(), "File 3 should still exist after dry-run");
    assert!(
        PathBuf::from("test_dir").exists(),
        "Directory should still exist"
    );
    assert!(
        PathBuf::from("test_dir/subdir").exists(),
        "Subdirectory should still exist"
    );

    // Verify files are still tracked by checking they don't appear as deleted
    let changes = changes_to_be_staged().unwrap();
    assert!(
        !changes
            .deleted
            .iter()
            .any(|x| x.to_str().unwrap().starts_with("test_dir/")),
        "No files in test_dir should appear as deleted"
    );
}
#[tokio::test]
#[serial(cwd)]
/// Tests --ignore-unmatch with recursive directory removal
async fn test_remove_ignore_unmatch() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a directory with files
    let file1 = create_file("test_dir/file1.txt", "File 1 content");
    let file2 = create_file("test_dir/file2.txt", "File 2 content");

    // Add file 1 to the index
    add::execute(AddArgs {
        pathspec: vec![String::from("test_dir/file1.txt")],
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

    // Run rm without ignore_unmatch flag
    let mut args = RemoveArgs {
        pathspec: vec![
            String::from("test_dir/file1.txt"),
            String::from("test_dir/file2.txt"),
        ],
        cached: false,
        recursive: true,
        force: true,
        dry_run: false,
        ignore_unmatch: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        sparse: false,
    };
    remove::execute(args.clone()).await;

    // Verify that no files or directories were removed
    assert!(file1.exists(), "File 1 should still exist");
    assert!(file2.exists(), "File 2 should still exist");

    args.ignore_unmatch = true;
    remove::execute(args).await;

    assert!(!file1.exists(), "File 1 should be remove");
    assert!(file2.exists(), "File 2 should still exist");
}
#[tokio::test]
#[serial(cwd)]
/// Tests rm --pathspec-from-file with newline-separated file
async fn test_remove_pathspec_from_file_newline() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create files
    let file1 = create_file("file1.txt", "File 1 content");
    let file2 = create_file("file2.txt", "File 2 content");

    // Add all files to index
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

    // Create newline-separated pathspec file
    fs::write("paths.txt", "file1.txt\n").unwrap();

    let args = RemoveArgs {
        pathspec: vec![], // pathspec comes from file
        cached: false,
        recursive: false,
        force: true,
        dry_run: false,
        ignore_unmatch: false,
        pathspec_from_file: Some(String::from("paths.txt")),
        pathspec_file_nul: false,
        sparse: false,
    };

    remove::execute(args).await;

    assert!(!file1.exists(), "file1.txt should be removed");
    assert!(file2.exists(), "file2.txt should still exist");
}
#[tokio::test]
#[serial(cwd)]
/// Tests rm --pathspec-from-file with NUL-separated file
async fn test_remove_pathspec_from_file_nul() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create files
    let file1 = create_file("file1.txt", "File 1 content");
    let file2 = create_file("file2.txt", "File 2 content");

    // Add all files to index
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

    // Create NUL-separated pathspec file
    let mut data = Vec::new();
    data.extend_from_slice(b"file1.txt\0");
    fs::write("paths.bin", data).unwrap();

    let args = RemoveArgs {
        pathspec: vec![],
        cached: false,
        recursive: false,
        force: true,
        dry_run: false,
        ignore_unmatch: false,
        pathspec_from_file: Some(String::from("paths.bin")),
        pathspec_file_nul: true,
        sparse: false,
    };

    remove::execute(args).await;

    assert!(!file1.exists(), "file1.txt should be removed");
    assert!(file2.exists(), "file2.txt should still exist");
}
#[tokio::test]
#[serial(cwd)]
/// Tests rm --pathspec-from-file with --ignore-unmatch
async fn test_remove_pathspec_from_file_ignore_unmatch() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a file and add it to index
    let file1 = create_file("file1.txt", "File 1 content");

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

    // Pathspec file contains one valid and one invalid path
    fs::write("paths.txt", "file1.txt\nnot_exist.txt\n").unwrap();

    let mut args = RemoveArgs {
        pathspec: vec![],
        cached: false,
        recursive: false,
        force: true,
        dry_run: false,
        ignore_unmatch: false,
        pathspec_from_file: Some(String::from("paths.txt")),
        pathspec_file_nul: false,
        sparse: false,
    };

    // Without --ignore-unmatch: should not remove
    remove::execute(args.clone()).await;
    assert!(file1.exists(), "file1.txt should still exist");

    // With --ignore-unmatch: should remove valid file
    args.ignore_unmatch = true;
    remove::execute(args).await;
    assert!(!file1.exists(), "file1.txt should be removed");
}

/// `libra rm --help` surfaces the EXAMPLES banner so users see the
/// single-file, recursive, `--cached`, force, dry-run, pathspec-from-file,
/// and JSON forms before they hit one of `rm`'s strict
/// conflicting-state safety errors. Cross-cutting `--help` EXAMPLES
/// rollout per `docs/development/commands/_general.md` item B.
#[test]
fn test_rm_help_lists_examples_banner() {
    let repo = tempdir().expect("tempdir for rm --help");
    let output = run_libra_command(&["rm", "--help"], repo.path());
    assert!(
        output.status.success(),
        "rm --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "rm --help should include EXAMPLES banner, stdout: {stdout}"
    );
    for invocation in [
        "libra rm stale.txt",
        "libra rm -r logs/",
        "libra rm --cached secrets.env",
        "libra rm -f conflicted.txt",
        "libra rm --dry-run",
        "libra rm --pathspec-from-file",
        "libra rm --json",
    ] {
        assert!(
            stdout.contains(invocation),
            "rm --help should include `{invocation}`, stdout: {stdout}"
        );
    }
}

/// `rm --sparse` is accepted on the CLI as a no-op (Libra has no
/// sparse-checkout cone). Driven through the real binary so the clap flag is
/// proven to parse; combined with `--cached`, the file is untracked from the
/// index but kept on disk exactly as a plain `rm --cached` would.
#[test]
fn test_remove_sparse_flag_is_noop() {
    let repo = create_committed_repo_via_cli();
    let tracked = repo.path().join("tracked.txt");
    assert!(tracked.exists(), "tracked.txt exists before removal");

    let output = run_libra_command(&["rm", "--cached", "--sparse", "tracked.txt"], repo.path());
    assert_cli_success(&output, "rm --cached --sparse should be accepted");

    // Working-tree file is kept (--cached), proving --sparse changed nothing.
    assert!(
        tracked.exists(),
        "rm --cached --sparse keeps the working-tree file"
    );

    // The index no longer tracks the file (the --cached removal still applied).
    let ls = run_libra_command(&["ls-files"], repo.path());
    assert_cli_success(&ls, "ls-files should succeed");
    let tracked_list = String::from_utf8_lossy(&ls.stdout);
    assert!(
        !tracked_list.contains("tracked.txt"),
        "rm --cached --sparse untracks the file from the index: {tracked_list}"
    );
}
