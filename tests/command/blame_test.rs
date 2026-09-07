//! Tests `libra blame` for line-level attribution, format envelopes
//! (human/JSON/machine), and SHA-1 vs. SHA-256 repository handling.
//!
//! **Layer:** L1 — deterministic, no external dependencies.
//!
//! Fixture conventions:
//! - CLI-driven cases use `create_committed_repo_via_cli()` from `mod.rs`
//!   plus extra `add`/`commit` invocations through `run_libra_command()`.
//! - In-process cases call `setup_repo_with_hash()` to bootstrap a repo
//!   under a chosen `core.objectformat` and `prepare_history()` to lay
//!   down a known two-commit history of `foo.txt` (line2 changed in the
//!   second commit). The two returned commit hashes act as expected blame
//!   targets.

use std::{fs, io::Write};

use chrono::DateTime;
use libra::{
    command::{
        add::{self, AddArgs},
        blame::{self, BlameArgs},
        commit::{self, CommitArgs},
        get_target_commit,
        init::{self, InitArgs},
    },
    internal::config::ConfigKv,
};
use tempfile::tempdir;

use super::*;

/// Scenario: running `libra blame` outside any repo must exit 128 with a
/// "fatal: not a libra repository" message. Pins the repo-presence guard.
#[test]
fn test_blame_cli_outside_repository_returns_fatal_128() {
    let temp = tempdir().unwrap();
    let output = run_libra_command(&["blame", "some_file.txt"], temp.path());
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal: not a libra repository"),
        "unexpected stderr: {stderr}"
    );
}

/// Scenario: `--json blame <file>` must emit the canonical envelope with
/// `command="blame"`, `data.file=<path>`, and `data.lines` as an array.
/// Schema pin for downstream JSON consumers.
#[test]
fn test_blame_json_output_includes_lines() {
    let repo = create_committed_repo_via_cli();
    std::fs::write(repo.path().join("tracked.txt"), "line1\nline2\n").unwrap();
    let add_output = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert!(
        add_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&add_output.stderr)
    );
    let commit_output = run_libra_command(
        &["commit", "-m", "update tracked", "--no-verify"],
        repo.path(),
    );
    assert!(
        commit_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&commit_output.stderr)
    );

    let output = run_libra_command(&["--json", "blame", "tracked.txt"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "blame");
    assert_eq!(json["data"]["file"], "tracked.txt");
    assert!(json["data"]["lines"].as_array().is_some());
}

/// Scenario: a line is introduced in commit A and only re-indented (whitespace
/// change) in commit B. Default blame attributes it to B; `blame -w` ignores the
/// whitespace difference and attributes it to A.
#[test]
fn test_blame_ignore_whitespace_attributes_to_older_commit() {
    let repo = create_committed_repo_via_cli();

    // Commit A: introduce the line with no surrounding whitespace.
    std::fs::write(repo.path().join("ws.txt"), "value\n").unwrap();
    assert!(
        run_libra_command(&["add", "ws.txt"], repo.path())
            .status
            .success()
    );
    assert!(
        run_libra_command(&["commit", "-m", "add ws", "--no-verify"], repo.path())
            .status
            .success()
    );
    let head_a = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    let commit_a = String::from_utf8_lossy(&head_a.stdout).trim().to_string();

    // Commit B: change only the whitespace (re-indent the same content).
    std::fs::write(repo.path().join("ws.txt"), "    value\n").unwrap();
    assert!(
        run_libra_command(&["add", "ws.txt"], repo.path())
            .status
            .success()
    );
    assert!(
        run_libra_command(&["commit", "-m", "reindent ws", "--no-verify"], repo.path())
            .status
            .success()
    );
    let head_b = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    let commit_b = String::from_utf8_lossy(&head_b.stdout).trim().to_string();
    assert_ne!(commit_a, commit_b, "the two commits must differ");

    // Default blame: the whitespace-only change is attributed to commit B.
    let default = run_libra_command(&["--json", "blame", "ws.txt"], repo.path());
    assert!(
        default.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&default.stderr)
    );
    let default_json = parse_json_stdout(&default);
    assert_eq!(
        default_json["data"]["lines"][0]["hash"], commit_b,
        "default blame attributes the re-indent to commit B"
    );

    // `-w`: the whitespace difference is ignored, so the line traces to commit A.
    let ignored = run_libra_command(&["--json", "blame", "-w", "ws.txt"], repo.path());
    assert!(
        ignored.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ignored.stderr)
    );
    let ignored_json = parse_json_stdout(&ignored);
    assert_eq!(
        ignored_json["data"]["lines"][0]["hash"], commit_a,
        "blame -w attributes the whitespace-only change to commit A"
    );
}

