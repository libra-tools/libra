//! Tests clean command removing untracked files with minimal flags.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::{fs, io::Write, process::Command};

use libra::utils::path;

use super::*;

#[tokio::test]
#[serial(cwd)]
/// Tests dry-run mode does not delete files.
async fn test_clean_dry_run_keeps_files() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    let mut file = fs::File::create("untracked.txt").unwrap();
    file.write_all(b"content").unwrap();

    clean::execute(CleanArgs {
        dry_run: true,
        force: 0,
        directories: false,
        ignored: false,
        only_ignored: false,
        exclude: vec![],
        pathspec: vec![],
    })
    .await;

    assert!(std::path::Path::new("untracked.txt").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests force mode deletes untracked files.
async fn test_clean_force_removes_files() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    let mut file = fs::File::create("untracked.txt").unwrap();
    file.write_all(b"content").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: false,
        ignored: false,
        only_ignored: false,
        exclude: vec![],
        pathspec: vec![],
    })
    .await;

    assert!(!std::path::Path::new("untracked.txt").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests requiring -f or -n to proceed.
async fn test_clean_requires_flag() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    let mut file = fs::File::create("untracked.txt").unwrap();
    file.write_all(b"content").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(test_dir.path())
        .arg("clean")
        .output()
        .expect("failed to execute `libra clean`");

    assert_eq!(output.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("fatal: clean requires -f or -n"));
    assert!(stderr.contains("Error-Code: LBR-CLI-002"));
    assert!(stderr.contains("Hint:"));

    assert!(std::path::Path::new("untracked.txt").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests clean does not remove tracked files.
async fn test_clean_force_keeps_tracked_files() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    let mut file = fs::File::create("tracked.txt").unwrap();
    file.write_all(b"content").unwrap();

    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec![String::from("tracked.txt")],
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

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: false,
        ignored: false,
        only_ignored: false,
        exclude: vec![],
        pathspec: vec![],
    })
    .await;

    assert!(std::path::Path::new("tracked.txt").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests clean removes untracked files in subdirectories.
async fn test_clean_force_removes_nested_untracked() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    fs::create_dir_all("dir/sub").unwrap();
    let mut file = fs::File::create("dir/sub/untracked.txt").unwrap();
    file.write_all(b"content").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: true,
        ignored: false,
        only_ignored: false,
        exclude: vec![],
        pathspec: vec![],
    })
    .await;

    assert!(!std::path::Path::new("dir/sub/untracked.txt").exists());
    assert!(!std::path::Path::new("dir/sub").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests clean respects ignore rules for untracked files.
async fn test_clean_force_respects_ignore_rules() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    fs::write(".libraignore", "ignored.txt\n").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec![String::from(".libraignore")],
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

    fs::write("ignored.txt", "ignored").unwrap();
    fs::write("normal.txt", "normal").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: false,
        ignored: false,
        only_ignored: false,
        exclude: vec![],
        pathspec: vec![],
    })
    .await;

    assert!(std::path::Path::new("ignored.txt").exists());
    assert!(!std::path::Path::new("normal.txt").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests clean removes multiple untracked files but keeps tracked files.
async fn test_clean_force_multiple_untracked_with_tracked() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    let mut tracked = fs::File::create("tracked.txt").unwrap();
    tracked.write_all(b"content").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec![String::from("tracked.txt")],
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

    fs::write("untracked1.txt", "one").unwrap();
    fs::write("untracked2.txt", "two").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: false,
        ignored: false,
        only_ignored: false,
        exclude: vec![],
        pathspec: vec![],
    })
    .await;

    assert!(std::path::Path::new("tracked.txt").exists());
    assert!(!std::path::Path::new("untracked1.txt").exists());
    assert!(!std::path::Path::new("untracked2.txt").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests clean handles missing index by treating all files as untracked.
async fn test_clean_force_with_missing_index() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    let index_path = path::index();
    if index_path.exists() {
        fs::remove_file(index_path).unwrap();
    }

    fs::write("untracked.txt", "content").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: false,
        ignored: false,
        only_ignored: false,
        exclude: vec![],
        pathspec: vec![],
    })
    .await;

    assert!(!std::path::Path::new("untracked.txt").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests clean reports a fatal error for a corrupted index and keeps files.
async fn test_clean_force_with_corrupted_index_returns_fatal_128() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    let index_path = path::index();
    fs::write(&index_path, b"corrupted-index-data").unwrap();

    fs::write("untracked.txt", "content").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(test_dir.path())
        .arg("clean")
        .arg("-f")
        .arg("protected/untracked.txt")
        .output()
        .expect("failed to execute `libra clean`");

    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("fatal: failed to load index"));
    assert!(stderr.contains("Error-Code: LBR-IO-001"));
    assert!(std::path::Path::new("untracked.txt").exists());
}

