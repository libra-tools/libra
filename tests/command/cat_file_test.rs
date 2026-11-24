//! Tests for the `cat-file` command, verifying object type, size, content
//! display, existence checks, AI object inspection, and structured-error
//! envelopes.
//!
//! **Layer:** L1 — deterministic, no external dependencies.
//!
//! Fixture conventions: each test uses `init_temp_repo()` to spawn a
//! fresh `libra init --vault false` repo in a tempdir, optionally calls
//! `configure_user_identity()` and `create_commit()` to lay down a known
//! object graph, and runs `libra cat-file ...` through `Command`. The
//! tests cross-reference object hashes by parsing the human-readable
//! output (`tree <hash>`, tree entries `mode blob <hash>\t<name>`); these
//! parsers must therefore stay in sync with the cat-file pretty-printer.

use std::{
    io::{Read, Write},
    process::Command,
};

use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};

use super::{
    loose_object_path, parse_cli_error_stderr, parse_json_stdout,
    run_libra_command_with_stdin_and_env,
};

/// Spawn `libra init --vault false` in a tempdir and return the `TempDir` (kept
/// alive by the caller for RAII cleanup).
fn init_temp_repo() -> tempfile::TempDir {
    let temp_dir = tempfile::tempdir().expect("Failed to create temporary directory");
    let temp_path = temp_dir.path();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["init", "--vault", "false"])
        .output()
        .expect("Failed to execute libra binary");

    if !output.status.success() {
        panic!(
            "Failed to initialize libra repository: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    temp_dir
}

/// Configure `user.name` / `user.email` through the CLI so subsequent
/// commits can be authored. Required before `create_commit()`.
fn configure_user_identity(temp_path: &std::path::Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["config", "user.name", "Test User"])
        .output()
        .expect("Failed to configure user.name");
    assert!(output.status.success(), "Failed to configure user.name");

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["config", "user.email", "test@example.com"])
        .output()
        .expect("Failed to configure user.email");
    assert!(output.status.success(), "Failed to configure user.email");
}

/// Write `content` to `filename`, stage it, and create a commit through
/// the CLI. Skips the pre-commit hook with `--no-verify` so the test does
/// not rely on hook availability.
fn create_commit(temp_path: &std::path::Path, filename: &str, content: &str, message: &str) {
    std::fs::write(temp_path.join(filename), content).expect("Failed to create file");

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["add", filename])
        .output()
        .expect("Failed to add file");
    assert!(
        output.status.success(),
        "Failed to add file: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["commit", "-m", message, "--no-verify"])
        .output()
        .expect("Failed to commit");
    assert!(
        output.status.success(),
        "Failed to commit: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Scenario: `cat-file -t HEAD` against a commit must print exactly
/// `commit` on stdout. Pins the canonical object-type vocabulary.
#[tokio::test]
async fn test_cat_file_type_commit() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);
    create_commit(temp_path, "hello.txt", "hello world\n", "first commit");

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-t", "HEAD"])
        .output()
        .expect("Failed to execute cat-file");

    assert!(
        output.status.success(),
        "cat-file -t failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "commit",
        "Expected type 'commit', got '{}'",
        stdout.trim()
    );
}

/// Scenario: `cat-file -s HEAD` must emit a positive numeric size.
/// Smoke test for the size pathway; the exact bytes are commit-shape
/// dependent so only `> 0` is asserted.
#[tokio::test]
async fn test_cat_file_size_commit() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);
    create_commit(temp_path, "hello.txt", "hello world\n", "first commit");

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-s", "HEAD"])
        .output()
        .expect("Failed to execute cat-file");

    assert!(
        output.status.success(),
        "cat-file -s failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let size: usize = stdout.trim().parse().expect("Expected a numeric size");
    assert!(size > 0, "Commit object size should be > 0, got {}", size);
}

/// Scenario: `cat-file --batch-check` reads object names from stdin and prints
/// `<sha> <type> <size>` per resolvable line and `<input> missing` otherwise.
#[tokio::test]
async fn test_cat_file_batch_check_reports_type_size_and_missing() {
    use std::process::Stdio;

    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);
    create_commit(temp_path, "hello.txt", "hello world\n", "first commit");

    let head = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("Failed to resolve HEAD");
    let head_hash = String::from_utf8_lossy(&head.stdout).trim().to_string();

    let mut child = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "--batch-check"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("Failed to spawn cat-file --batch-check");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"HEAD\nnot-a-real-ref\n")
        .expect("Failed to write batch-check input");
    let output = child
        .wait_with_output()
        .expect("Failed to wait on cat-file");
    assert!(
        output.status.success(),
        "cat-file --batch-check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "expected two output lines, got: {stdout}");
    assert!(
        lines[0].starts_with(&format!("{head_hash} commit ")),
        "first line should be '<hash> commit <size>', got: {}",
        lines[0]
    );
    let size_token = lines[0].rsplit(' ').next().unwrap_or("");
    assert!(
        size_token.parse::<usize>().is_ok(),
        "size should be numeric, got: {}",
        lines[0]
    );
    assert_eq!(lines[1], "not-a-real-ref missing");
}

/// A corrupt object is a repository failure, not an ordinary batch "missing"
/// response. This keeps automation from silently treating damaged storage as an
/// absent user-supplied name.
#[tokio::test]
async fn test_cat_file_batch_check_surfaces_corrupt_object() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);
    create_commit(temp_path, "hello.txt", "hello world\n", "first commit");

    let head = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("Failed to resolve HEAD");
    assert!(head.status.success());
    let head_hash = String::from_utf8_lossy(&head.stdout).trim().to_string();
    std::fs::write(
        loose_object_path(temp_path, &head_hash),
        b"not a zlib object",
    )
    .expect("Failed to corrupt HEAD object");

    let output = run_libra_command_with_stdin_and_env(
        &["cat-file", "--batch-check"],
        temp_path,
        &format!("{head_hash}\n"),
        &[("LIBRA_ERROR_JSON", "1")],
    );

    assert!(
        !output.status.success(),
        "corrupt object must fail batch mode"
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("missing"),
        "corrupt storage must not be reported as a missing object"
    );
    let (_, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-REPO-002");
}

