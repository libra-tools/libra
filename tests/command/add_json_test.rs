//! Structured JSON output tests for `libra add`.
//!
//! **Layer:** L1 — deterministic, no external dependencies.
//!
//! These tests pin the JSON envelope shape (`ok`, `command`, `data { added,
//! modified, removed, refreshed, ignored, failed, dry_run }`) and the error
//! envelope (`ok=false`, `error_code`) emitted on stderr. Each test uses a
//! fresh `tempdir()` and either an empty repo or `create_committed_repo()`
//! which lays down `base.txt` plus an initial commit so subsequent staging
//! can produce `modified`/`refreshed` rows. Schema regressions here are
//! breaking changes for downstream consumers (CI parsers, AI agents).

use std::fs;

use tempfile::tempdir;

use super::{
    assert_cli_success, configure_identity_via_cli, init_repo_via_cli, mark_skip_worktree,
    run_libra_command,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse the trimmed stdout of an Output as JSON or panic with the raw bytes
/// for diagnostic purposes. Local copy that includes the failing string in the
/// panic message (the `mod.rs` version does not).
fn parse_json_stdout(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON output, got: {stdout}\nerror: {e}"))
}

/// Create a repo with identity configured and an initial commit on `base.txt`.
/// Used as a baseline for every test that needs an existing tracked file.
fn create_committed_repo() -> tempfile::TempDir {
    let repo = tempdir().expect("tempdir");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());

    fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    let out = run_libra_command(&["add", "base.txt"], repo.path());
    assert_cli_success(&out, "add base.txt");
    let out = run_libra_command(&["commit", "-m", "initial", "--no-verify"], repo.path());
    assert_cli_success(&out, "initial commit");

    repo
}

// ---------------------------------------------------------------------------
// Schema completeness
// ---------------------------------------------------------------------------

/// Scenario: `libra --json add <new>` emits the canonical envelope with
/// `ok=true`, `command="add"`, the new file in `data.added`, and every other
/// bucket (`modified`, `removed`, `refreshed`, `ignored`, `failed`) empty.
/// `dry_run` must be `false`. Pins the schema for first-time adds.
#[test]
fn json_add_new_file_returns_structured_schema() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    fs::write(repo.path().join("new_file.rs"), "fn main() {}").unwrap();

    let output = run_libra_command(&["--json", "add", "new_file.rs"], repo.path());
    assert_cli_success(&output, "json add new file");

    let parsed = parse_json_stdout(&output);
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["command"], "add");

    let data = &parsed["data"];
    // added is a string array containing the new file
    let added = data["added"].as_array().expect("added should be array");
    assert!(
        added.iter().any(|v| v.as_str() == Some("new_file.rs")),
        "added should contain new_file.rs, got: {added:?}"
    );
    // Other arrays should be empty
    assert_eq!(data["modified"].as_array().map(Vec::len), Some(0));
    assert_eq!(data["removed"].as_array().map(Vec::len), Some(0));
    assert_eq!(data["refreshed"].as_array().map(Vec::len), Some(0));
    assert_eq!(data["ignored"].as_array().map(Vec::len), Some(0));
    assert_eq!(data["failed"].as_array().map(Vec::len), Some(0));
    assert_eq!(data["dry_run"], false);
}

/// Scenario: editing an already-tracked file and re-running `add` should put
/// the path in `data.modified` (not `added`). Confirms the bucket selection
/// logic distinguishes new vs. updated content.
#[test]
fn json_add_modified_file() {
    let repo = create_committed_repo();

    fs::write(repo.path().join("base.txt"), "modified\n").unwrap();

    let output = run_libra_command(&["--json", "add", "base.txt"], repo.path());
    assert_cli_success(&output, "json add modified");

    let parsed = parse_json_stdout(&output);
    assert_eq!(parsed["ok"], true);

    let data = &parsed["data"];
    let modified = data["modified"]
        .as_array()
        .expect("modified should be array");
    assert!(
        modified.iter().any(|v| v.as_str() == Some("base.txt")),
        "modified should contain base.txt, got: {modified:?}"
    );
    assert_eq!(data["added"].as_array().map(Vec::len), Some(0));
}