#[tokio::test]
#[serial(env, cwd)]
/// Tests dry-run output format.
async fn test_clean_dry_run_output_format() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;

    let file_path = test_dir.path().join("untracked.txt");
    fs::write(&file_path, "content").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(test_dir.path())
        .arg("clean")
        .arg("-n")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Would remove untracked.txt"));
}

#[tokio::test]
#[serial(env, cwd)]
/// Tests that -f and -n together behave like dry-run (no deletion).
async fn test_clean_force_and_dry_run_prefers_dry_run() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;

    fs::write(test_dir.path().join("untracked.txt"), "content").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(test_dir.path())
        .arg("clean")
        .arg("-f")
        .arg("-n")
        .output()
        .expect("failed to execute `libra clean`");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Would remove untracked.txt"));
    assert!(test_dir.path().join("untracked.txt").exists());
}

#[test]
fn test_clean_force_json_reports_deleted_files() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("generated.txt"), "content").unwrap();

    let output = run_libra_command(&["clean", "-f", "--json"], repo.path());
    assert_cli_success(&output, "clean -f --json should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "clean");
    assert_eq!(json["data"]["dry_run"], false);
    assert_eq!(
        json["data"]["removed"],
        serde_json::json!(["generated.txt"])
    );
    assert!(
        !repo.path().join("generated.txt").exists(),
        "clean -f should remove the reported file"
    );
}

#[tokio::test]
#[serial(cwd)]
/// Tests clean can handle relatively long file paths.
async fn test_clean_force_with_long_path() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    let long_name = "a".repeat(200);
    let long_path = format!("dir/{long_name}.txt");
    fs::create_dir_all("dir").unwrap();
    fs::write(&long_path, "content").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: true,
        ignored: false,
        only_ignored: false,
        exclude: vec![],
        pathspec: vec![],
    })
    .await;

    assert!(!std::path::Path::new(&long_path).exists());
}

#[cfg(unix)]
#[tokio::test]
#[serial(cwd)]
/// Tests clean does not delete files outside the workdir via symlinked directories.
async fn test_clean_force_does_not_follow_symlink_dirs() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    let outside_dir = tempdir().unwrap();
    let outside_file = outside_dir.path().join("outside.txt");
    fs::write(&outside_file, "content").unwrap();

    symlink(outside_dir.path(), "linked").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: false,
        ignored: false,
        only_ignored: false,
        exclude: vec![],
        pathspec: vec![],
    })
    .await;

    assert!(outside_file.exists());
}

#[cfg(unix)]
#[tokio::test]
#[serial(cwd)]
/// Tests clean reports a fatal error when deletion is denied.
async fn test_clean_force_permission_error_returns_io_exit_code() {
    if skip_permission_denied_test_if_root("test_clean_force_permission_error_returns_io_exit_code")
    {
        return;
    }

    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    fs::create_dir_all("protected").unwrap();
    fs::write("protected/untracked.txt", "content").unwrap();

    let mut perms = fs::metadata("protected").unwrap().permissions();
    perms.set_mode(0o555);
    fs::set_permissions("protected", perms).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(test_dir.path())
        .arg("clean")
        .arg("-f")
        .arg("protected/untracked.txt")
        .output()
        .expect("failed to execute `libra clean`");

    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("fatal: failed to remove"));
    assert!(stderr.contains("Error-Code: LBR-IO-002"));
    assert!(std::path::Path::new("protected/untracked.txt").exists());

    let mut perms = fs::metadata("protected").unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions("protected", perms).unwrap();
}