/// Scenario: `cat-file --batch` prints the `<sha> <type> <size>` header AND the
/// raw object contents for each resolvable line, and `<input> missing` otherwise.
#[tokio::test]
async fn test_cat_file_batch_reports_header_and_contents() {
    use std::process::Stdio;

    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);
    create_commit(temp_path, "hello.txt", "hello world\n", "first commit");

    let head = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("Failed to resolve HEAD");
    let head_hash = String::from_utf8_lossy(&head.stdout).trim().to_string();

    let mut child = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("Failed to spawn cat-file --batch");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"HEAD\nnot-a-real-ref\n")
        .expect("Failed to write batch input");
    let output = child
        .wait_with_output()
        .expect("Failed to wait on cat-file");
    assert!(
        output.status.success(),
        "cat-file --batch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("{head_hash} commit ")),
        "expected '<hash> commit <size>' header, got: {stdout}"
    );
    // The commit contents follow the header line.
    assert!(
        stdout.contains("first commit"),
        "expected the commit message in the contents, got: {stdout}"
    );
    assert!(
        stdout.contains("tree "),
        "expected the commit tree line in the contents, got: {stdout}"
    );
    assert!(
        stdout.contains("not-a-real-ref missing"),
        "expected the missing line, got: {stdout}"
    );
}

/// Scenario: `cat-file --batch-check="%(objecttype) %(objectsize)"` must
/// expand the format atoms instead of the default `<sha> <type> <size>` line.
#[tokio::test]
async fn test_cat_file_batch_check_custom_format_expands_atoms() {
    use std::process::Stdio;

    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);
    create_commit(temp_path, "hello.txt", "hello world\n", "first commit");

    let head = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("Failed to resolve HEAD");
    let head_hash = String::from_utf8_lossy(&head.stdout).trim().to_string();

    let mut child = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "--batch-check=%(objecttype) %(objectsize)"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn cat-file --batch-check with format");

    {
        let mut stdin = child.stdin.take().expect("Failed to open stdin");
        stdin
            .write_all(format!("{head_hash}\n").as_bytes())
            .expect("Failed to write batch-check input");
    }

    let output = child
        .wait_with_output()
        .expect("Failed to wait for cat-file --batch-check with format");

    assert!(
        output.status.success(),
        "cat-file --batch-check with format failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("commit "),
        "expected format-expanded line starting with 'commit', got: {stdout}"
    );
}

/// `command="cat-file"`, `data.mode="type"`, `data.object="HEAD"` and
/// `data.object_type="commit"`. Schema pin for the type-mode envelope.
#[tokio::test]
async fn test_cat_file_type_json_output() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);
    create_commit(temp_path, "hello.txt", "hello world\n", "first commit");

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-t", "HEAD", "--json"])
        .output()
        .expect("Failed to execute cat-file");

    assert!(
        output.status.success(),
        "cat-file -t --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "cat-file");
    assert_eq!(json["data"]["mode"], "type");
    assert_eq!(json["data"]["object"], "HEAD");
    assert_eq!(json["data"]["object_type"], "commit");
}

/// Scenario: `cat-file -p HEAD` on a commit must include `tree `,
/// `author `, `committer `, and the commit message. Locks the
/// commit-pretty-printer's stable headers so other tests can grep for
/// them (e.g. tree-hash extraction in subsequent cases).
#[tokio::test]
async fn test_cat_file_pretty_commit() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);
    create_commit(temp_path, "hello.txt", "hello world\n", "first commit");

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-p", "HEAD"])
        .output()
        .expect("Failed to execute cat-file");

    assert!(
        output.status.success(),
        "cat-file -p failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("tree "),
        "Commit pretty-print should contain 'tree': {}",
        stdout
    );
    assert!(
        stdout.contains("author "),
        "Commit pretty-print should contain 'author': {}",
        stdout
    );
    assert!(
        stdout.contains("committer "),
        "Commit pretty-print should contain 'committer': {}",
        stdout
    );
    assert!(
        stdout.contains("first commit"),
        "Commit pretty-print should contain message: {}",
        stdout
    );
}

/// Scenario: end-to-end commit → tree path. Extracts the tree hash from
/// the commit's pretty output, then verifies `cat-file -p <tree>` lists
/// the blob entry (`blob` + filename) and `cat-file -t <tree>` returns
/// `tree`. Pins both the tree-pretty format and tree type tagging.
#[tokio::test]
async fn test_cat_file_pretty_tree() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);
    create_commit(temp_path, "file.txt", "content\n", "add file");

    // Get the tree hash from the commit
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-p", "HEAD"])
        .output()
        .expect("Failed to execute cat-file");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let tree_hash = stdout
        .lines()
        .find(|l| l.starts_with("tree "))
        .expect("should have tree line")
        .strip_prefix("tree ")
        .unwrap()
        .trim();

    // Now cat-file -p the tree
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-p", tree_hash])
        .output()
        .expect("Failed to execute cat-file on tree");
    assert!(
        output.status.success(),
        "cat-file -p tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("blob"),
        "Tree pretty-print should contain 'blob': {}",
        stdout
    );
    assert!(
        stdout.contains("file.txt"),
        "Tree pretty-print should contain 'file.txt': {}",
        stdout
    );

    // cat-file -t the tree should return "tree"
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-t", tree_hash])
        .output()
        .expect("Failed to execute cat-file -t on tree");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "tree");
}

