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
        force: false,
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
        force: true,
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
    })
    .await;

    clean::execute(CleanArgs {
        dry_run: false,
        force: true,
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
        force: true,
        directories: false,
        ignored: false,
        only_ignored: false,
        exclude: vec![],
        pathspec: vec![],
    })
    .await;

    assert!(!std::path::Path::new("dir/sub/untracked.txt").exists());
    assert!(std::path::Path::new("dir/sub").exists());
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
    })
    .await;

    fs::write("ignored.txt", "ignored").unwrap();
    fs::write("normal.txt", "normal").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: true,
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
    })
    .await;

    fs::write("untracked1.txt", "one").unwrap();
    fs::write("untracked2.txt", "two").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: true,
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
        force: true,
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
        force: true,
        directories: false,
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
        force: true,
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
        force: true,
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
    })
    .await;

    // Add an untracked file in the same directory
    fs::write("mixed_dir/untracked.txt", "untracked").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: true,
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
    })
    .await;

    fs::write("ignored.txt", "ignored").unwrap();
    fs::write("debug.log", "log content").unwrap();
    fs::write("normal.txt", "normal").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: true,
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
    })
    .await;

    fs::write("ignored.txt", "ignored").unwrap();
    fs::write("debug.log", "log content").unwrap();
    fs::write("normal.txt", "normal").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: true,
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
        force: true,
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
        force: true,
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
    })
    .await;

    fs::create_dir_all("ignored_dir").unwrap();
    fs::write("ignored_dir/file.txt", "content").unwrap();

    clean::execute(CleanArgs {
        dry_run: false,
        force: true,
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
        force: true,
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
        force: true,
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
        force: true,
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
        force: true,
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