#[tokio::test]
#[serial(cwd)]
async fn test_clean_json_dry_run_lists_candidates() {
    let repo = tempdir().unwrap();
    test::setup_with_new_libra_in(repo.path()).await;

    fs::write(repo.path().join("alpha.txt"), "alpha").unwrap();
    fs::write(repo.path().join("beta.txt"), "beta").unwrap();

    let output = run_libra_command(&["clean", "-n", "--json"], repo.path());
    assert_cli_success(&output, "clean --json dry-run should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "clean");
    assert_eq!(json["data"]["dry_run"], true);

    let removed = json["data"]["removed"]
        .as_array()
        .expect("removed should be an array");
    assert!(removed.iter().any(|path| path == "alpha.txt"));
    assert!(removed.iter().any(|path| path == "beta.txt"));
}

#[tokio::test]
#[serial(cwd)]
/// Tests -d flag removes untracked directories.
async fn test_clean_d_flag_removes_untracked_dirs() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create an untracked directory with files
    fs::create_dir_all("untracked_dir/sub").unwrap();
    fs::write("untracked_dir/file.txt", "content").unwrap();
    fs::write("untracked_dir/sub/nested.txt", "nested").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: true,
        ignored: false,
        only_ignored: false,
        exclude: vec![],
        pathspec: vec![],
    })
    .await;

    assert!(!std::path::Path::new("untracked_dir").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests -d flag does not remove directories with tracked files.
async fn test_clean_d_flag_keeps_dirs_with_tracked_files() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create a directory with a tracked file
    fs::create_dir_all("mixed_dir").unwrap();
    fs::write("mixed_dir/tracked.txt", "tracked").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec![String::from("mixed_dir/tracked.txt")],
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

    // Add an untracked file in the same directory
    fs::write("mixed_dir/untracked.txt", "untracked").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: true,
        ignored: false,
        only_ignored: false,
        exclude: vec![],
        pathspec: vec![],
    })
    .await;

    // Directory should still exist because it has tracked files
    assert!(std::path::Path::new("mixed_dir").exists());
    assert!(std::path::Path::new("mixed_dir/tracked.txt").exists());
    assert!(!std::path::Path::new("mixed_dir/untracked.txt").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests -x flag removes ignored files.
async fn test_clean_x_flag_removes_ignored_files() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create .libraignore and ignored files
    fs::write(".libraignore", "ignored.txt\n*.log\n").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec![String::from(".libraignore")],
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

    fs::write("ignored.txt", "ignored").unwrap();
    fs::write("debug.log", "log content").unwrap();
    fs::write("normal.txt", "normal").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: false,
        ignored: true,
        only_ignored: false,
        exclude: vec![],
        pathspec: vec![],
    })
    .await;

    assert!(!std::path::Path::new("ignored.txt").exists());
    assert!(!std::path::Path::new("debug.log").exists());
    assert!(!std::path::Path::new("normal.txt").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests -X flag removes only ignored files.
async fn test_clean_x_flag_removes_only_ignored_files() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create .libraignore and ignored files
    fs::write(".libraignore", "ignored.txt\n*.log\n").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec![String::from(".libraignore")],
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

    fs::write("ignored.txt", "ignored").unwrap();
    fs::write("debug.log", "log content").unwrap();
    fs::write("normal.txt", "normal").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: false,
        ignored: false,
        only_ignored: true,
        exclude: vec![],
        pathspec: vec![],
    })
    .await;

    assert!(!std::path::Path::new("ignored.txt").exists());
    assert!(!std::path::Path::new("debug.log").exists());
    assert!(std::path::Path::new("normal.txt").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests --exclude flag excludes matching patterns.
async fn test_clean_exclude_flag_excludes_patterns() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    fs::write("important.txt", "important").unwrap();
    fs::write("temp.log", "log").unwrap();
    fs::write("data.csv", "data").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: false,
        ignored: false,
        only_ignored: false,
        exclude: vec!["*.txt".to_string()],
        pathspec: vec![],
    })
    .await;

    assert!(std::path::Path::new("important.txt").exists());
    assert!(!std::path::Path::new("temp.log").exists());
    assert!(!std::path::Path::new("data.csv").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests --exclude with multiple patterns.
async fn test_clean_exclude_multiple_patterns() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    fs::write("file.txt", "txt").unwrap();
    fs::write("file.log", "log").unwrap();
    fs::write("file.csv", "csv").unwrap();
    fs::write("file.dat", "dat").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: false,
        ignored: false,
        only_ignored: false,
        exclude: vec!["*.txt".to_string(), "*.log".to_string()],
        pathspec: vec![],
    })
    .await;

    assert!(std::path::Path::new("file.txt").exists());
    assert!(std::path::Path::new("file.log").exists());
    assert!(!std::path::Path::new("file.csv").exists());
    assert!(!std::path::Path::new("file.dat").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests -x and -X together returns an error.
async fn test_clean_x_and_x_together_returns_error() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    fs::write("file.txt", "content").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(test_dir.path())
        .args(["clean", "-f", "-x", "-X"])
        .output()
        .expect("failed to execute `libra clean`");

    assert_eq!(output.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot use -x and -X together"));
}

#[tokio::test]
#[serial(cwd)]
/// Tests -d with dry-run shows directories that would be removed.
async fn test_clean_d_dry_run_shows_directories() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    fs::create_dir_all("untracked_dir").unwrap();
    fs::write("untracked_dir/file.txt", "content").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(test_dir.path())
        .args(["clean", "-n", "-d"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Would remove untracked_dir"));
}

#[tokio::test]
#[serial(cwd)]
/// Tests -d with -x removes ignored directories.
async fn test_clean_dx_removes_ignored_directories() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    // Create .libraignore with directory pattern
    fs::write(".libraignore", "ignored_dir/\n").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec![String::from(".libraignore")],
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

    fs::create_dir_all("ignored_dir").unwrap();
    fs::write("ignored_dir/file.txt", "content").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: true,
        ignored: true,
        only_ignored: false,
        exclude: vec![],
        pathspec: vec![],
    })
    .await;

    assert!(!std::path::Path::new("ignored_dir").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests pathspec limits cleaning to the matching file.
async fn test_clean_pathspec_matches_single_file() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    fs::write("keep.txt", "keep").unwrap();
    fs::write("remove.txt", "remove").unwrap();
    fs::write("other.log", "log").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: false,
        ignored: false,
        only_ignored: false,
        exclude: vec![],
        pathspec: vec!["remove.txt".to_string()],
    })
    .await;

    assert!(!std::path::Path::new("remove.txt").exists());
    assert!(std::path::Path::new("keep.txt").exists());
    assert!(std::path::Path::new("other.log").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests pathspec limits cleaning to files under a matching directory prefix.
async fn test_clean_pathspec_matches_directory_prefix() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    fs::create_dir_all("build/out").unwrap();
    fs::write("build/out/artifact.o", "obj").unwrap();
    fs::write("build/debug.log", "log").unwrap();
    fs::write("keep.txt", "keep").unwrap();
    fs::write("other.log", "log").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: false,
        ignored: false,
        only_ignored: false,
        exclude: vec![],
        pathspec: vec!["build".to_string()],
    })
    .await;

    assert!(!std::path::Path::new("build/out/artifact.o").exists());
    assert!(!std::path::Path::new("build/debug.log").exists());
    assert!(std::path::Path::new("keep.txt").exists());
    assert!(std::path::Path::new("other.log").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests pathspec matching nothing cleans nothing.
async fn test_clean_pathspec_matches_nothing_cleans_nothing() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    fs::write("untracked.txt", "content").unwrap();
    fs::write("other.log", "log").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: false,
        ignored: false,
        only_ignored: false,
        exclude: vec![],
        pathspec: vec!["nonexistent".to_string()],
    })
    .await;

    assert!(std::path::Path::new("untracked.txt").exists());
    assert!(std::path::Path::new("other.log").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests pathspec with -d removes a matching untracked directory.
async fn test_clean_pathspec_with_d_removes_matching_dir() {
    let test_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(test_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(test_dir.path());

    fs::create_dir_all("remove_dir/sub").unwrap();
    fs::write("remove_dir/file.txt", "content").unwrap();
    fs::write("remove_dir/sub/nested.txt", "nested").unwrap();
    fs::create_dir_all("keep_dir").unwrap();
    fs::write("keep_dir/file.txt", "content").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: 1,
        directories: true,
        ignored: false,
        only_ignored: false,
        exclude: vec![],
        pathspec: vec!["remove_dir".to_string()],
    })
    .await;

    assert!(!std::path::Path::new("remove_dir").exists());
    assert!(std::path::Path::new("keep_dir").exists());
}