/// Scenario: end-to-end commit → tree → blob. Resolves the blob hash by
/// parsing the tree entry line, then asserts:
/// - `cat-file -p <blob>` echoes the original file content verbatim,
/// - `cat-file -t <blob>` returns `blob`,
/// - `cat-file -s <blob>` returns `14` (matching `"Hello, Libra!\n"`).
/// Pins type/size/content invariants for blob objects.
#[tokio::test]
async fn test_cat_file_pretty_blob() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);
    create_commit(temp_path, "readme.txt", "Hello, Libra!\n", "init readme");

    // Get tree hash, then blob hash from tree
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-p", "HEAD"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let tree_hash = stdout
        .lines()
        .find(|l| l.starts_with("tree "))
        .unwrap()
        .strip_prefix("tree ")
        .unwrap()
        .trim();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-p", tree_hash])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // tree line format: "100644 blob <hash>\t<name>"
    let blob_line = stdout
        .lines()
        .find(|l| l.contains("readme.txt"))
        .expect("should find readme.txt in tree");
    let blob_hash = blob_line
        .split_whitespace()
        .nth(2)
        .unwrap()
        // remove the tab and filename suffix: the hash may be followed by \t
        .split('\t')
        .next()
        .unwrap();

    // cat-file -p the blob
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-p", blob_hash])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "cat-file -p blob failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "Hello, Libra!\n", "Blob content should match");

    // cat-file -t the blob should return "blob"
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-t", blob_hash])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "blob");

    // cat-file -s the blob should be 14 bytes ("Hello, Libra!\n" = 14)
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-s", blob_hash])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "14");
}

/// Scenario: a syntactically-valid but unknown 40-zero hash must surface
/// a structured `LBR-CLI-003` error (exit 129, `fatal:` on stderr). The
/// command must NOT panic when an object is missing — regression guard
/// against unwrap-on-load bugs.
#[tokio::test]
async fn test_cat_file_panic_handling() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    // Test that the command reports a structured invalid-target error rather than panicking
    // when accessing a non-existent object in a valid repository.
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-p", "0000000000000000000000000000000000000000"])
        .output()
        .expect("Failed to execute cat-file");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(129));
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(stderr.contains("fatal:"));
}

#[tokio::test]
async fn test_cat_file_json_invalid_object_returns_cli_003() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args([
            "cat-file",
            "-p",
            "0000000000000000000000000000000000000000",
            "--json",
        ])
        .output()
        .expect("Failed to execute cat-file");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(129));
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
}

/// Scenario: `cat-file -e <object>` must be silent in both directions —
/// existing object → exit 0 with empty stderr; missing object → exit 1
/// with empty stderr. Pins Git-compatible status-only semantics so
/// scripts can `if libra cat-file -e $hash; then ...`.
#[tokio::test]
async fn test_cat_file_exist_check() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);
    create_commit(temp_path, "f.txt", "data", "commit");

    // HEAD exists
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .env(
            "LIBRA_CONFIG_GLOBAL_DB",
            temp_path.join(".libra-test-global-config.db"),
        )
        .args(["cat-file", "-e", "HEAD"])
        .output()
        .expect("Failed to execute cat-file -e");
    assert!(
        output.status.success(),
        "cat-file -e HEAD should succeed for existing object"
    );
    assert!(
        output.stderr.is_empty(),
        "cat-file -e HEAD should not print stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Non-existent hash
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .env(
            "LIBRA_CONFIG_GLOBAL_DB",
            temp_path.join(".libra-test-global-config.db"),
        )
        .args(["cat-file", "-e", "0000000000000000000000000000000000000000"])
        .output()
        .expect("Failed to execute cat-file -e");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stderr.is_empty(),
        "cat-file -e missing object should stay silent: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Scenario: `cat-file -e --json`/`--machine` emits a `{ exists: bool }`
/// envelope for agents while preserving the status-only exit contract — a
/// present object exits 0, a well-formed but absent object exits 1 (with the
/// JSON still written to stdout).
#[tokio::test]
async fn test_cat_file_exist_check_json() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);
    create_commit(temp_path, "f.txt", "data", "commit");

    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(temp_path)
            .env(
                "LIBRA_CONFIG_GLOBAL_DB",
                temp_path.join(".libra-test-global-config.db"),
            )
            .args(args)
            .output()
            .expect("Failed to execute cat-file")
    };

    // Present object → exists:true, exit 0.
    let out = run(&["cat-file", "-e", "HEAD", "--json"]);
    assert!(
        out.status.success(),
        "cat-file -e HEAD --json should exit 0"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"exists\"") && stdout.contains("true"),
        "expected exists:true JSON: {stdout}"
    );

    // Well-formed but absent object → exists:false, exit 1, JSON still emitted.
    let out = run(&[
        "cat-file",
        "-e",
        "0000000000000000000000000000000000000000",
        "--json",
    ]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "absent object exits 1 even with --json"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"exists\"") && stdout.contains("false"),
        "expected exists:false JSON: {stdout}"
    );

    // A malformed/unresolvable name is a hard error (LBR-CLI-003 / exit 129,
    // the same as the non-JSON path) and emits no `exists` envelope.
    let out = run(&["cat-file", "-e", "not a valid ref!!", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(129),
        "malformed name should exit 129 (LBR-CLI-003)"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("\"exists\""),
        "malformed name must not emit an exists envelope: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// Scenario: `-t -s` together must be rejected — clap's mutual-exclusion
/// guards prevent ambiguous output. Confirms the CLI grammar.
#[tokio::test]
async fn test_cat_file_mutual_exclusion() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-t", "-s", "HEAD"])
        .output()
        .expect("Failed to execute cat-file");

    assert!(
        !output.status.success(),
        "cat-file -t -s should fail (mutual exclusion)"
    );
}

/// Test `cat-file -p` with multiple files in a tree.
#[tokio::test]
async fn test_cat_file_tree_multiple_files() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);

    // Create multiple files
    std::fs::write(temp_path.join("a.txt"), "aaa\n").unwrap();
    std::fs::write(temp_path.join("b.txt"), "bbb\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["add", "."])
        .output()
        .unwrap();
    assert!(output.status.success());

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["commit", "-m", "two files", "--no-verify"])
        .output()
        .unwrap();
    assert!(output.status.success());

    // Get tree hash
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-p", "HEAD"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let tree_hash = stdout
        .lines()
        .find(|l| l.starts_with("tree "))
        .unwrap()
        .strip_prefix("tree ")
        .unwrap()
        .trim();

    // Pretty-print tree
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-p", tree_hash])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("a.txt"), "Should list a.txt: {}", stdout);
    assert!(stdout.contains("b.txt"), "Should list b.txt: {}", stdout);
}