// ---------------------------------------------------------------------------
// --dry-run --json
// ---------------------------------------------------------------------------

/// Scenario: `--dry-run` must report what would be staged in `data.added`,
/// set `data.dry_run=true`, and leave the index untouched (cross-checked via
/// `status --short`). Guards both the JSON shape and the side-effect-free
/// invariant.
#[test]
fn json_dry_run_does_not_modify_index() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    fs::write(repo.path().join("a.rs"), "content").unwrap();

    let output = run_libra_command(&["--json", "add", "--dry-run", "a.rs"], repo.path());
    assert_cli_success(&output, "json dry-run add");

    let parsed = parse_json_stdout(&output);
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["data"]["dry_run"], true);
    let added = parsed["data"]["added"]
        .as_array()
        .expect("added should be array");
    assert!(
        added.iter().any(|v| v.as_str() == Some("a.rs")),
        "dry-run should preview a.rs"
    );

    // Verify index was NOT modified
    let status = run_libra_command(&["status", "--short"], repo.path());
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        !stdout.contains("A  a.rs"),
        "dry-run should not stage the file: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// --refresh --json
// ---------------------------------------------------------------------------

/// Scenario: `add --refresh` only updates index stat metadata when content
/// hashes are unchanged. The JSON output must always include a `refreshed`
/// array (possibly empty depending on platform mtime granularity) and leave
/// `added`/`modified`/`removed` empty. Touches the file by rewriting the
/// same bytes after a 50 ms sleep so mtime can advance.
#[test]
fn json_refresh_returns_refreshed_array() {
    let repo = create_committed_repo();

    // Touch the file to change mtime (but not content hash)
    let path = repo.path().join("base.txt");
    // Write same content to trigger mtime change
    let content = fs::read(&path).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    fs::write(&path, &content).unwrap();

    let output = run_libra_command(&["--json", "add", "--refresh"], repo.path());
    assert_cli_success(&output, "json refresh");

    let parsed = parse_json_stdout(&output);
    assert_eq!(parsed["ok"], true);

    let data = &parsed["data"];
    // refreshed may or may not be non-empty depending on stat changes
    assert!(data["refreshed"].is_array());
    assert_eq!(data["added"].as_array().map(Vec::len), Some(0));
    assert_eq!(data["modified"].as_array().map(Vec::len), Some(0));
    assert_eq!(data["removed"].as_array().map(Vec::len), Some(0));
}

// ---------------------------------------------------------------------------
// -A --json
// ---------------------------------------------------------------------------