#[tokio::test]
#[serial(cwd)]
/// Tests pathspec filtering shows only matching paths in dry-run JSON output.
async fn test_clean_pathspec_dry_run_json_filters_candidates() {
    let repo = tempdir().unwrap();
    test::setup_with_new_libra_in(repo.path()).await;

    fs::write(repo.path().join("alpha.txt"), "alpha").unwrap();
    fs::write(repo.path().join("beta.txt"), "beta").unwrap();
    fs::write(repo.path().join("gamma.log"), "gamma").unwrap();

    let output = run_libra_command(&["clean", "-n", "--json", "alpha.txt"], repo.path());
    assert_cli_success(&output, "clean -n --json alpha.txt should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "clean");
    assert_eq!(json["data"]["dry_run"], true);

    let removed = json["data"]["removed"]
        .as_array()
        .expect("removed should be an array");
    assert!(
        removed.iter().any(|path| path == "alpha.txt"),
        "alpha.txt should be in dry-run output"
    );
    assert!(
        !removed.iter().any(|path| path == "beta.txt"),
        "beta.txt should not be in dry-run output"
    );
    assert!(
        !removed.iter().any(|path| path == "gamma.log"),
        "gamma.log should not be in dry-run output"
    );
}

#[test]
fn test_clean_short_exclude_alias() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("keep.log"), "x").unwrap();
    fs::write(p.join("remove.txt"), "y").unwrap();

    // `-e` is the short alias for `--exclude`: a dry-run lists remove.txt
    // (would be removed) but not keep.log (excluded by the pattern).
    let out = run_libra_command(&["clean", "-n", "-e", "*.log", "--json"], p);
    assert_cli_success(&out, "clean -n -e *.log --json should succeed");
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["dry_run"], true);
    let removed = json["data"]["removed"].as_array().expect("removed array");
    let names: Vec<&str> = removed.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(
        names.contains(&"remove.txt"),
        "remove.txt would be removed: {names:?}"
    );
    assert!(
        !names.contains(&"keep.log"),
        "keep.log excluded by -e: {names:?}"
    );
}