/// Pipe raw `stdin` bytes into `libra <args>` and collect the output.
fn run_libra_with_stdin_bytes(
    temp_path: &std::path::Path,
    args: &[&str],
    stdin: &[u8],
) -> std::process::Output {
    use std::process::Stdio;

    let mut child = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn libra");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(stdin)
        .expect("Failed to write libra stdin");
    child.wait_with_output().expect("Failed to wait on libra")
}

/// Store `body` as a raw object of type `kind` via `hash-object -w --stdin`
/// and return its object id.
fn write_raw_object(temp_path: &std::path::Path, kind: &str, body: &[u8]) -> String {
    let output = run_libra_with_stdin_bytes(
        temp_path,
        &["hash-object", "-t", kind, "-w", "--stdin"],
        body,
    );
    assert!(
        output.status.success(),
        "hash-object -t {kind} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Scenario: `cat-file -p <tree>` renders every entry kind with Git's
/// zero-padded octal mode and the type of the object it points to: regular
/// file, symlink, executable, subtree, and gitlink (submodule, typed
/// `commit`), in both human and JSON output. Regression: modes were printed
/// from the enum discriminant (`000000`, `000004`, `000001`, …) and gitlinks
/// were typed `blob`.
#[test]
fn test_cat_file_pretty_tree_prints_git_modes_and_types() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    const BLOB: &str = "ce013625030ba8dba906f756967f9e9ca394464a";
    const SUBTREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    const GITLINK: &str = "0123456789abcdef0123456789abcdef01234567";
    let entries = [
        ("100644", "blob", BLOB, "a.txt"),
        ("120000", "blob", BLOB, "b-link"),
        ("100755", "blob", BLOB, "c.sh"),
        ("040000", "tree", SUBTREE, "d-dir"),
        ("160000", "commit", GITLINK, "e-module"),
    ];
    let mut raw_tree = Vec::new();
    for (mode, _, id, name) in entries {
        // Raw tree entries store the directory mode without its leading zero.
        raw_tree.extend_from_slice(mode.trim_start_matches('0').as_bytes());
        raw_tree.push(b' ');
        raw_tree.extend_from_slice(name.as_bytes());
        raw_tree.push(0);
        raw_tree.extend_from_slice(&hex::decode(id).expect("valid hex object id"));
    }
    let tree = write_raw_object(temp_path, "tree", &raw_tree);

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-p", &tree])
        .output()
        .expect("Failed to execute cat-file -p");
    assert!(
        output.status.success(),
        "cat-file -p tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: String = entries
        .iter()
        .map(|(mode, kind, id, name)| format!("{mode} {kind} {id}\t{name}\n"))
        .collect();
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["--json", "cat-file", "-p", &tree])
        .output()
        .expect("Failed to execute cat-file -p --json");
    assert!(
        output.status.success(),
        "cat-file -p tree --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = parse_json_stdout(&output);
    let json_entries = json["data"]["entries"]
        .as_array()
        .expect("tree JSON must carry entries");
    assert_eq!(json_entries.len(), entries.len());
    for (entry, (mode, kind, id, name)) in json_entries.iter().zip(entries) {
        assert_eq!(entry["mode"], mode, "{entry}");
        assert_eq!(entry["object_type"], kind, "{entry}");
        assert_eq!(entry["hash"], id, "{entry}");
        assert_eq!(entry["name"], name, "{entry}");
    }
}

/// Scenario: `cat-file -p <commit>` prints the stored object byte-for-byte,
/// like Git: signature and other extra headers stay in the header block and
/// the message keeps its exact whitespace, with no newline appended. The JSON
/// `message` carries only the (trimmed) message, never header lines.
/// Regression: the commit was re-rendered from parsed fields, dropping
/// `gpgsig`, moving extra headers below the blank line, and trimming the
/// message.
#[test]
fn test_cat_file_pretty_commit_prints_raw_object() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    let raw_commit = [
        "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904",
        "author Alice <alice@example.com> 1700000000 +0000",
        "committer Bob <bob@example.com> 1700000001 +0100",
        "encoding ISO-8859-1",
        "gpgsig -----BEGIN PGP SIGNATURE-----",
        " ",
        " iQEzBAABCAAdFiEE",
        " -----END PGP SIGNATURE-----",
        "change-id abc",
        "",
        "",
        "  indented subject  ",
        "",
        "body line   ",
    ]
    .join("\n");
    let commit = write_raw_object(temp_path, "commit", raw_commit.as_bytes());

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-p", &commit])
        .output()
        .expect("Failed to execute cat-file -p");
    assert!(
        output.status.success(),
        "cat-file -p commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), raw_commit);

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["--json", "cat-file", "-p", &commit])
        .output()
        .expect("Failed to execute cat-file -p --json");
    assert!(
        output.status.success(),
        "cat-file -p commit --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["object_type"], "commit");
    assert_eq!(
        json["data"]["tree"],
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    );
    assert_eq!(json["data"]["message"], "indented subject  \n\nbody line");
}

/// Test `cat-file` with a non-existent reference.
#[tokio::test]
async fn test_cat_file_nonexistent_ref() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);
    create_commit(temp_path, "f.txt", "data", "commit");

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-t", "nonexistent-branch"])
        .output()
        .expect("Failed to execute cat-file");

    assert!(
        !output.status.success(),
        "cat-file should fail for non-existent ref"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// AI object tests
// ═══════════════════════════════════════════════════════════════════════

/// Test `cat-file --ai-list-types` on a fresh repo (no AI objects yet).
#[tokio::test]
async fn test_cat_file_ai_list_types_empty() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "--ai-list-types"])
        .output()
        .expect("Failed to execute cat-file --ai-list-types");

    assert!(
        output.status.success(),
        "cat-file --ai-list-types should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Fresh repo has no AI objects, output should be empty or minimal
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Since there are no AI objects, none of the types should appear with counts
    assert!(
        !stdout.contains("(0 objects)"),
        "Should not show types with zero objects"
    );
}