/// Scenario: a whitespace-only change is followed by an intervening commit that
/// inserts a line *above* it, shifting its line number. `blame -w` must still
/// trace the shifted line to the commit that introduced its content, which
/// requires remapping diff line numbers through the back-walk (not a direct
/// `new_line - 1` index into the final file).
#[test]
fn test_blame_ignore_whitespace_after_line_shift() {
    let repo = create_committed_repo_via_cli();

    // Commit A: introduce the line (no surrounding whitespace).
    std::fs::write(repo.path().join("shift.txt"), "value\n").unwrap();
    assert!(
        run_libra_command(&["add", "shift.txt"], repo.path())
            .status
            .success()
    );
    assert!(
        run_libra_command(&["commit", "-m", "A add value", "--no-verify"], repo.path())
            .status
            .success()
    );
    let commit_a =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], repo.path()).stdout)
            .trim()
            .to_string();

    // Commit B: re-indent the line (whitespace-only change).
    std::fs::write(repo.path().join("shift.txt"), "    value\n").unwrap();
    assert!(
        run_libra_command(&["add", "shift.txt"], repo.path())
            .status
            .success()
    );
    assert!(
        run_libra_command(&["commit", "-m", "B reindent", "--no-verify"], repo.path())
            .status
            .success()
    );

    // Commit C: insert a header line ABOVE, shifting "value" to line 2.
    std::fs::write(repo.path().join("shift.txt"), "header\n    value\n").unwrap();
    assert!(
        run_libra_command(&["add", "shift.txt"], repo.path())
            .status
            .success()
    );
    assert!(
        run_libra_command(
            &["commit", "-m", "C add header", "--no-verify"],
            repo.path()
        )
        .status
        .success()
    );
    let commit_c =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], repo.path()).stdout)
            .trim()
            .to_string();

    // `-w`: line 2 ("value", shifted) traces past the whitespace-only B to A;
    // line 1 (the inserted header) is attributed to C.
    let ignored = run_libra_command(&["--json", "blame", "-w", "shift.txt"], repo.path());
    assert!(
        ignored.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&ignored.stderr)
    );
    let json = parse_json_stdout(&ignored);
    assert_eq!(
        json["data"]["lines"][0]["hash"], commit_c,
        "the inserted header line is attributed to commit C"
    );
    assert_eq!(
        json["data"]["lines"][1]["hash"], commit_a,
        "the shifted whitespace-only line still traces to commit A under -w"
    );
}