/// Scenario: `-A` must include both new and modified files in the
/// appropriate buckets in a single invocation. Pins the multi-bucket
/// behavior of the all-changes flag.
#[test]
fn json_add_all_includes_all_changes() {
    let repo = create_committed_repo();

    // Create new file + modify existing + delete a tracked file
    fs::write(repo.path().join("new.rs"), "new").unwrap();
    fs::write(repo.path().join("base.txt"), "changed\n").unwrap();

    let output = run_libra_command(&["--json", "add", "-A"], repo.path());
    assert_cli_success(&output, "json add -A");

    let parsed = parse_json_stdout(&output);
    assert_eq!(parsed["ok"], true);
    let data = &parsed["data"];

    let added: Vec<&str> = data["added"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let modified: Vec<&str> = data["modified"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();

    assert!(added.contains(&"new.rs"), "added: {added:?}");
    assert!(modified.contains(&"base.txt"), "modified: {modified:?}");
}

// ---------------------------------------------------------------------------
// -u --json (update tracked only, no new files)
// ---------------------------------------------------------------------------

/// Scenario: `-u` (update tracked-only) must NOT report new files in
/// `data.added`; only the modified tracked file should appear in
/// `data.modified`. Locks in the difference between `-A` and `-u`.
#[test]
fn json_add_update_excludes_new_files() {
    let repo = create_committed_repo();

    fs::write(repo.path().join("new.rs"), "new").unwrap();
    fs::write(repo.path().join("base.txt"), "changed\n").unwrap();

    let output = run_libra_command(&["--json", "add", "-u"], repo.path());
    assert_cli_success(&output, "json add -u");

    let parsed = parse_json_stdout(&output);
    let data = &parsed["data"];
    let added = data["added"].as_array().unwrap();
    assert!(
        added.is_empty(),
        "-u should not add new files, got: {added:?}"
    );
    let modified: Vec<&str> = data["modified"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(modified.contains(&"base.txt"));
}

// ---------------------------------------------------------------------------
// --force --json
// ---------------------------------------------------------------------------

/// Scenario: `-f` overrides `.libraignore` so an ignored path is staged
/// (appears in `data.added`) and the `data.ignored` bucket stays empty.
/// Locks in the override semantics.
#[test]
fn json_force_add_ignored_file() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    fs::write(repo.path().join(".libraignore"), "ignored.log\n").unwrap();
    fs::write(repo.path().join("ignored.log"), "log data").unwrap();

    let output = run_libra_command(&["--json", "add", "-f", "ignored.log"], repo.path());
    assert_cli_success(&output, "json force add");

    let parsed = parse_json_stdout(&output);
    assert_eq!(parsed["ok"], true);

    let data = &parsed["data"];
    let added: Vec<&str> = data["added"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        added.contains(&"ignored.log"),
        "force should stage ignored file: {added:?}"
    );
    assert_eq!(
        data["ignored"].as_array().map(Vec::len),
        Some(0),
        "ignored list should be empty when --force is used"
    );
}

// ---------------------------------------------------------------------------
// --force --dry-run --json
// ---------------------------------------------------------------------------

/// Scenario: combining `-f --dry-run` must preview the ignored file in
/// `data.added` with `data.dry_run=true` while leaving the index unchanged.
/// Verifies that `--force` does not bypass the `--dry-run` guard.
#[test]
fn json_force_dry_run_previews_ignored_file() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    fs::write(repo.path().join(".libraignore"), "ignored.log\n").unwrap();
    fs::write(repo.path().join("ignored.log"), "log data").unwrap();

    let output = run_libra_command(
        &["--json", "add", "-f", "--dry-run", "ignored.log"],
        repo.path(),
    );
    assert_cli_success(&output, "json force dry-run");

    let parsed = parse_json_stdout(&output);
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["data"]["dry_run"], true);

    let added: Vec<&str> = parsed["data"]["added"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(added.contains(&"ignored.log"));

    // Verify index was NOT modified
    let status = run_libra_command(&["status", "--short"], repo.path());
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(!stdout.contains("ignored.log"));
}

// ---------------------------------------------------------------------------
// ignored-only JSON: ok == false
// ---------------------------------------------------------------------------

/// Scenario: pointing `--json add` at a single ignored path must produce
/// a structured error envelope on stderr with `ok=false` and
/// `error_code="LBR-ADD-001"`. Pins the error-code contract.
#[test]
fn json_ignored_only_returns_error() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    fs::write(repo.path().join(".libraignore"), "ignored.txt\n").unwrap();
    fs::write(repo.path().join("ignored.txt"), "data").unwrap();

    let output = run_libra_command(&["--json", "add", "ignored.txt"], repo.path());
    assert!(!output.status.success());

    // Error JSON is on stderr (pretty-printed)
    let stderr = String::from_utf8_lossy(&output.stderr);
    let parsed: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("parse error JSON from stderr");
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error_code"], "LBR-ADD-001");
}

// ---------------------------------------------------------------------------
// Nothing specified JSON
// ---------------------------------------------------------------------------