/// Test `cat-file --ai-list <type>` on a fresh repo.
#[tokio::test]
async fn test_cat_file_ai_list_empty_type() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "--ai-list", "intent"])
        .output()
        .expect("Failed to execute cat-file --ai-list");

    assert!(
        output.status.success(),
        "cat-file --ai-list intent should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No intent objects found"),
        "Should report no objects: {}",
        stdout
    );
}

/// Test `cat-file --ai-list <invalid_type>` fails.
#[tokio::test]
async fn test_cat_file_ai_list_invalid_type() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "--ai-list", "foobar"])
        .output()
        .expect("Failed to execute cat-file --ai-list");

    assert!(
        !output.status.success(),
        "cat-file --ai-list foobar should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown AI object type"),
        "Should report unknown type: {}",
        stderr
    );
}

#[tokio::test]
async fn test_cat_file_ai_list_invalid_type_json_returns_cli_003() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "--ai-list", "foobar", "--json"])
        .output()
        .expect("Failed to execute cat-file");

    assert!(!output.status.success());
    let (_, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
}

#[tokio::test]
async fn test_cat_file_json_pretty_print_io_read_failed_when_object_body_corrupted() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);
    create_commit(temp_path, "hello.txt", "hello world\n", "first commit");

    let head_output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("Failed to execute rev-parse");
    assert!(
        head_output.status.success(),
        "rev-parse HEAD failed: {}",
        String::from_utf8_lossy(&head_output.stderr)
    );
    let head = String::from_utf8_lossy(&head_output.stdout)
        .trim()
        .to_string();

    let object_path = loose_object_path(temp_path, &head);
    let raw_data = std::fs::read(&object_path).expect("Failed to read commit object file");

    let mut decoder = ZlibDecoder::new(raw_data.as_slice());
    let mut decompressed = Vec::new();
    decoder
        .read_to_end(&mut decompressed)
        .expect("Failed to decode commit object payload");
    let header_end = decompressed
        .iter()
        .position(|&b| b == b'\0')
        .expect("Malformed object payload");
    let mut corrupted = Vec::with_capacity(header_end + 1 + 5);
    corrupted.extend_from_slice(&decompressed[..=header_end]);
    corrupted.extend_from_slice(b"\xff\xff");

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder
        .write_all(&corrupted)
        .expect("Failed to re-encode corrupted commit object");
    let encoded = encoder
        .finish()
        .expect("Failed to finish corrupted commit object encoding");
    std::fs::write(&object_path, encoded).expect("Failed to write corrupted commit object");

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-p", &head, "--json"])
        .output()
        .expect("Failed to execute cat-file");

    assert!(!output.status.success());
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-REPO-002");
}

/// Test `cat-file --ai <uuid>` with a non-existent UUID.
#[tokio::test]
async fn test_cat_file_ai_nonexistent_uuid() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "--ai", "00000000-0000-0000-0000-000000000000"])
        .output()
        .expect("Failed to execute cat-file --ai");

    assert!(
        !output.status.success(),
        "cat-file --ai with non-existent UUID should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("AI object not found"),
        "Should report not found: {}",
        stderr
    );
}

/// Test `cat-file --ai-type <uuid>` with a non-existent UUID.
#[tokio::test]
async fn test_cat_file_ai_type_nonexistent() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args([
            "cat-file",
            "--ai-type",
            "00000000-0000-0000-0000-000000000000",
        ])
        .output()
        .expect("Failed to execute cat-file --ai-type");

    assert!(
        !output.status.success(),
        "cat-file --ai-type with non-existent UUID should fail"
    );
}

/// Test that AI flags and Git flags are mutually exclusive.
#[tokio::test]
async fn test_cat_file_ai_git_mutual_exclusion() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "--ai-list-types", "-t", "HEAD"])
        .output()
        .expect("Failed to execute cat-file");

    assert!(
        !output.status.success(),
        "AI and Git flags should be mutually exclusive"
    );
}

