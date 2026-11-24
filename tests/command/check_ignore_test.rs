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
    // PM-20: `-z` requires `--stdin`, so feed the two paths on stdin.
    let out = run_libra_command_with_stdin(
        &["check-ignore", "-z", "--stdin"],
        repo.path(),
        "a.log\0b.log\0",
    );
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

/// PM-17: relative paths resolve from the caller's current directory, and the
/// original spelling is echoed.
#[test]
fn check_ignore_resolves_paths_from_cwd() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    let p = repo.path();
    std::fs::write(p.join(".gitignore"), "one\nignored-*\n").expect("root gitignore");
    std::fs::create_dir_all(p.join("a/b")).expect("mkdir a/b");
    std::fs::write(p.join("a/.gitignore"), "two*\n*three\n").expect("a gitignore");
    std::fs::write(p.join("a/b/.gitignore"), "!on*\n!two\n").expect("a/b gitignore");
    let inside = p.join("a");

    // AC1: `-v` reports the deciding rule with a repo-root-relative source.
    let out = run_libra_command(
        &["check-ignore", "--verbose", "3-three", "three-not-this-one"],
        &inside,
    );
    assert_cli_success(&out, "AC1");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a/.gitignore:2:*three\t3-three\n",
        "AC1"
    );

    // AC2: paths are resolved from `a/`, and echoed as typed.
    let out = run_libra_command(
        &["check-ignore", "3-three", "../one", "../not-ignored"],
        &inside,
    );
    assert_cli_success(&out, "AC2");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "3-three\n../one\n",
        "AC2"
    );

    // AC3: `-n` prints the non-matching path with empty source/pattern fields.
    let out = run_libra_command(
        &["check-ignore", "-v", "-n", "3-three", "../not-ignored"],
        &inside,
    );
    assert_cli_success(&out, "AC3");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a/.gitignore:2:*three\t3-three\n::\t../not-ignored\n",
        "AC3"
    );

    // AC4: escaping the repository is an invalid target.
    let out = run_libra_command(&["check-ignore", "../../outside"], &inside);
    assert!(!out.status.success(), "AC4 must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-003"),
        "AC4 code"
    );
}

/// PM-20: `--quiet`/`-z` argument rules and negated `-v` matches.
#[test]
fn check_ignore_quiet_and_argument_rules() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    let p = repo.path();
    std::fs::write(p.join(".gitignore"), "one\nignored-*\n").expect("root gitignore");
    std::fs::create_dir_all(p.join("a/b")).expect("mkdir a/b");
    std::fs::write(p.join("a/.gitignore"), "two*\n*three\n").expect("a gitignore");
    std::fs::write(p.join("a/b/.gitignore"), "!on*\n!two\n").expect("a/b gitignore");

    // AC1: quiet + ignored path -> exit 0, no stdout.
    let out = run_libra_command(&["check-ignore", "--quiet", "one"], p);
    assert!(out.status.success(), "AC1 exit");
    assert!(out.stdout.is_empty(), "AC1 stdout");

    // AC2: quiet + not ignored -> exit 1, no stdout.
    let out = run_libra_command(&["check-ignore", "--quiet", "not-ignored"], p);
    assert_eq!(out.status.code(), Some(1), "AC2 exit");
    assert!(out.stdout.is_empty(), "AC2 stdout");

    // AC3/AC4/AC5: invalid combinations are usage errors.
    for args in [
        vec!["check-ignore", "--quiet", "one", "two"],
        vec!["check-ignore", "--quiet", "-v", "one"],
        vec!["check-ignore", "-z", "one"],
    ] {
        let out = run_libra_command(&args, p);
        assert!(!out.status.success(), "{args:?} must fail");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-002"),
            "{args:?} code"
        );
    }

    // AC6: a negated rule still counts as a match under `-v`.
    let out = run_libra_command(&["check-ignore", "-v", "a/b/one"], p);
    assert!(out.status.success(), "AC6 exit");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "a/b/.gitignore:1:!on*\ta/b/one\n",
        "AC6"
    );

    // AC7: `--quiet --stdin` is legal and silent.
    let out = run_libra_command_with_stdin(&["check-ignore", "--quiet", "--stdin"], p, "one\n");
    assert!(out.status.success(), "AC7 exit");
    assert!(out.stdout.is_empty(), "AC7 stdout");
}

fn stream_repo() -> tempfile::TempDir {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    std::fs::write(repo.path().join(".gitignore"), "one\nignored-*\n").expect("root");
    std::fs::create_dir_all(repo.path().join("a/b")).expect("mkdir");
    std::fs::write(repo.path().join("a/.gitignore"), "two*\n*three\n").expect("a");
    std::fs::write(repo.path().join("a/b/.gitignore"), "!on*\n!two\n").expect("a/b");
    repo
}