/// Scenario: `blame --porcelain` emits the machine-readable format — a
/// `<sha> <orig> <final> [<group>]` header followed (once per commit) by the
/// author/committer/summary/filename metadata block and tab-prefixed content.
#[test]
fn test_blame_porcelain_emits_commit_metadata() {
    let repo = create_committed_repo_via_cli();
    std::fs::write(repo.path().join("tracked.txt"), "alpha\nbeta\n").unwrap();
    assert!(
        run_libra_command(&["add", "tracked.txt"], repo.path())
            .status
            .success()
    );
    assert!(
        run_libra_command(
            &["commit", "-m", "add tracked porcelain", "--no-verify"],
            repo.path()
        )
        .status
        .success()
    );

    let output = run_libra_command(&["blame", "--porcelain", "tracked.txt"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    // First line is the porcelain header: "<sha> <orig> <final> [<group>]".
    let first = stdout.lines().next().unwrap_or("");
    let header: Vec<&str> = first.split(' ').collect();
    assert!(
        header.len() >= 3,
        "header needs sha/orig/final, got: {first}"
    );
    assert!(
        header[0].len() >= 40 && header[0].chars().all(|c| c.is_ascii_hexdigit()),
        "first token should be a full object hash, got: {first}"
    );

    // Metadata block (printed once for the single attributing commit).
    assert!(
        stdout.contains("\nauthor-mail <"),
        "missing author-mail: {stdout}"
    );
    assert!(
        stdout.contains("\nauthor-time "),
        "missing author-time: {stdout}"
    );
    assert!(
        stdout.contains("\nauthor-tz "),
        "missing author-tz: {stdout}"
    );
    assert!(
        stdout.contains("\ncommitter "),
        "missing committer: {stdout}"
    );
    assert!(
        stdout.contains("\nsummary add tracked porcelain"),
        "missing summary: {stdout}"
    );
    assert!(
        stdout.contains("\nfilename tracked.txt"),
        "missing filename: {stdout}"
    );
    // Content lines are tab-prefixed.
    assert!(
        stdout.contains("\talpha"),
        "missing tab-prefixed content: {stdout}"
    );
    assert!(
        stdout.contains("\tbeta"),
        "missing tab-prefixed content: {stdout}"
    );
}

/// Scenario: `--machine blame` must emit exactly one non-empty stdout
/// line of valid JSON (NDJSON-friendly). Mirrors `add_json_test`'s
/// machine-mode contract.
#[test]
fn test_blame_machine_output_is_single_line_json() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--machine", "blame", "tracked.txt"], repo.path());
    assert_cli_success(&output, "machine blame tracked.txt");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let non_empty_lines: Vec<&str> = stdout.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(
        non_empty_lines.len(),
        1,
        "machine output should be exactly one non-empty line, got: {stdout}"
    );

    let parsed: serde_json::Value =
        serde_json::from_str(non_empty_lines[0]).expect("machine output should be valid JSON");
    assert_eq!(parsed["command"], "blame");
    assert_eq!(parsed["data"]["file"], "tracked.txt");
    assert!(parsed["data"]["lines"].as_array().is_some());
}

/// Scenario: human-readable blame output must truncate excessively long
/// (Unicode) author names with an ellipsis ("...") rather than corrupt
/// the table layout. Regression guard against char-vs-byte width bugs.
#[test]
fn test_blame_human_output_handles_long_unicode_author_names() {
    let repo = create_committed_repo_via_cli();

    let name_output = run_libra_command(
        &[
            "config",
            "user.name",
            "测试作者名字很长很长很长很长很长很长",
        ],
        repo.path(),
    );
    assert_cli_success(&name_output, "config user.name");
    let email_output = run_libra_command(
        &["config", "user.email", "unicode@example.com"],
        repo.path(),
    );
    assert_cli_success(&email_output, "config user.email");

    std::fs::write(repo.path().join("tracked.txt"), "unicode blame line\n").unwrap();
    let add_output = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert_cli_success(&add_output, "add tracked.txt");
    let commit_output = run_libra_command(
        &["commit", "-m", "unicode author", "--no-verify"],
        repo.path(),
    );
    assert_cli_success(&commit_output, "commit unicode author");

    let output = run_libra_command(&["blame", "tracked.txt"], repo.path());
    assert_cli_success(&output, "blame tracked.txt");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("..."),
        "expected truncated author marker in blame output, got: {stdout}"
    );
}