/// Running `cat-file` outside a repository should return exit code 128.
#[test]
fn test_cat_file_cli_outside_repository_returns_fatal_128() {
    let temp = tempfile::tempdir().unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp.path())
        .args(["cat-file", "-t", "HEAD"])
        .output()
        .expect("Failed to execute cat-file");

    assert_eq!(output.status.code(), Some(128));
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-REPO-001");
    assert!(
        stderr.contains("fatal: not a libra repository"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn test_cat_file_batch_command_dispatches_info_and_contents() {
    use std::process::Stdio;

    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();
    configure_user_identity(temp_path);
    create_commit(temp_path, "hello.txt", "hello world\n", "first commit");

    let head = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("Failed to resolve HEAD");
    let head_hash = String::from_utf8_lossy(&head.stdout).trim().to_string();

    let run_bc = |stdin_body: String| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(temp_path)
            .args(["cat-file", "--batch-command"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Failed to spawn cat-file --batch-command");
        child
            .stdin
            .take()
            .expect("child stdin")
            .write_all(stdin_body.as_bytes())
            .expect("Failed to write batch-command input");
        child
            .wait_with_output()
            .expect("Failed to wait on cat-file")
    };

    // `info` prints the header only; `contents` adds the object body; a missing
    // object reports "<spec> missing".
    let out = run_bc(format!(
        "info {head_hash}\ncontents {head_hash}\ninfo not-a-real-ref\n"
    ));
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains(&format!("{head_hash} commit ")),
        "info/contents header: {s}"
    );
    assert!(
        s.contains("first commit") && s.contains("tree "),
        "contents body: {s}"
    );
    assert!(s.contains("not-a-real-ref missing"), "missing line: {s}");

    // `flush` is rejected without --buffer.
    let flush = run_bc("flush\n".to_string());
    assert!(!flush.status.success(), "flush must error without --buffer");

    // An unknown command is a usage error.
    let unknown = run_bc("bogus deadbeef\n".to_string());
    assert!(!unknown.status.success(), "unknown command must error");
}

#[test]
fn test_cat_file_buffer_enables_flush_and_requires_batch_mode() {
    use std::process::Stdio;

    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();
    configure_user_identity(temp_path);
    create_commit(temp_path, "hello.txt", "hello world\n", "first commit");

    let head = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("Failed to resolve HEAD");
    let head_hash = String::from_utf8_lossy(&head.stdout).trim().to_string();

    let run = |args: &[&str], stdin_body: String| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(temp_path)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Failed to spawn cat-file");
        child
            .stdin
            .take()
            .expect("child stdin")
            .write_all(stdin_body.as_bytes())
            .expect("Failed to write input");
        child
            .wait_with_output()
            .expect("Failed to wait on cat-file")
    };

    // Under --buffer the `flush` command is valid and both `info` outputs are
    // produced (one before flush, one after).
    let buffered = run(
        &["cat-file", "--batch-command", "--buffer"],
        format!("info {head_hash}\nflush\ninfo {head_hash}\n"),
    );
    assert!(
        buffered.status.success(),
        "flush is valid under --buffer: {}",
        String::from_utf8_lossy(&buffered.stderr)
    );
    let buffered_out = String::from_utf8_lossy(&buffered.stdout);
    assert_eq!(
        buffered_out
            .matches(&format!("{head_hash} commit "))
            .count(),
        2,
        "both info outputs are present: {buffered_out}"
    );

    // Buffering does not change the output bytes vs the unbuffered run of the
    // same info commands.
    let unbuffered = run(
        &["cat-file", "--batch-command"],
        format!("info {head_hash}\ninfo {head_hash}\n"),
    );
    assert_eq!(
        buffered.stdout, unbuffered.stdout,
        "buffering must not change the emitted bytes"
    );

    // A `flush` with a stray argument is a usage error even under --buffer.
    let bad_flush = run(
        &["cat-file", "--batch-command", "--buffer"],
        "flush extra\n".to_string(),
    );
    assert!(
        !bad_flush.status.success(),
        "flush takes no argument: {}",
        String::from_utf8_lossy(&bad_flush.stdout)
    );

    // --buffer without a batch mode is a usage error (exit 129).
    let bad = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "--buffer", &head_hash])
        .output()
        .expect("Failed to run cat-file --buffer");
    assert_eq!(
        bad.status.code(),
        Some(129),
        "--buffer requires a batch mode: {}",
        String::from_utf8_lossy(&bad.stderr)
    );
}

#[test]
fn test_cat_file_batch_all_objects_lists_sorted_objects() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();
    configure_user_identity(temp_path);
    create_commit(temp_path, "hello.txt", "hello world\n", "first commit");

    let head = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("Failed to resolve HEAD");
    let head_hash = String::from_utf8_lossy(&head.stdout).trim().to_string();

    let out = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "--batch-check", "--batch-all-objects"])
        .output()
        .expect("Failed to run cat-file --batch-all-objects");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();

    // The first commit produces at least a commit, a tree, and a blob.
    assert!(lines.len() >= 3, "expected >= 3 objects, got: {stdout}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with(&head_hash) && l.contains(" commit ")),
        "HEAD commit must be listed: {stdout}"
    );
    assert!(
        lines.iter().any(|l| l.contains(" tree ")),
        "a tree must be listed: {stdout}"
    );
    assert!(
        lines.iter().any(|l| l.contains(" blob ")),
        "a blob must be listed: {stdout}"
    );

    // Objects are emitted in ascending object-id order.
    let hashes: Vec<&str> = lines
        .iter()
        .map(|l| l.split(' ').next().unwrap_or(""))
        .collect();
    let mut sorted = hashes.clone();
    sorted.sort_unstable();
    assert_eq!(hashes, sorted, "objects must be sorted by id: {stdout}");

    // Without --batch / --batch-check it is a usage error.
    let bad = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "--batch-all-objects"])
        .output()
        .expect("Failed to run cat-file --batch-all-objects (no mode)");
    assert!(
        !bad.status.success(),
        "--batch-all-objects requires --batch/--batch-check"
    );
}