/// Scenario: `--json add` with no pathspec must exit 129 (CLI usage) and
/// emit a structured error envelope with `error_code="LBR-CLI-002"`. Guards
/// the no-arg failure path.
#[test]
fn json_nothing_specified_returns_error() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["--json", "add"], repo.path());
    assert_eq!(output.status.code(), Some(129));

    let stderr = String::from_utf8_lossy(&output.stderr);
    let parsed: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("parse error JSON from stderr");
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error_code"], "LBR-CLI-002");
}

// ---------------------------------------------------------------------------
// No changes scenario
// ---------------------------------------------------------------------------

/// Scenario: re-adding an unchanged tracked file must succeed with all
/// buckets empty. Confirms that "no-op" still produces a valid envelope.
#[test]
fn json_add_no_changes() {
    let repo = create_committed_repo();

    // base.txt is already committed and unchanged
    let output = run_libra_command(&["--json", "add", "base.txt"], repo.path());
    assert_cli_success(&output, "json add no changes");

    let parsed = parse_json_stdout(&output);
    assert_eq!(parsed["ok"], true);
    let data = &parsed["data"];
    assert_eq!(data["added"].as_array().map(Vec::len), Some(0));
    assert_eq!(data["modified"].as_array().map(Vec::len), Some(0));
    assert_eq!(data["removed"].as_array().map(Vec::len), Some(0));
}

// ---------------------------------------------------------------------------
// Paths are relative, no leading /
// ---------------------------------------------------------------------------

/// Scenario: paths emitted in JSON `data.added` must be repository-relative
/// (no absolute-path leak). Cross-platform regression guard against accidental
/// absolute-path serialization.
#[test]
fn json_paths_are_relative_no_leading_slash() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(repo.path().join("src/main.rs"), "fn main() {}").unwrap();

    let output = run_libra_command(&["--json", "add", "src/main.rs"], repo.path());
    assert_cli_success(&output, "json add with subdirectory");

    let parsed = parse_json_stdout(&output);
    let added = parsed["data"]["added"].as_array().unwrap();
    for path in added {
        let s = path.as_str().unwrap();
        assert!(!s.starts_with('/'), "path should be relative: {s}");
    }
}

// ---------------------------------------------------------------------------
// --machine produces single-line JSON
// ---------------------------------------------------------------------------

/// Scenario: `--machine add` must emit exactly one non-empty stdout line
/// containing valid JSON (NDJSON-friendly). Regression guard for tooling
/// that pipes Libra into line-oriented JSON consumers.
#[test]
fn machine_add_is_single_line_json() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    fs::write(repo.path().join("file.txt"), "content").unwrap();

    let output = run_libra_command(&["--machine", "add", "file.txt"], repo.path());
    assert_cli_success(&output, "machine add");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let non_empty_lines: Vec<_> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        non_empty_lines.len(),
        1,
        "machine output should be exactly 1 line, got: {non_empty_lines:?}"
    );
    let _: serde_json::Value =
        serde_json::from_str(non_empty_lines[0]).expect("machine output should be valid JSON");
}

// ---------------------------------------------------------------------------
// Partial ignore JSON: ok == true + ignored list
// ---------------------------------------------------------------------------