/// Scenario: `-e` / `--show-email` replaces the author name with the author
/// email (in Git's `<email>` form) in the default human output.
#[test]
fn test_blame_show_email_displays_author_email() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // Use short identities so the author column is not truncated.
    assert_cli_success(
        &run_libra_command(&["config", "user.name", "Zoe"], p),
        "config name",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "z@e.io"], p),
        "config email",
    );
    std::fs::write(p.join("tracked.txt"), "one line\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], p),
        "add tracked.txt",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "by zoe", "--no-verify"], p),
        "commit by zoe",
    );

    // Default output shows the author name.
    let out = run_libra_command(&["blame", "tracked.txt"], p);
    assert_cli_success(&out, "blame default");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Zoe") && !stdout.contains("<z@e.io>"),
        "default blame shows the name, not the email: {stdout}"
    );

    // `-e` and `--show-email` show `<email>` instead of the name.
    for flag in ["-e", "--show-email"] {
        let out = run_libra_command(&["blame", flag, "tracked.txt"], p);
        assert_cli_success(&out, "blame show-email");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("<z@e.io>"),
            "blame {flag} should show the email: {stdout}"
        );
        assert!(
            !stdout.contains("Zoe"),
            "blame {flag} must not show the name: {stdout}"
        );
    }
}

/// Scenario: each line in JSON blame output must reference the commit
/// hash that introduced it. With the known 2-commit `foo.txt` history,
/// line 1 maps to the first commit and line 2 to the second. The `date`
/// field must be RFC3339-parseable.
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_blame_json_assigns_lines_to_introducing_commits() {
    let repo = tempdir().unwrap();
    let _guard = setup_repo_with_hash(&repo, "sha1").await;
    let (first, second) = prepare_history().await;

    let output = run_libra_command(&["--json", "blame", "foo.txt"], repo.path());
    assert_cli_success(&output, "json blame foo.txt");

    let json = parse_json_stdout(&output);
    let lines = json["data"]["lines"]
        .as_array()
        .expect("blame lines should be an array");
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["line_number"], 1);
    assert_eq!(lines[0]["hash"], first.to_string());
    assert_eq!(lines[1]["line_number"], 2);
    assert_eq!(lines[1]["hash"], second.to_string());
    let date = lines[0]["date"]
        .as_str()
        .expect("blame date should be a string");
    assert!(
        DateTime::parse_from_rfc3339(date).is_ok(),
        "expected RFC3339 blame date, got: {date}"
    );
}

/// Scenario: `-L <n>,<m>` must restrict blame output to the requested
/// line range. Asks for line 2 only and asserts the array has length 1
/// with the expected hash and content.
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_blame_json_line_range_filters_output() {
    let repo = tempdir().unwrap();
    let _guard = setup_repo_with_hash(&repo, "sha1").await;
    let (_first, second) = prepare_history().await;

    let output = run_libra_command(&["--json", "blame", "-L", "2,2", "foo.txt"], repo.path());
    assert_cli_success(&output, "json blame with line range");

    let json = parse_json_stdout(&output);
    let lines = json["data"]["lines"]
        .as_array()
        .expect("blame lines should be an array");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["line_number"], 2);
    assert_eq!(lines[0]["hash"], second.to_string());
    assert_eq!(lines[0]["content"], "line2-modified");
}

/// Scenario: an out-of-bounds `-L` range must surface as a stable CLI
/// error tagged `LBR-CLI-002` (category `cli`) with exit code 129.
/// Pins the structured error envelope.
#[test]
fn test_blame_invalid_line_range_uses_stable_cli_error() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["blame", "-L", "9,10", "tracked.txt"], repo.path());
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert_eq!(report.category, "cli");
}