/// Commit one file and return `(HEAD hash, HEAD commit size)`.
fn commit_and_read_head(temp_path: &std::path::Path) -> (String, String) {
    configure_user_identity(temp_path);
    create_commit(temp_path, "hello.txt", "hello world\n", "first commit");

    let read = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_libra"))
            .current_dir(temp_path)
            .args(args)
            .output()
            .expect("Failed to run libra");
        assert!(
            output.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    (
        read(&["rev-parse", "HEAD"]),
        read(&["cat-file", "-s", "HEAD"]),
    )
}

/// Scenario: batch input is raw bytes. A line that is not valid UTF-8 is
/// answered `<input> missing` with the bytes echoed unchanged, and the lines
/// around it (including a CRLF-terminated one) are still processed.
/// Regression: stdin was decoded with `read_to_string`, so a single invalid
/// byte aborted the whole batch with a read error and no output.
#[test]
fn test_cat_file_batch_accepts_non_utf8_input() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();
    let (head_hash, head_size) = commit_and_read_head(temp_path);
    let head_line = format!("{head_hash} commit {head_size}\n");

    let check = run_libra_with_stdin_bytes(
        temp_path,
        &["cat-file", "--batch-check"],
        b"HEAD\r\n\xff\xfe-name\nnot-a-real-ref\n",
    );
    assert!(
        check.status.success(),
        "--batch-check must accept non-UTF-8 input: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let mut expected = head_line.clone().into_bytes();
    expected.extend_from_slice(b"\xff\xfe-name missing\nnot-a-real-ref missing\n");
    assert_eq!(
        check.stdout,
        expected,
        "unexpected --batch-check output: {}",
        String::from_utf8_lossy(&check.stdout)
    );

    let batch = run_libra_with_stdin_bytes(temp_path, &["cat-file", "--batch"], b"\xc3\x28\n");
    assert!(
        batch.status.success(),
        "--batch must accept non-UTF-8 input: {}",
        String::from_utf8_lossy(&batch.stderr)
    );
    assert_eq!(batch.stdout, b"\xc3\x28 missing\n");

    for args in [
        ["cat-file", "--batch-command"].as_slice(),
        ["cat-file", "--batch-command", "--buffer"].as_slice(),
    ] {
        let command = run_libra_with_stdin_bytes(temp_path, args, b"info \xff\xfe\ninfo HEAD\n");
        assert!(
            command.status.success(),
            "{args:?} must accept a non-UTF-8 argument: {}",
            String::from_utf8_lossy(&command.stderr)
        );
        let mut expected = b"\xff\xfe missing\n".to_vec();
        expected.extend_from_slice(head_line.as_bytes());
        assert_eq!(
            command.stdout,
            expected,
            "unexpected {args:?} output: {}",
            String::from_utf8_lossy(&command.stdout)
        );
    }

    // A non-UTF-8 command word is an ordinary unknown-command usage error.
    let unknown =
        run_libra_with_stdin_bytes(temp_path, &["cat-file", "--batch-command"], b"\xff HEAD\n");
    assert_eq!(
        unknown.status.code(),
        Some(129),
        "non-UTF-8 command must be a usage error: {}",
        String::from_utf8_lossy(&unknown.stderr)
    );
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("unknown command"),
        "stderr: {}",
        String::from_utf8_lossy(&unknown.stderr)
    );
}

#[test]
fn cat_file_storage_read_error_is_not_absent() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();
    let pack_dir = temp_path.join(".libra/objects/pack");
    std::fs::create_dir_all(&pack_dir).expect("pack dir");
    std::fs::write(pack_dir.join("orphan.pack"), b"not-a-pack").expect("unindexed pack");
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["cat-file", "-e", "0123456789abcdef0123456789abcdef01234567"])
        .output()
        .expect("cat-file");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let (_message, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-IO-001", "{stderr}");
    assert_ne!(output.status.code(), Some(1), "{stderr}");
    assert!(!stderr.contains("absent"), "{stderr}");
}