/// WT-01 (M-GUARD G7, issues/476): issue-verified `clean -n` / `-nd` / `-fdx`.
#[test]
fn test_clean_issue476_verified_surface_guard() {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    fs::write(root.join("junk.txt"), "junk\n").expect("junk");
    fs::create_dir_all(root.join("junkdir")).expect("junkdir");
    fs::write(root.join("junkdir/inside.txt"), "in\n").expect("inside");

    let dry = run_libra_command(&["clean", "-n"], root);
    assert_cli_success(&dry, "G7 -n");
    let dry_out = String::from_utf8_lossy(&dry.stdout);
    assert!(
        dry_out.contains("Would remove junk.txt"),
        "G7 -n: {dry_out}"
    );
    assert!(root.join("junk.txt").is_file(), "G7 -n keeps the file");

    let dry_dir = run_libra_command(&["clean", "-nd"], root);
    assert_cli_success(&dry_dir, "G7 -nd");
    let dry_dir_out = String::from_utf8_lossy(&dry_dir.stdout);
    assert!(
        dry_dir_out.contains("Would remove junkdir"),
        "G7 -nd: {dry_dir_out}"
    );
    assert!(
        root.join("junkdir/inside.txt").is_file(),
        "G7 -nd keeps the directory"
    );

    fs::write(root.join(".libraignore"), "ignored.log\n").expect("ignore");
    fs::write(root.join("ignored.log"), "ignored\n").expect("ignored");
    let force = run_libra_command(&["clean", "-fdx"], root);
    assert_cli_success(&force, "G7 -fdx");
    assert!(!root.join("junk.txt").exists(), "G7 -fdx removes junk.txt");
    assert!(
        !root.join("junkdir").exists(),
        "G7 -fdx removes the untracked directory"
    );
    assert!(
        !root.join("ignored.log").exists(),
        "G7 -fdx removes the ignored file"
    );
    assert!(
        root.join("tracked.txt").is_file(),
        "G7 -fdx must keep tracked files"
    );
}

/// Issue #483: `clean -n` writes no objects.
#[test]
#[serial_test::serial(cwd)]
fn test_clean_dry_run_writes_no_objects() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(p.join("tracked"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "tracked"], p), "add tracked");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    fs::write(p.join("untracked.txt"), "new\n").unwrap();

    let before = count_objects_snapshot(p);
    let out = run_libra_command(&["clean", "-n"], p);
    assert_cli_success(&out, "clean -n");
    assert_no_objects_written(p, &before, "clean -n");
    assert!(p.join("untracked.txt").exists(), "dry run keeps the file");
}

