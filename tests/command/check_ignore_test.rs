//! Integration tests for `libra check-ignore`.
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network).

use std::fs;

use tempfile::tempdir;

use super::{
    assert_cli_success, init_repo_via_cli, parse_json_stdout, run_libra_command,
    run_libra_command_with_env, run_libra_command_with_stdin,
};

/// Initialize a repo and write a `.libraignore` with a known rule set:
/// `*.log` ignored, `keep.log` whitelisted, `build/` ignored.
fn setup_repo() -> tempfile::TempDir {
    let repo = tempdir().expect("tempdir");
    let init = run_libra_command(&["init"], repo.path());
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    fs::write(
        repo.path().join(".libraignore"),
        "*.log\n!keep.log\nbuild/\n",
    )
    .expect("write .libraignore");
    repo
}

fn stdout_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn ignored_path_prints_and_exits_zero() {
    let repo = setup_repo();
    let out = run_libra_command(&["check-ignore", "a.log"], repo.path());
    assert_eq!(out.status.code(), Some(0), "an ignored path exits 0");
    assert_eq!(stdout_of(&out), "a.log\n");
}

#[test]
fn non_ignored_path_exits_one_with_no_output() {
    let repo = setup_repo();
    let out = run_libra_command(&["check-ignore", "a.txt"], repo.path());
    assert_eq!(out.status.code(), Some(1), "no ignored path exits 1");
    assert!(
        stdout_of(&out).is_empty(),
        "exit-1 prints nothing on stdout"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).is_empty(),
        "exit-1 is a silent signal, not an error"
    );
}

#[test]
fn whitelisted_path_is_not_ignored() {
    let repo = setup_repo();
    // `!keep.log` after `*.log` whitelists keep.log -> not ignored.
    let out = run_libra_command(&["check-ignore", "keep.log"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(1),
        "whitelisted path is not ignored"
    );
}

#[test]
fn verbose_shows_source_line_and_pattern() {
    let repo = setup_repo();
    let out = run_libra_command(&["check-ignore", "-v", "a.log"], repo.path());
    assert_eq!(out.status.code(), Some(0));
    let stdout = stdout_of(&out);
    // `<source>:<line>:<pattern>\t<path>` — *.log is on line 1 of .libraignore.
    assert_eq!(
        stdout, ".libraignore:1:*.log\ta.log\n",
        "verbose attribution"
    );
}

#[test]
fn non_matching_requires_verbose_and_lists_unmatched() {
    let repo = setup_repo();
    // -n without -v is a usage error.
    let err = run_libra_command(&["check-ignore", "-n", "a.log"], repo.path());
    assert!(
        !matches!(err.status.code(), Some(0) | Some(1)),
        "-n without -v is a usage error (exit code {:?})",
        err.status.code()
    );
    assert!(
        String::from_utf8_lossy(&err.stderr).contains("verbose"),
        "usage error mentions --verbose: {}",
        String::from_utf8_lossy(&err.stderr)
    );

    // -v -n lists matched (with pattern) and non-matched (empty fields).
    let out = run_libra_command(&["check-ignore", "-v", "-n", "a.log", "a.txt"], repo.path());
    assert_eq!(out.status.code(), Some(0), "a.log is ignored so exit 0");
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains(".libraignore:1:*.log\ta.log"),
        "matched line: {stdout}"
    );
    assert!(
        stdout.contains("::\ta.txt"),
        "non-matching line has empty fields: {stdout}"
    );
}

#[test]
fn stdin_reads_pathnames() {
    let repo = setup_repo();
    let out =
        run_libra_command_with_stdin(&["check-ignore", "--stdin"], repo.path(), "a.log\na.txt\n");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stdout_of(&out),
        "a.log\n",
        "only the ignored path is printed"
    );
}

#[test]
fn z_uses_nul_delimiters() {
    let repo = setup_repo();
    // Both *.log files are ignored; -z NUL-terminates each output path.
    let out = run_libra_command(&["check-ignore", "-z", "a.log", "b.log"], repo.path());
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout_of(&out), "a.log\0b.log\0");
}