/// Drive `libra <args>` interactively: send each request without closing
/// stdin and wait (bounded) for a stdout line starting with the expected
/// prefix before sending the next request. A batch mode that reads stdin to
/// EOF before answering never produces those lines, so this times out.
fn assert_batch_answers_each_line(
    temp_path: &std::path::Path,
    args: &[&str],
    exchanges: &[(&str, &str)],
) {
    use std::{
        io::BufRead,
        process::Stdio,
        sync::mpsc,
        time::{Duration, Instant},
    };

    let mut child = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn libra");
    let mut stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");

    let (tx, rx) = mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        let mut stdout = std::io::BufReader::new(stdout);
        loop {
            let mut line = Vec::new();
            match stdout.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx
                        .send(String::from_utf8_lossy(&line).into_owned())
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });

    for (request, expected_prefix) in exchanges {
        stdin
            .write_all(request.as_bytes())
            .expect("Failed to write batch request");
        stdin.flush().expect("Failed to flush batch request");

        let deadline = Instant::now() + Duration::from_secs(60);
        let mut seen = Vec::new();
        loop {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(line) if line.starts_with(expected_prefix) => break,
                // `--batch` object contents follow the header line.
                Ok(line) => seen.push(line),
                Err(_) => {
                    let _ = child.kill();
                    let output = child.wait_with_output().expect("Failed to reap libra");
                    panic!(
                        "{args:?}: no response starting with {expected_prefix:?} to \
                         {request:?} while stdin stayed open (other lines: {seen:?}; \
                         stderr: {})",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
        }
    }

    drop(stdin);
    let output = child.wait_with_output().expect("Failed to wait on libra");
    let _ = reader.join();
    assert!(
        output.status.success(),
        "{args:?} failed after stdin closed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Scenario: without `--buffer`, every batch mode answers each input line
/// before the next one arrives, so a caller can keep one cat-file process
/// open and query it interactively (as `git cat-file --batch` allows).
/// Regression: stdin was read to EOF before any output was produced.
#[test]
fn test_cat_file_batch_modes_answer_each_line_before_eof() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();
    let (head_hash, _) = commit_and_read_head(temp_path);
    let head_header = format!("{head_hash} commit ");

    let lookups = [
        ("HEAD\n", head_header.as_str()),
        ("not-a-real-ref\n", "not-a-real-ref missing"),
        ("HEAD\n", head_header.as_str()),
    ];
    assert_batch_answers_each_line(temp_path, &["cat-file", "--batch-check"], &lookups);
    assert_batch_answers_each_line(temp_path, &["cat-file", "--batch"], &lookups);
    assert_batch_answers_each_line(
        temp_path,
        &["cat-file", "--batch-command"],
        &[
            ("info HEAD\n", head_header.as_str()),
            ("contents not-a-real-ref\n", "not-a-real-ref missing"),
            ("contents HEAD\n", head_header.as_str()),
        ],
    );
    // Under --buffer, queued commands are answered once `flush` arrives.
    assert_batch_answers_each_line(
        temp_path,
        &["cat-file", "--batch-command", "--buffer"],
        &[
            (
                "info not-a-real-ref\ninfo HEAD\nflush\n",
                head_header.as_str(),
            ),
            ("info HEAD\nflush\n", head_header.as_str()),
        ],
    );
}

/// PM-14: `cat-file <type> <object>` peels tags/commits and prints raw bytes.
#[test]
fn cat_file_type_object_form() {
    use std::process::Command;

    let repo = tempfile::tempdir().expect("tempdir");
    let libra = env!("CARGO_BIN_EXE_libra");
    let run = |args: &[&str]| {
        Command::new(libra)
            .args(args)
            .current_dir(repo.path())
            .env("HOME", repo.path().join(".home"))
            .env("LIBRA_TEST", "1")
            .output()
            .expect("run libra")
    };
    assert!(run(&["init", "--vault", "false"]).status.success());
    assert!(run(&["config", "user.name", "t"]).status.success());
    assert!(run(&["config", "user.email", "t@t"]).status.success());
    std::fs::write(repo.path().join("a.txt"), "hello\n").unwrap();
    assert!(run(&["add", "a.txt"]).status.success());
    assert!(run(&["commit", "-m", "c1", "--no-verify"]).status.success());
    let hello_blob = String::from_utf8_lossy(&run(&["rev-parse", "HEAD:a.txt"]).stdout)
        .trim()
        .to_string();
    assert!(run(&["tag", "-a", "v1", "-m", "tagmsg"]).status.success());
    // A second commit so HEAD is not the tagged commit.
    std::fs::write(repo.path().join("a.txt"), "first\n").unwrap();
    assert!(run(&["add", "a.txt"]).status.success());
    assert!(run(&["commit", "-m", "c2", "--no-verify"]).status.success());

    // AC1: commit bytes equal `-p`.
    let raw = run(&["cat-file", "commit", "HEAD"]);
    assert!(raw.status.success(), "AC1");
    assert_eq!(raw.stdout, run(&["cat-file", "-p", "HEAD"]).stdout, "AC1");

    // AC2: the earlier blob prints its exact bytes.
    let blob = run(&["cat-file", "blob", &hello_blob]);
    assert!(blob.status.success(), "AC2");
    assert_eq!(blob.stdout, b"hello\n", "AC2");

    // AC3: a commit peels to its raw tree bytes.
    let tree = run(&["cat-file", "tree", "HEAD"]);
    assert!(tree.status.success(), "AC3");
    assert!(tree.stdout.starts_with(b"100644 a.txt"), "AC3: {tree:?}");

    // AC4/AC7: annotated tags print raw tag bytes and peel to their commit.
    let tag = run(&["cat-file", "tag", "v1"]);
    assert!(tag.status.success(), "AC4");
    assert_eq!(tag.stdout, run(&["cat-file", "-p", "v1"]).stdout, "AC4");
    let peeled = run(&["cat-file", "commit", "v1"]);
    assert!(peeled.status.success(), "AC7");
    assert_eq!(
        peeled.stdout,
        run(&["cat-file", "-p", "v1^{commit}"]).stdout,
        "AC7"
    );

    // AC5/AC6: impossible peels and unknown types.
    let not_blob = run(&["cat-file", "blob", "HEAD"]);
    assert!(!not_blob.status.success(), "AC5 must fail");
    assert!(
        String::from_utf8_lossy(&not_blob.stderr).contains("LBR-CLI-003"),
        "AC5 code"
    );
    let bogus = run(&["cat-file", "bogus", "HEAD"]);
    assert!(!bogus.status.success(), "AC6 must fail");
    assert!(
        String::from_utf8_lossy(&bogus.stderr).contains("LBR-CLI-002"),
        "AC6 code"
    );

    // AC8: mode flags conflict with the two-positional form.
    let mixed = run(&["cat-file", "--ai-list-types", "commit", "HEAD"]);
    assert!(!mixed.status.success(), "AC8 must fail");
    assert!(
        String::from_utf8_lossy(&mixed.stderr).contains("LBR-CLI-002"),
        "AC8 code"
    );
}
