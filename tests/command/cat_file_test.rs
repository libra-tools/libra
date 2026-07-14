//! Tests for the `cat-file` command, verifying object type, size, content
//! display, existence checks, AI object inspection, and structured-error
//! envelopes.
//!
//! **Layer:** L1 — deterministic, no external dependencies.
//!
//! Fixture conventions: each test uses `init_temp_repo()` to spawn a
//! fresh `libra init` repo in a tempdir, optionally calls
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

/// Spawn `libra init` in a fresh tempdir and return the `TempDir` (kept
/// alive by the caller for RAII cleanup).
fn init_temp_repo() -> tempfile::TempDir {
    let temp_dir = tempfile::tempdir().expect("Failed to create temporary directory");
    let temp_path = temp_dir.path();

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["init"])
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