/// Scenario: when `add` receives a mix of staged and ignored paths, the
/// envelope must still report `ok=true` with the staged file in `data.added`
/// and the ignored file enumerated in `data.ignored`. Pins the partial-success
/// contract: ADR-IA-02 makes the mixed ignored form exit 1 (Git parity) while
/// keeping the data envelope intact.
#[test]
fn json_partial_ignore_returns_ok_with_ignored_list() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    fs::write(repo.path().join(".libraignore"), "ignored.txt\n").unwrap();
    fs::write(repo.path().join("good.txt"), "good").unwrap();
    fs::write(repo.path().join("ignored.txt"), "ignored").unwrap();

    let output = run_libra_command(&["--json", "add", "good.txt", "ignored.txt"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(1),
        "mixed ignored add must exit 1: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed = parse_json_stdout(&output);
    assert_eq!(parsed["ok"], true);

    let data = &parsed["data"];
    let added: Vec<&str> = data["added"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(added.contains(&"good.txt"), "good.txt should be staged");

    let ignored: Vec<&str> = data["ignored"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        ignored.contains(&"ignored.txt"),
        "ignored.txt should be in ignored list"
    );
}

/// M-MISS M15: a non-existent-but-ignored pathspec lands in `data.ignored`
/// (not `data.missing`), and the report exits 1 like any mixed ignored add.
#[test]
fn json_ignore_missing_ignored_path_in_ignored_bucket() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());

    fs::write(repo.path().join(".libraignore"), "ignored-file\n").unwrap();
    fs::write(repo.path().join("track-this"), "base\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "track-this"], repo.path()),
        "stage base",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], repo.path()),
        "commit base",
    );
    fs::write(repo.path().join("track-this"), "modified\n").unwrap();

    let output = run_libra_command(
        &[
            "--json",
            "add",
            "-n",
            "--ignore-missing",
            "track-this",
            "ignored-file",
        ],
        repo.path(),
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "mixed ignored missing pathspec must exit 1: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed = parse_json_stdout(&output);
    assert_eq!(parsed["ok"], true);
    let data = &parsed["data"];

    let ignored: Vec<&str> = data["ignored"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        ignored.contains(&"ignored-file"),
        "ignored-file should be in the ignored bucket: {parsed}"
    );

    let missing: Vec<&str> = data["missing"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert!(
        !missing.contains(&"ignored-file"),
        "ignored-file must not appear in missing: {parsed}"
    );
}

// ---------------------------------------------------------------------------
// Error JSON: pathspec not matched
// ---------------------------------------------------------------------------

/// Scenario: a pathspec that matches no file must yield exit code 129 and
/// a structured error envelope with `error_code="LBR-CLI-003"` and the
/// offending path in `message`. Locks the error tag and message contract.
#[test]
fn json_pathspec_not_matched_returns_error() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["--json", "add", "nonexistent.rs"], repo.path());
    assert_eq!(output.status.code(), Some(129));

    let stderr = String::from_utf8_lossy(&output.stderr);
    let parsed: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("parse error JSON from stderr");
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error_code"], "LBR-CLI-003");
    assert!(
        parsed["message"]
            .as_str()
            .unwrap()
            .contains("nonexistent.rs")
    );
}

/// M-FAIL F3: when marker registration fails, the `--json` error envelope
/// carries `stored_objects` (payloads written) and `staged: 0` (staging
/// unchanged) alongside the stable `LBR-IO-002` code.
#[test]
fn json_add_marker_failure_details() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::write(repo.path().join("marker-failure.txt"), "content").unwrap();
    fs::write(
        repo.path().join(".libra/object-index-repair"),
        "conflicting non-directory",
    )
    .unwrap();

    let out = run_libra_command(&["--json", "add", "marker-failure.txt"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(128),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let report: serde_json::Value = serde_json::from_str(stderr.trim())
        .unwrap_or_else(|e| panic!("expected JSON error envelope, got: {stderr}\nerror: {e}"));
    assert_eq!(report["error_code"], "LBR-IO-002");
    assert_eq!(report["exit_code"], 128);
    let details = &report["details"];
    assert!(
        details["stored_objects"].as_u64().unwrap_or(0) >= 1,
        "details must report stored payloads: {report}"
    );
    assert_eq!(details["staged"].as_u64(), Some(0), "{report}");
    assert!(
        report["message"]
            .as_str()
            .unwrap()
            .contains("no paths were staged"),
        "{report}"
    );

    // F4: after removing the injection, a direct retry succeeds without
    // rewriting the payloads and stages the file.
    fs::remove_file(repo.path().join(".libra/object-index-repair")).unwrap();
    let retried = run_libra_command(&["--json", "add", "marker-failure.txt"], repo.path());
    assert_eq!(
        retried.status.code(),
        Some(0),
        "retry must succeed: {}",
        String::from_utf8_lossy(&retried.stderr)
    );
    let ok: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&retried.stdout).trim())
            .expect("retry JSON envelope");
    assert_eq!(ok["ok"], true);
    assert_eq!(ok["data"]["added"][0], "marker-failure.txt");
}