#[test]
fn no_index_reports_match_for_tracked_path() {
    // Build the repo WITHOUT an ignore rule first so the file can be tracked,
    // then add the rule so the tracked path also matches a pattern.
    let repo = tempdir().expect("tempdir");
    let init = run_libra_command(&["init"], repo.path());
    assert!(
        init.status.success(),
        "init: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    fs::write(repo.path().join("tracked.log"), "x").expect("write tracked.log");
    let add = run_libra_command(&["add", "tracked.log"], repo.path());
    assert!(
        add.status.success(),
        "add of a not-yet-ignored file should succeed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    // Now introduce a rule that matches the already-tracked file.
    fs::write(repo.path().join(".libraignore"), "*.log\n").expect("write .libraignore");

    // Default: a tracked path is reported as NOT ignored.
    let default = run_libra_command(&["check-ignore", "tracked.log"], repo.path());
    assert_eq!(
        default.status.code(),
        Some(1),
        "tracked path is not ignored without --no-index"
    );

    // --no-index: report the raw pattern match even though it is tracked.
    let no_index = run_libra_command(&["check-ignore", "--no-index", "tracked.log"], repo.path());
    assert_eq!(
        no_index.status.code(),
        Some(0),
        "--no-index reports the pattern match for a tracked path"
    );
    assert_eq!(stdout_of(&no_index), "tracked.log\n");
}

#[test]
fn json_output_reports_each_path_verdict() {
    let repo = setup_repo();
    let out = run_libra_command(&["--json", "check-ignore", "a.log", "a.txt"], repo.path());
    // a.log is ignored, so the overall exit is 0.
    assert_eq!(out.status.code(), Some(0));
    let json = parse_json_stdout(&out);
    let results = json["data"]["results"].as_array().expect("results array");
    assert_eq!(results.len(), 2);

    let a_log = results
        .iter()
        .find(|r| r["path"] == "a.log")
        .expect("a.log entry");
    assert_eq!(a_log["ignored"], true);
    assert_eq!(a_log["pattern"], "*.log");

    let a_txt = results
        .iter()
        .find(|r| r["path"] == "a.txt")
        .expect("a.txt entry");
    assert_eq!(a_txt["ignored"], false);
}

#[test]
fn outside_repository_is_an_error() {
    // A plain tempdir with no `libra init`.
    let dir = tempdir().expect("tempdir");
    let out = run_libra_command(&["check-ignore", "a.log"], dir.path());
    assert!(
        !matches!(out.status.code(), Some(0) | Some(1)),
        "outside a repository is a fatal error, not a 0/1 ignore signal (exit {:?})",
        out.status.code()
    );
}

#[test]
fn path_escaping_the_worktree_is_a_fatal_error() {
    let repo = setup_repo();
    // `..` escapes the worktree — a fatal error (exit 128), NOT a silent
    // non-match, and the matcher must never consult a `.libraignore` outside the
    // worktree.
    let out = run_libra_command(&["check-ignore", "../outside.log"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(128),
        "an out-of-worktree path is fatal: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout_of(&out).is_empty(),
        "no ignore verdict is printed for an out-of-worktree path"
    );
}

#[test]
fn verbose_reports_the_last_matching_pattern_line() {
    let repo = tempdir().expect("tempdir");
    let init = run_libra_command(&["init"], repo.path());
    assert!(
        init.status.success(),
        "init: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    // Two identical `*.log` patterns; the LAST one (line 3) is the deciding rule.
    fs::write(repo.path().join(".libraignore"), "*.log\nfoo.txt\n*.log\n")
        .expect("write .libraignore");

    let out = run_libra_command(&["check-ignore", "-v", "a.log"], repo.path());
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stdout_of(&out),
        ".libraignore:3:*.log\ta.log\n",
        "verbose reports the deciding (last) matching line"
    );
}

/// PM-18: global ignore sources follow Git — XDG default, case-insensitive
/// config keys, and explicit configuration beating XDG.
#[test]
fn check_ignore_global_sources_follow_git() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    let p = repo.path();
    // Global XDG sources live outside the worktree, so their reported path
    // stays absolute (Git's behaviour).
    let xdg_home = tempdir().expect("xdg home");
    let xdg_git = xdg_home.path().join("git");
    std::fs::create_dir_all(&xdg_git).expect("mkdir xdg git");
    std::fs::write(xdg_git.join("ignore"), "xdgignored\n").expect("write xdg ignore");
    let xdg_env = xdg_home.path().to_str().expect("utf8 xdg").to_string();

    // AC6: the XDG ignore file is reported with its absolute path.
    let out = run_libra_command_with_env(
        &["check-ignore", "-v", "xdgignored"],
        p,
        &[("XDG_CONFIG_HOME", &xdg_env)],
    );
    assert_cli_success(&out, "AC6");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!(
            "{}:1:xdgignored\txdgignored\n",
            xdg_git.join("ignore").display()
        ),
        "AC6"
    );

    // AC7: a lowercase `core.excludesfile` spelling is honored.
    std::fs::write(p.join("global-excludes"), "globalone\n").expect("write global excludes");
    assert_cli_success(
        &run_libra_command(&["config", "core.excludesfile", "global-excludes"], p),
        "AC7 config",
    );
    let out = run_libra_command_with_env(
        &["check-ignore", "-v", "globalone"],
        p,
        &[("XDG_CONFIG_HOME", &xdg_env)],
    );
    assert_cli_success(&out, "AC7");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "global-excludes:1:globalone\tglobalone\n",
        "AC7"
    );

    // AC8: with an explicit excludes file, the XDG default is not consulted.
    let out = run_libra_command_with_env(
        &["check-ignore", "xdgignored"],
        p,
        &[("XDG_CONFIG_HOME", &xdg_env)],
    );
    assert_eq!(out.status.code(), Some(1), "AC8 exit");
    assert!(out.stdout.is_empty(), "AC8 stdout");
}