/// PM WC-04: a single force keeps nested repositories (`REMOVE_DIR_KEEP_NESTED_GIT`).
#[test]
fn clean_single_force_keeps_nested_repos() {
    use std::{collections::BTreeMap, path::Path};

    use sha2::{Digest, Sha256};

    fn sha(path: &Path) -> String {
        let bytes = std::fs::read(path).expect("read for snapshot");
        let digest = Sha256::digest(&bytes);
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }
    fn snapshot(root: &Path, dirs: &[&str]) -> BTreeMap<String, String> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name();
                // Repository metadata and the isolated test HOME change on every
                // CLI run; the snapshot is about user-visible worktree files.
                if name == ".libra" || name == ".git" || name == ".libra-test-home" {
                    continue;
                }
                if path.is_dir() {
                    walk(root, &path, out);
                } else if let Ok(relative) = path.strip_prefix(root) {
                    out.insert(relative.to_string_lossy().replace('\\', "/"), sha(&path));
                }
            }
        }
        let mut out = BTreeMap::new();
        for dir in dirs {
            walk(root, &root.join(dir), &mut out);
        }
        out
    }
    let build = || {
        let repo = tempdir().expect("failed to create repository root");
        init_repo_via_cli(repo.path());
        let script = |args: &[&str]| {
            let out = run_libra_command(args, repo.path());
            assert_cli_success(&out, &format!("{args:?}"));
        };
        script(&["config", "user.name", "t"]);
        script(&["config", "user.email", "t@t"]);
        std::fs::write(repo.path().join("tracked"), "x\n").unwrap();
        script(&["add", "tracked"]);
        script(&["commit", "-m", "base", "--no-verify"]);
        std::fs::write(repo.path().join(".gitignore"), "*.world\n").unwrap();
        for (dir, file, body) in [
            ("foo/.git/objects", "", ""),
            ("foo/.git/refs", "", ""),
            ("baz/boo/.git/objects", "", ""),
            ("baz/boo/.git/refs", "", ""),
            ("bar", "goodbye.people", ""),
            ("foo", "hello.world", "hello\n"),
            ("foo/sub", "extra.world", "extra\n"),
            ("baz/boo", "deeper.world", "deeper\n"),
        ] {
            std::fs::create_dir_all(repo.path().join(dir)).unwrap();
            if !file.is_empty() {
                std::fs::write(repo.path().join(dir).join(file), body).unwrap();
            }
        }
        for nested in ["foo", "baz/boo"] {
            std::fs::write(
                repo.path().join(nested).join(".git/HEAD"),
                "ref: refs/heads/main\n",
            )
            .unwrap();
            std::fs::write(repo.path().join(nested).join(".git/index"), "").unwrap();
        }
        let lib = run_libra_command(&["init", "lib"], repo.path());
        assert_cli_success(&lib, "nested libra init");
        std::fs::write(repo.path().join("lib/notes.txt"), "notes\n").unwrap();
        std::fs::write(repo.path().join("lib/extra.world"), "extra\n").unwrap();
        repo
    };
    const DIRS: [&str; 3] = ["foo", "baz/boo", "lib"];

    // AC1/AC2: `clean -f -d` keeps every nested repository file.
    let repo = build();
    let before = snapshot(repo.path(), &DIRS);
    let out = run_libra_command(&["clean", "-f", "-d"], repo.path());
    assert_cli_success(&out, "AC1");
    assert_eq!(snapshot(repo.path(), &DIRS), before, "AC2");

    // AC3/AC4: `clean -f` (no -d) keeps them too.
    let repo = build();
    let before = snapshot(repo.path(), &DIRS);
    let out = run_libra_command(&["clean", "-f"], repo.path());
    assert_cli_success(&out, "AC3");
    assert_eq!(snapshot(repo.path(), &DIRS), before, "AC4");

    // AC5: the preview never lists a nested repository (or its ancestors).
    let repo = build();
    let full_before = snapshot(repo.path(), &["."]);
    let out = run_libra_command(&["clean", "-n", "-d"], repo.path());
    assert_cli_success(&out, "AC5");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    for prefix in ["Would remove foo", "Would remove baz", "Would remove lib"] {
        assert!(
            !text.lines().any(|line| line.starts_with(prefix)),
            "AC5: {text}"
        );
    }
    assert_eq!(snapshot(repo.path(), &["."]), full_before, "AC5 snapshot");

    // AC6: `-x` (delete ignored too) still protects nested repositories.
    let repo = build();
    let before = snapshot(repo.path(), &DIRS);
    let out = run_libra_command(&["clean", "-f", "-d", "-x"], repo.path());
    assert_cli_success(&out, "AC6");
    assert_eq!(snapshot(repo.path(), &DIRS), before, "AC6");

    // AC7: `-X` (only ignored) still protects nested repositories.
    let repo = build();
    let before = snapshot(repo.path(), &DIRS);
    let out = run_libra_command(&["clean", "-f", "-d", "-X"], repo.path());
    assert_cli_success(&out, "AC7");
    assert_eq!(snapshot(repo.path(), &DIRS), before, "AC7");
}

