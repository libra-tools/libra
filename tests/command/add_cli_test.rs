//! Binary-level `libra add` behavior checks driven through the compiled binary.
//!
//! **Layer:** L1 — deterministic, no external dependencies.
//!
//! These tests spawn the real `libra` binary in an isolated temp directory and
//! assert on stable exit codes, structured error codes (e.g. `LBR-CLI-003`),
//! and the post-condition state observable via `libra status --short`. The
//! fixture convention is: `tempdir() -> repo/ -> libra init`, then write a
//! handful of plain-text files and exercise `libra add`. The intent is to
//! lock in the user-visible contract — exit codes, error code tags, and the
//! atomicity of partial-failure cases — separately from the in-process tests.

use std::{fs, path::Path, process::Command};

use tempfile::tempdir;

/// Spawn `libra` with a fully isolated `HOME`/`XDG_CONFIG_HOME` so no host
/// configuration leaks into the subprocess. Logging env vars are stripped so
/// stderr stays deterministic.
fn run_libra(args: &[&str], cwd: &Path) -> std::process::Output {
    let home = cwd.join(".libra-test-home");
    let config_home = home.join(".config");
    fs::create_dir_all(&config_home).unwrap();

    Command::new(env!("CARGO_BIN_EXE_libra"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env_remove("RUST_LOG")
        .env_remove("LIBRA_LOG")
        .output()
        .unwrap()
}

/// Create the directory and run `libra init` through the binary, asserting
/// success. This mirrors the user's first-touch experience instead of using
/// the in-process `init` helper.
fn init_repo(repo: &Path) {
    fs::create_dir_all(repo).unwrap();
    let output = run_libra(&["init"], repo);
    assert!(output.status.success(), "{:?}", output);
}

/// Scenario: when one of multiple `add` pathspecs cannot be resolved, the
/// command must fail atomically — no path may be staged. Guards exit code
/// 129 (CLI usage), the `LBR-CLI-003` error tag, and the absence of any
/// `A  good.txt` row in `status --short`.
#[test]
fn missing_pathspec_is_fatal_and_atomic() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    fs::write(repo.join("good.txt"), "good").unwrap();

    let output = run_libra(&["add", "good.txt", "missing.txt"], &repo);
    assert_eq!(output.status.code(), Some(129));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("fatal: pathspec 'missing.txt' did not match any files"));
    assert!(stderr.contains("Error-Code: LBR-CLI-003"));

    let status = run_libra(&["status", "--short"], &repo);
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(!stdout.contains("A  good.txt"), "status was: {stdout}");
}

/// Scenario: a mix of staged-eligible and ignored paths must succeed (exit
/// code 1, like Git) for the eligible ones while warning on stderr about the ignored
/// path. Confirms only the non-ignored file appears in `status --short`.
#[test]
fn partial_ignore_stages_good_files_and_warns() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    fs::write(repo.join(".libraignore"), "ignored.txt\n").unwrap();
    fs::write(repo.join("good.txt"), "good").unwrap();
    fs::write(repo.join("ignored.txt"), "ignored").unwrap();

    let output = run_libra(&["add", "good.txt", "ignored.txt"], &repo);
    // ADR-IA-02: mixed ignore stages good.txt and exits 1 (Git parity).
    assert_eq!(
        output.status.code(),
        Some(1),
        "mixed ignored add must exit 1: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ignored.txt"),
        "stderr should warn about ignored file: {stderr}"
    );

    let status = run_libra(&["status", "--short"], &repo);
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(stdout.contains("A  good.txt"), "status was: {stdout}");
}

/// Scenario: `libra add` against a single ignored path is treated as a
/// conflict (exit 128) and must not stage any file — including unrelated
/// ones present in the working tree. Pins both the exit code and the
/// "no side effects" invariant.
#[test]
fn ignored_only_path_returns_conflict_exit_code() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    fs::write(repo.join(".libraignore"), "ignored.txt\n").unwrap();
    fs::write(repo.join("visible.txt"), "visible").unwrap();
    fs::write(repo.join("ignored.txt"), "ignored").unwrap();

    let output = run_libra(&["add", "ignored.txt"], &repo);
    assert_eq!(output.status.code(), Some(128));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ignored.txt"));

    let status = run_libra(&["status", "--short"], &repo);
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        !stdout.contains("A  visible.txt"),
        "ignored-only add should not stage unrelated files: {stdout}"
    );
    assert!(
        !stdout.contains("A  ignored.txt"),
        "ignored-only add should not stage the ignored file: {stdout}"
    );
}

/// Scenario: a corrupted `.libra/index` must surface a fatal error tagged
/// `LBR-REPO-002` with exit code 128 and never panic. Regression guard
/// against unwrap-in-index-parser bugs that would leak `thread 'main'
/// panicked` / `stack backtrace` to end-users.
#[test]
fn corrupt_index_reports_fatal_without_panic() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    fs::write(repo.join("good.txt"), "good").unwrap();
    fs::write(repo.join(".libra").join("index"), b"garb").unwrap();

    let output = run_libra(&["add", "good.txt"], &repo);
    assert_eq!(output.status.code(), Some(128));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("fatal: unable to read index"));
    assert!(stderr.contains("Error-Code: LBR-REPO-002"));
    assert!(!stderr.contains("thread 'main' panicked"));
    assert!(!stderr.contains("stack backtrace"));
}

/// M-DECLINED X1 / X7: `add -i` is a stable unsupported refusal, not a usage hint.
#[test]
fn interactive_flag_is_unsupported_and_atomic() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    fs::write(repo.join("good.txt"), "good").unwrap();

    let output = run_libra(&["add", "-i", "good.txt"], &repo);
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-UNSUPPORTED-001"), "{stderr}");
    assert!(
        stderr.contains("interactive add is not supported"),
        "{stderr}"
    );

    let status = run_libra(&["status", "--short"], &repo);
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        !stdout.contains("A  good.txt"),
        "add -i must not stage: {stdout}"
    );
}

/// M-DECLINED X6: an unknown add flag stays a 129 usage error.
#[test]
fn unknown_flag_stays_cli_invalid() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    fs::write(repo.join("good.txt"), "good").unwrap();

    let output = run_libra(&["add", "--bogus", "good.txt"], &repo);
    assert_eq!(output.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
    assert!(!stderr.contains("LBR-UNSUPPORTED-001"), "{stderr}");
}