/// M-EXIT E9: `--json` with a mixed ignored pathspec keeps the data envelope
/// on stdout (no human block on stderr) and exits 1.
#[test]
fn json_mixed_ignored_emits_data_and_exits_one() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::write(repo.path().join(".libraignore"), "*.log\n").unwrap();
    fs::write(repo.path().join("other.txt"), "untracked\n").unwrap();
    fs::write(repo.path().join("top.log"), "ignored\n").unwrap();

    let out = run_libra_command(
        &["--json", "add", "-n", "other.txt", "top.log"],
        repo.path(),
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let envelope = parse_json_stdout(&out);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["command"], "add");
    let data = &envelope["data"];
    assert_eq!(data["added"][0], "other.txt");
    assert_eq!(data["ignored"][0], "top.log");
    assert!(
        String::from_utf8_lossy(&out.stderr).trim().is_empty(),
        "JSON mode must keep stderr clean"
    );
}

/// M-CHMOD R11 (plan-20260918 CH-01): `--json` carries `chmod_rejected` on the
/// envelope and exits 1 without human-readable stderr lines.
#[cfg(unix)]
#[test]
fn json_add_chmod_rejected_field_and_exit() {
    use std::os::unix::fs::symlink;

    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    fs::write(repo.path().join("reg"), "content\n").unwrap();
    symlink("target", repo.path().join("foo4")).unwrap();
    for spec in ["reg", "foo4"] {
        assert_cli_success(
            &run_libra_command(&["add", spec], repo.path()),
            "stage fixture",
        );
    }

    let output = run_libra_command(
        &["--json", "add", "--chmod=+x", "--dry-run", "reg", "foo4"],
        repo.path(),
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed = parse_json_stdout(&output);
    assert_eq!(parsed["ok"], true);
    let rejected = parsed["data"]["chmod_rejected"]
        .as_array()
        .expect("chmod_rejected array");
    assert_eq!(rejected.len(), 1, "{parsed}");
    assert_eq!(rejected[0]["path"], "foo4");
    assert_eq!(rejected[0]["flip"], "+x");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("cannot chmod"),
        "JSON mode must not print human error lines: {stderr}"
    );
}

/// SW-06 (M-ADVICE D9, plan-20260918): the JSON envelope carries
/// `sparse_paths` for a skip-worktree pathspec and exits 1 with no human
/// stderr diagnostic.
#[test]
fn json_add_sparse_paths_field() {
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    fs::write(root.join("s"), "s\n").expect("write s");
    assert_cli_success(&run_libra_command(&["add", "s"], root), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "commit",
    );
    mark_skip_worktree(root, "s");
    fs::remove_file(root.join("s")).expect("remove s");

    let output = run_libra_command(
        &["--json", "add", "--dry-run", "--ignore-missing", "s"],
        root,
    );
    assert_eq!(output.status.code(), Some(1), "exit 1");
    let parsed = parse_json_stdout(&output);
    assert_eq!(
        parsed["data"]["sparse_paths"],
        serde_json::json!(["s"]),
        "sparse_paths: {parsed}"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).trim().is_empty(),
        "JSON mode must not print the human diagnostic"
    );
}

/// WT-05 (M-ITA-ADD N8, plan-20260918): `--json add -N` reports the recorded
/// paths in `data.intent_to_add`.
#[test]
fn json_add_intent_to_add_field() {
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    fs::write(root.join("new.txt"), "content\n").expect("write");
    let output = run_libra_command(&["--json", "add", "-N", "new.txt"], root);
    assert_cli_success(&output, "json add -N");
    let parsed = parse_json_stdout(&output);
    let intent = parsed["data"]["intent_to_add"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        intent.iter().any(|entry| entry.as_str() == Some("new.txt")),
        "N8 JSON intent_to_add: {parsed}"
    );
}