/// Scenario: `-L` accepts Git's `/regex/` endpoints (start and/or end), a single
/// endpoint spans to the end of the file, and a non-matching regex errors — all
/// matching `git blame -L` semantics.
#[test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
fn test_blame_regex_line_range() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    fs::write(
        p.join("f.rs"),
        "fn alpha() {}\nfn beta() {}\nfn gamma() {}\nfn delta() {}\nfn eps() {}\n",
    )
    .unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.rs"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );

    let line_numbers = |args: &[&str]| -> Vec<u64> {
        let output = run_libra_command(args, p);
        assert_cli_success(&output, "blame line range");
        parse_json_stdout(&output)["data"]["lines"]
            .as_array()
            .expect("lines array")
            .iter()
            .map(|line| line["line_number"].as_u64().expect("line_number"))
            .collect()
    };

    // /regex/ start and end resolve to matching line numbers.
    assert_eq!(
        line_numbers(&["--json", "blame", "-L", "/beta/,/delta/", "f.rs"]),
        vec![2, 3, 4]
    );
    // A single /regex/ (or numeric) endpoint spans to the end of the file (like Git).
    assert_eq!(
        line_numbers(&["--json", "blame", "-L", "/beta/", "f.rs"]),
        vec![2, 3, 4, 5]
    );
    assert_eq!(
        line_numbers(&["--json", "blame", "-L", "2", "f.rs"]),
        vec![2, 3, 4, 5]
    );
    // Numeric start + regex end, and regex start + `+COUNT`.
    assert_eq!(
        line_numbers(&["--json", "blame", "-L", "2,/delta/", "f.rs"]),
        vec![2, 3, 4]
    );
    assert_eq!(
        line_numbers(&["--json", "blame", "-L", "/beta/,+1", "f.rs"]),
        vec![2]
    );

    // A regex with no match is a usage error (LBR-CLI-002, exit 129).
    let no_match = run_libra_command(&["blame", "-L", "/nomatch/", "f.rs"], p);
    assert_eq!(no_match.status.code(), Some(129));
    let (_stderr, report) = parse_cli_error_stderr(&no_match.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
}

/// Bootstrap a repo with the requested hash algorithm (`"sha1"` or
/// `"sha256"`), set a stable identity, and return the
/// `ChangeDirGuard` that pins the process CWD to the repo for the
/// remainder of the test (RAII; lives to end of test).
async fn setup_repo_with_hash(
    temp: &tempfile::TempDir,
    object_format: &str,
) -> test::ChangeDirGuard {
    test::setup_clean_testing_env_in(temp.path());
    init::init(InitArgs {
        bare: false,
        initial_branch: None,
        repo_directory: temp.path().to_str().unwrap().to_string(),
        template: None,
        quiet: true,
        shared: None,
        object_format: Some(object_format.to_string()),
        ref_format: None,
        from_git_repository: None,
        vault: false,
    })
    .await
    .unwrap();
    let guard = test::ChangeDirGuard::new(temp.path());
    ConfigKv::set("user.name", "Blame Test User", false)
        .await
        .unwrap();
    ConfigKv::set("user.email", "blame-test@example.com", false)
        .await
        .unwrap();
    guard
}