/// WC-04b: explicit pathspecs also respect the nested-repository protection.
#[test]
fn clean_single_force_pathspec_keeps_nested_repos() {
    use std::{collections::BTreeMap, path::Path};

    use sha2::{Digest, Sha256};

    fn snapshot(root: &Path, dir: &str) -> BTreeMap<String, String> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(root, &path, out);
                } else if let Ok(relative) = path.strip_prefix(root) {
                    let digest = Sha256::digest(std::fs::read(&path).expect("read"));
                    out.insert(
                        relative.to_string_lossy().replace('\\', "/"),
                        digest.iter().map(|byte| format!("{byte:02x}")).collect(),
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, &root.join(dir), &mut out);
        out
    }
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    let script = |args: &[&str]| {
        let out = run_libra_command(args, repo.path());
        assert_cli_success(&out, &format!("{args:?}"));
    };
    script(&["config", "user.name", "t"]);
    script(&["config", "user.email", "t@t"]);
    std::fs::write(repo.path().join("tracked"), "x\n").unwrap();
    script(&["add", "tracked"]);
    script(&["commit", "-m", "base", "--no-verify"]);
    for dir in [
        "foo/.git/objects",
        "foo/.git/refs",
        "baz/boo/.git/objects",
        "baz/boo/.git/refs",
    ] {
        std::fs::create_dir_all(repo.path().join(dir)).unwrap();
    }
    for nested in ["foo", "baz/boo"] {
        std::fs::write(
            repo.path().join(nested).join(".git/HEAD"),
            "ref: refs/heads/main\n",
        )
        .unwrap();
        std::fs::write(repo.path().join(nested).join(".git/index"), "").unwrap();
    }
    std::fs::write(repo.path().join("foo/hello.world"), "hello\n").unwrap();
    std::fs::write(repo.path().join("baz/boo/deeper.world"), "deeper\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "lib"], repo.path()),
        "nested libra init",
    );
    std::fs::write(repo.path().join("lib/notes.txt"), "notes\n").unwrap();
    std::fs::write(repo.path().join(".gitignore"), "*.world\n").unwrap();

    for (pathspec, dir) in [
        ("foo/", "foo"),
        ("lib/", "lib"),
        ("foo/hello.world", "foo"),
        ("lib/notes.txt", "lib"),
        ("baz/", "baz/boo"),
    ] {
        let before = snapshot(repo.path(), dir);
        let out = run_libra_command(&["clean", "-f", pathspec], repo.path());
        assert_cli_success(&out, pathspec);
        assert_eq!(
            snapshot(repo.path(), dir),
            before,
            "pathspec {pathspec} must keep nested repository {dir}"
        );
    }
}

#[test]
#[serial(cwd)]
fn clean_relative_pathspec_prefix() {
    use std::fs;

    use tempfile::tempdir;

    let repo = tempdir().unwrap();
    let root = repo.path();
    assert_cli_success(&run_libra_command(&["init"], root), "init");
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/part3.c"), "x").unwrap();
    fs::write(root.join("src/local.c"), "x").unwrap();
    fs::write(root.join("docs/here.c"), "x").unwrap();
    fs::write(root.join("src/tracked.c"), "x").unwrap();
    assert_cli_success(
        &run_libra_command(&["config", "--local", "user.name", "T"], root),
        "name",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--local", "user.email", "t@t.io"], root),
        "email",
    );
    assert_cli_success(&run_libra_command(&["add", "src/tracked.c"], root), "track");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "track"], root),
        "commit",
    );

    let preview = run_libra_command(&["clean", "-n", "../src"], &root.join("docs"));
    assert_cli_success(&preview, "clean -n ../src");
    let text = String::from_utf8_lossy(&preview.stdout);
    assert!(text.contains("Would remove ../src/part3.c"), "{text}");
    assert!(text.contains("Would remove ../src/local.c"), "{text}");
    assert!(!text.contains("here.c"), "{text}");

    let local = run_libra_command(&["clean", "-n"], &root.join("src"));
    assert_cli_success(&local, "clean -n cwd");
    let text = String::from_utf8_lossy(&local.stdout);
    assert!(text.contains("Would remove part3.c"), "{text}");
    assert!(!text.contains("src/"), "{text}");

    let force = run_libra_command(&["clean", "-f", "../src/part3.c"], &root.join("docs"));
    assert_cli_success(&force, "clean -f");
    assert!(String::from_utf8_lossy(&force.stdout).contains("Removing ../src/part3.c"));
    let json = run_libra_command(
        &["--json", "clean", "-n", "../src/local.c"],
        &root.join("docs"),
    );
    assert_cli_success(&json, "json clean");
    let parsed: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json");
    assert_eq!(parsed["data"]["removed"][0], "src/local.c");

    fs::remove_file(root.join("src/local.c")).unwrap();
    fs::write(root.join("src/part3.c"), "x").unwrap();
    let directory = run_libra_command(&["clean", "-f", "../src"], &root.join("docs"));
    assert_cli_success(&directory, "clean -f ../src");
    let text = String::from_utf8_lossy(&directory.stdout);
    assert!(text.contains("Removing ../src/part3.c"), "{text}");
    assert!(!root.join("src/part3.c").exists());
}