/// Background reader: forwards each delimited field over a channel so the test
/// can bound every read with a timeout while stdin stays open.
fn spawn_field_reader(
    stdout: std::process::ChildStdout,
    delimiter: u8,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    use std::io::Read;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stdout = stdout;
        loop {
            let mut field = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                match stdout.read(&mut byte) {
                    Ok(0) => return,
                    Ok(_) => {
                        if byte[0] == delimiter {
                            break;
                        }
                        field.push(byte[0]);
                    }
                    Err(_) => return,
                }
            }
            if tx.send(field).is_err() {
                return;
            }
        }
    });
    rx
}

/// PM-31: `--stdin` answers each record as soon as it arrives.
#[test]
fn check_ignore_stdin_streams_each_line() {
    use std::io::Write;
    use std::process::Stdio;
    use std::time::Duration;

    let repo = stream_repo();
    let mut command =
        super::base_libra_command(&["check-ignore", "-n", "-v", "--stdin"], repo.path());
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn check-ignore");
    let mut stdin = child.stdin.take().expect("stdin");
    let fields = spawn_field_reader(child.stdout.take().expect("stdout"), b'\n');

    stdin.write_all(b"one\n").expect("write one");
    stdin.flush().expect("flush one");
    let first = fields
        .recv_timeout(Duration::from_secs(10))
        .expect("AC1 streamed within 10s");
    assert_eq!(
        String::from_utf8_lossy(&first),
        ".gitignore:1:one\tone",
        "AC1"
    );

    stdin.write_all(b"two\n").expect("write two");
    stdin.flush().expect("flush two");
    let second = fields
        .recv_timeout(Duration::from_secs(10))
        .expect("AC2 streamed within 10s");
    assert_eq!(String::from_utf8_lossy(&second), "::\ttwo", "AC2");

    drop(stdin);
    let _ = child.wait();
}

/// PM-31: C quoting on line input, and NUL fields for `-z -v`.
#[test]
fn check_ignore_stdin_c_quoting() {
    use std::io::Write;
    use std::process::Stdio;
    use std::time::Duration;

    let repo = stream_repo();
    let stdin_text = "one\n\"a/b/one two\"\n\"a/b/one\\\"three\"\nnot-ignored\n\"a/space dir/x\"\n";
    let out =
        run_libra_command_with_stdin(&["check-ignore", "-v", "--stdin"], repo.path(), stdin_text);
    assert_cli_success(&out, "AC3");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        ".gitignore:1:one\tone\n\
a/b/.gitignore:1:!on*\t\"a/b/one two\"\n\
a/b/.gitignore:1:!on*\t\"a/b/one\\\"three\"\n",
        "AC3"
    );

    let out = run_libra_command_with_stdin(
        &["check-ignore", "-v", "-n", "--stdin"],
        repo.path(),
        stdin_text,
    );
    assert_cli_success(&out, "AC4");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert_eq!(text.matches("::\t").count(), 2, "AC4: {text}");

    let out = run_libra_command_with_stdin(&["check-ignore", "--stdin"], repo.path(), "\"bad");
    assert!(!out.status.success(), "AC5 must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-002"),
        "AC5 code"
    );

    // AC6: NUL fields, no quoting.
    let out = run_libra_command_with_stdin(
        &["check-ignore", "-z", "-v", "--stdin"],
        repo.path(),
        "one\0a/b/one\"x\0not-ignored\0",
    );
    assert_cli_success(&out, "AC6");
    assert_eq!(
        out.stdout,
        [
            b".gitignore\0".as_slice(),
            b"1\0",
            b"one\0",
            b"one\0",
            b"a/b/.gitignore\0",
            b"1\0",
            b"!on*\0",
            b"a/b/one\"x\0",
        ]
        .concat(),
        "AC6"
    );

    // AC7: the first match is streamed while stdin stays open.
    let mut command =
        super::base_libra_command(&["check-ignore", "-z", "-v", "--stdin"], repo.path());
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn");
    let mut stdin = child.stdin.take().expect("stdin");
    let fields = spawn_field_reader(child.stdout.take().expect("stdout"), 0);
    stdin.write_all(b"one\0").expect("write one");
    stdin.flush().expect("flush one");
    for expected in [".gitignore", "1", "one", "one"] {
        let field = fields
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("AC7 field '{expected}' within 10s"));
        assert_eq!(String::from_utf8_lossy(&field), expected, "AC7");
    }
    drop(stdin);
    let _ = child.wait();
}