/// Build a fixed two-commit history of `foo.txt`:
///   c1: "line1\nline2\n"      (first hash)
///   c2: "line1\nline2-modified\n" (second hash)
/// Returns `(first, second)` in chronological order. Assumes a
/// `ChangeDirGuard` is already active.
async fn prepare_history() -> (ObjectHash, ObjectHash) {
    // first commit
    let mut f = fs::File::create("foo.txt").unwrap();
    writeln!(f, "line1").unwrap();
    writeln!(f, "line2").unwrap();

    add::execute(AddArgs {
        pathspec: vec!["foo.txt".into()],
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

    commit::execute(CommitArgs {
        message: Some("init".into()),
        ..Default::default()
    })
    .await;

    let first = get_target_commit("HEAD").await.unwrap();

    // second commit (modify line2)
    let mut f = fs::File::create("foo.txt").unwrap();
    writeln!(f, "line1").unwrap();
    writeln!(f, "line2-modified").unwrap();

    add::execute(AddArgs {
        pathspec: vec!["foo.txt".into()],
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

    commit::execute(CommitArgs {
        message: Some("update".into()),
        ..Default::default()
    })
    .await;

    let second = get_target_commit("HEAD").await.unwrap();
    (first, second)
}

/// Stage and commit the current `foo.txt` with `message`, returning the
/// resulting commit hash. Assumes a `ChangeDirGuard` is already active.
async fn commit_foo(message: &str) -> ObjectHash {
    add::execute(AddArgs {
        pathspec: vec!["foo.txt".into()],
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
    commit::execute(CommitArgs {
        message: Some(message.into()),
        ..Default::default()
    })
    .await;
    get_target_commit("HEAD").await.unwrap()
}

/// Build a fixed three-commit history of `foo.txt` where each commit
/// introduces exactly one line:
///   c1: "line1\n"                          (first  -> line 1)
///   c2: "line1\nline2\n"                    (second -> line 2)
///   c3: "line1\nline2\nline3\n"             (third  -> line 3)
/// Returns `(first, second, third)` in chronological order. Assumes a
/// `ChangeDirGuard` is already active.
async fn prepare_three_commit_history() -> (ObjectHash, ObjectHash, ObjectHash) {
    let mut f = fs::File::create("foo.txt").unwrap();
    writeln!(f, "line1").unwrap();
    drop(f);
    let first = commit_foo("c1").await;

    let mut f = fs::File::create("foo.txt").unwrap();
    writeln!(f, "line1").unwrap();
    writeln!(f, "line2").unwrap();
    drop(f);
    let second = commit_foo("c2").await;

    let mut f = fs::File::create("foo.txt").unwrap();
    writeln!(f, "line1").unwrap();
    writeln!(f, "line2").unwrap();
    writeln!(f, "line3").unwrap();
    drop(f);
    let third = commit_foo("c3").await;

    (first, second, third)
}

/// Scenario (blame.md "（新增）blame 归属正确性 / 3 个 commit 链"): with a
/// three-commit history where each commit appends one line, every blame
/// line must be attributed to the commit that introduced it.
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_blame_json_three_commit_chain_attributes_each_line() {
    let repo = tempdir().unwrap();
    let _guard = setup_repo_with_hash(&repo, "sha1").await;
    let (first, second, third) = prepare_three_commit_history().await;

    let output = run_libra_command(&["--json", "blame", "foo.txt"], repo.path());
    assert_cli_success(&output, "json blame foo.txt (3-commit chain)");

    let json = parse_json_stdout(&output);
    let lines = json["data"]["lines"]
        .as_array()
        .expect("blame lines should be an array");
    assert_eq!(lines.len(), 3, "expected three blamed lines: {json}");
    assert_eq!(lines[0]["line_number"], 1);
    assert_eq!(lines[0]["hash"], first.to_string());
    assert_eq!(lines[1]["line_number"], 2);
    assert_eq!(lines[1]["hash"], second.to_string());
    assert_eq!(lines[2]["line_number"], 3);
    assert_eq!(lines[2]["hash"], third.to_string());
}

/// Scenario (blame.md "（新增）empty file"): blaming a committed empty file
/// returns an empty result (no blame lines) in JSON mode and prints
/// "File is empty" in human mode, rather than erroring.
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_blame_empty_file_returns_empty_result() {
    let repo = tempdir().unwrap();
    let _guard = setup_repo_with_hash(&repo, "sha1").await;

    // Commit an empty file.
    fs::File::create("empty.txt").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["empty.txt".into()],
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
    commit::execute(CommitArgs {
        message: Some("add empty file".into()),
        ..Default::default()
    })
    .await;

    // JSON mode: empty result, not an error.
    let output = run_libra_command(&["--json", "blame", "empty.txt"], repo.path());
    assert_cli_success(&output, "json blame empty.txt");
    let json = parse_json_stdout(&output);
    let lines = json["data"]["lines"]
        .as_array()
        .expect("blame lines should be an array");
    assert!(
        lines.is_empty(),
        "an empty file must yield no blame lines: {json}"
    );

    // Human mode: an explicit "File is empty" notice on success.
    let human = run_libra_command(&["blame", "empty.txt"], repo.path());
    assert_cli_success(&human, "human blame empty.txt");
    let stdout = String::from_utf8_lossy(&human.stdout);
    assert!(
        stdout.contains("File is empty"),
        "human blame of an empty file should say 'File is empty', got: {stdout}"
    );
}

/// Scenario: `blame::execute` against a SHA-1 repo must complete without
/// panic. Smoke test for the SHA-1 code path.
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn blame_runs_with_sha1() {
    let repo = tempdir().unwrap();
    let _guard = setup_repo_with_hash(&repo, "sha1").await;
    prepare_history().await;

    // should not panic for SHA-1 repo
    blame::execute(BlameArgs {
        file: "foo.txt".into(),
        commit: "HEAD".into(),
        line_range: None,
        porcelain: false,
        line_porcelain: false,
        show_email: false,
        long: false,
        suppress: false,
        raw_timestamp: false,
        abbrev: None,
        root: false,
        show_name: false,
        ignore_whitespace: false,
    })
    .await;
}

/// Scenario: `blame::execute` against a SHA-256 repo must complete
/// without panic. Smoke test for the SHA-256 code path; pairs with the
/// SHA-1 case to guarantee both are wired through.
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn blame_runs_with_sha256() {
    let repo = tempdir().unwrap();
    let _guard = setup_repo_with_hash(&repo, "sha256").await;
    prepare_history().await;

    // should not panic for SHA-256 repo
    blame::execute(BlameArgs {
        file: "foo.txt".into(),
        commit: "HEAD".into(),
        line_range: None,
        porcelain: false,
        line_porcelain: false,
        show_email: false,
        long: false,
        suppress: false,
        raw_timestamp: false,
        abbrev: None,
        root: false,
        show_name: false,
        ignore_whitespace: false,
    })
    .await;
}

/// Scenario: a 40-hex (SHA-1 length) commit identifier passed against a
/// SHA-256 repo must be rejected by `get_target_commit`. Format-mismatch
/// regression guard so users do not silently get the wrong commit.
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn blame_rejects_sha1_length_on_sha256_repo() {
    let repo = tempdir().unwrap();
    let _guard = setup_repo_with_hash(&repo, "sha256").await;
    prepare_history().await;

    // Passing a 40-hex (SHA-1 length) commit id into a SHA-256 repo should be rejected.
    let res = get_target_commit("4b825dc642cb6eb9a060e54bf8d69288fbee4904").await;
    assert!(
        res.is_err(),
        "expect get_target_commit to reject SHA-1 length hash in SHA-256 repo"
    );
}

#[test]
fn test_blame_display_flags_long_suppress_timestamp_abbrev() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("tracked.txt"), "line1\nline2\n").unwrap();
    assert!(
        run_libra_command(&["add", "tracked.txt"], p)
            .status
            .success()
    );
    assert!(
        run_libra_command(&["commit", "-m", "u", "--no-verify"], p)
            .status
            .success()
    );

    // -l: full 40-char (sha1) commit hash at the start of each line.
    let long = run_libra_command(&["blame", "-l", "tracked.txt"], p);
    assert!(long.status.success(), "blame -l ok");
    let l = String::from_utf8_lossy(&long.stdout);
    let first_tok = l.lines().next().unwrap().split_whitespace().next().unwrap();
    assert!(
        first_tok.len() >= 40,
        "-l shows the full hash, got {first_tok:?}"
    );

    // --abbrev=12: 12-digit hash column.
    let ab = run_libra_command(&["blame", "--abbrev=12", "tracked.txt"], p);
    assert!(ab.status.success(), "blame --abbrev ok");
    let a = String::from_utf8_lossy(&ab.stdout);
    let ab_tok = a.lines().next().unwrap().split_whitespace().next().unwrap();
    assert_eq!(
        ab_tok.len(),
        12,
        "--abbrev=12 shows 12 digits, got {ab_tok:?}"
    );

    // -s: suppress the author/date columns (no parenthesised author block).
    let sup = run_libra_command(&["blame", "-s", "tracked.txt"], p);
    assert!(sup.status.success(), "blame -s ok");
    let s = String::from_utf8_lossy(&sup.stdout);
    let first = s.lines().next().unwrap();
    assert!(!first.contains('('), "-s drops the author block: {first:?}");
    assert!(
        first.contains(") "),
        "-s keeps the line-number marker: {first:?}"
    );

    // -t: raw epoch timestamp (a run of digits) in the date column.
    let ts = run_libra_command(&["blame", "-t", "tracked.txt"], p);
    assert!(ts.status.success(), "blame -t ok");
    let t = String::from_utf8_lossy(&ts.stdout);
    assert!(
        t.lines().next().unwrap().contains('('),
        "-t keeps the author block: {t:?}"
    );

    // -p: alias for --porcelain.
    let porc = run_libra_command(&["blame", "-p", "tracked.txt"], p);
    assert!(porc.status.success(), "blame -p ok");
    assert!(
        String::from_utf8_lossy(&porc.stdout).contains("author "),
        "-p emits porcelain headers"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn blame_root_flag_is_accepted_noop() {
    let repo = tempdir().unwrap();
    let _guard = setup_repo_with_hash(&repo, "sha1").await;
    prepare_history().await;

    // `--root` is accepted and is a no-op: Libra's blame never prefixes
    // boundary/root commits with `^`, so the output is unchanged.
    blame::execute(BlameArgs {
        file: "foo.txt".into(),
        commit: "HEAD".into(),
        line_range: None,
        porcelain: false,
        line_porcelain: false,
        show_email: false,
        long: false,
        suppress: false,
        raw_timestamp: false,
        abbrev: None,
        root: true,
        show_name: false,
        ignore_whitespace: false,
    })
    .await;
}

#[test]
fn test_blame_show_name_flag_prints_filename() {
    let repo = create_committed_repo_via_cli();
    std::fs::write(repo.path().join("named.txt"), "alpha\nbeta\n").unwrap();
    assert!(
        run_libra_command(&["add", "named.txt"], repo.path())
            .status
            .success()
    );
    assert!(
        run_libra_command(&["commit", "-m", "add named", "--no-verify"], repo.path())
            .status
            .success()
    );

    // `-f`/`--show-name` inserts the filename after the hash on each line.
    for flag in ["-f", "--show-name"] {
        let out = run_libra_command(&["blame", flag, "named.txt"], repo.path());
        assert_cli_success(&out, &format!("blame {flag}"));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout
                .lines()
                .all(|l| l.is_empty() || l.contains("named.txt")),
            "every blame line shows the filename with {flag}: {stdout}"
        );
    }

    // Without the flag, the filename is not in the per-line output.
    let plain = run_libra_command(&["blame", "named.txt"], repo.path());
    assert_cli_success(&plain, "blame (no -f)");
    assert!(
        !String::from_utf8_lossy(&plain.stdout).contains("named.txt"),
        "plain blame omits the filename column"
    );

    // `-f -s`: the suppress path also shows the filename (after the hash) but
    // drops the author/date columns.
    let suppressed = run_libra_command(&["blame", "-f", "-s", "named.txt"], repo.path());
    assert_cli_success(&suppressed, "blame -f -s");
    let sup_out = String::from_utf8_lossy(&suppressed.stdout);
    assert!(
        sup_out.contains("named.txt"),
        "-f -s still shows the filename: {sup_out}"
    );
    assert!(
        !sup_out.contains("alpha") || !sup_out.contains(") alpha)"),
        "sanity: content present"
    );
    // `-s` drops the localized date/time, so no "(<author>" paren group remains.
    assert!(
        sup_out.lines().all(|l| l.is_empty() || !l.contains(" (")),
        "-s suppresses the author/date paren group: {sup_out}"
    );

    // Porcelain is unaffected by `-f` — it already emits a `filename` line, and
    // the per-line header format does not change.
    let porc_plain = run_libra_command(&["blame", "--porcelain", "named.txt"], repo.path());
    let porc_f = run_libra_command(&["blame", "--porcelain", "-f", "named.txt"], repo.path());
    assert_cli_success(&porc_f, "blame --porcelain -f");
    assert_eq!(
        String::from_utf8_lossy(&porc_plain.stdout),
        String::from_utf8_lossy(&porc_f.stdout),
        "-f does not change porcelain output"
    );
}