#[test]
#[serial(cwd, env)]
fn clean_respects_core_excludesfile() {
    use std::fs;

    use tempfile::tempdir;

    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(&run_libra_command(&["init"], &repo), "init");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let xdg = home.join("xdg");
    let global_dir = temp.path().join("global-dir");
    fs::create_dir_all(&global_dir).unwrap();
    let global_db = global_dir.join("config.db");
    let exclude = temp.path().join("global-ignore");
    fs::write(&exclude, "*.tmp\n").unwrap();
    let envs = [
        ("HOME", home.to_str().unwrap()),
        ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
        ("LIBRA_CONFIG_GLOBAL_DB", global_db.to_str().unwrap()),
    ];
    assert_cli_success(
        &run_libra_command_with_env(
            &[
                "config",
                "--global",
                "core.excludesfile",
                exclude.to_str().unwrap(),
            ],
            &repo,
            &envs,
        ),
        "set excludesfile",
    );
    fs::write(repo.join("keep.c"), "x").unwrap();
    fs::write(repo.join("skip.tmp"), "x").unwrap();
    let out = run_libra_command_with_env(&["clean", "-n"], &repo, &envs);
    assert_cli_success(&out, "clean");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("keep.c"), "{text}");
    assert!(!text.contains("skip.tmp"), "{text}");
}

#[test]
#[serial(cwd)]
fn clean_does_not_descend_untracked_dirs_without_d() {
    use std::fs;

    use tempfile::tempdir;

    let repo = tempdir().unwrap();
    let root = repo.path();
    assert_cli_success(&run_libra_command(&["init"], root), "init");
    fs::write(root.join("tracked.c"), "x").unwrap();
    assert_cli_success(&run_libra_command(&["add", "tracked.c"], root), "add");
    fs::write(root.join("loose.c"), "x").unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();
    fs::write(root.join("docs/manual.txt"), "x").unwrap();
    let preview = run_libra_command(&["clean", "-n"], root);
    assert_cli_success(&preview, "clean -n");
    let text = String::from_utf8_lossy(&preview.stdout);
    assert!(text.contains("loose.c"), "{text}");
    assert!(!text.contains("manual.txt"), "{text}");

    let force = run_libra_command(&["clean", "-f"], root);
    assert_cli_success(&force, "clean -f");
    assert!(!root.join("loose.c").exists());
    assert!(
        root.join("docs/manual.txt").exists(),
        "untracked dir survives without -d"
    );

    let explicit = run_libra_command(&["clean", "-f", "docs/manual.txt"], root);
    assert_cli_success(&explicit, "explicit file pathspec");
    assert!(!root.join("docs/manual.txt").exists());
}

#[test]
#[serial(cwd)]
fn clean_double_force_nested_repo() {
    use std::fs;

    use tempfile::tempdir;

    let repo = tempdir().unwrap();
    let root = repo.path();
    assert_cli_success(&run_libra_command(&["init"], root), "init");
    fs::create_dir_all(root.join("nested/.git")).unwrap();
    fs::write(root.join("nested/inside.txt"), "x").unwrap();
    let single = run_libra_command(&["clean", "-n", "-d"], root);
    let single_text = String::from_utf8_lossy(&single.stdout);
    assert!(!single_text.contains("nested"), "{single_text}");
    let double = run_libra_command(&["clean", "-n", "-f", "-f", "-d"], root);
    assert_cli_success(&double, "double force preview");
    let double_text = String::from_utf8_lossy(&double.stdout);
    assert!(double_text.contains("Would remove nested"), "{double_text}");
    let compact = run_libra_command(&["clean", "-n", "-ff", "-d"], root);
    assert_cli_success(&compact, "-ff");
    assert_eq!(compact.stdout, double.stdout);
}
