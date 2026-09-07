//! JSON schema stability tests for `libra switch`.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use super::*;

#[test]
fn json_switch_existing_branch() {
    let repo = create_committed_repo_via_cli();
    // Create and switch to feature, then switch back to main
    let _ = run_libra_command(&["switch", "-c", "feature"], repo.path());
    let output = run_libra_command(&["--json", "switch", "main"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = parse_json_stdout(&output);
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "switch");
    assert_eq!(json["data"]["branch"], "main");
    assert_eq!(json["data"]["created"], false);
    assert_eq!(json["data"]["detached"], false);
    assert_eq!(json["data"]["already_on"], false);
    assert!(json["data"]["tracking"].is_null());
    assert!(json["data"]["commit"].is_string());
    assert!(json["data"]["previous_branch"].is_string());
    assert!(json["data"]["previous_commit"].is_string());
}

#[test]
fn json_switch_create_branch() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["--json", "switch", "-c", "new-feature"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = parse_json_stdout(&output);
    assert_eq!(json["ok"], true);
    assert_eq!(json["data"]["branch"], "new-feature");
    assert_eq!(json["data"]["created"], true);
    assert_eq!(json["data"]["detached"], false);
    assert_eq!(json["data"]["already_on"], false);
}

#[test]
fn json_switch_detach() {
    let repo = create_committed_repo_via_cli();
    // Get the current commit hash
    let log_output = run_libra_command(&["log", "--oneline", "-1"], repo.path());
    let commit_line = String::from_utf8_lossy(&log_output.stdout);
    let short_hash = commit_line.split_whitespace().next().unwrap_or("HEAD");

    let output = run_libra_command(&["--json", "switch", "--detach", short_hash], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = parse_json_stdout(&output);
    assert_eq!(json["ok"], true);
    assert!(json["data"]["branch"].is_null());
    assert_eq!(json["data"]["detached"], true);
    assert_eq!(json["data"]["created"], false);
}

#[test]
fn json_switch_already_on() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["--json", "switch", "main"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json = parse_json_stdout(&output);
    assert_eq!(json["ok"], true);
    assert_eq!(json["data"]["already_on"], true);
    assert_eq!(json["data"]["branch"], "main");
}

#[test]
fn json_switch_unborn_current_branch_reports_error() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["--json", "switch", "main"], repo.path());
    assert_eq!(output.status.code(), Some(129));
    assert!(
        output.stdout.is_empty(),
        "json errors should keep stdout empty, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let parsed: serde_json::Value = serde_json::from_str(stderr.trim())
        .unwrap_or_else(|e| panic!("expected JSON on stderr, got: {stderr}\nerror: {e}"));
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["message"], "branch 'main' not found");
    assert!(
        parsed["hints"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .any(|hint| hint
                .as_str()
                .unwrap_or_default()
                .contains("libra switch -c main")),
        "expected create hint in JSON error, got: {parsed}"
    );
}

#[test]
fn json_error_branch_not_found() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["--json", "switch", "nonexistent"], repo.path());
    assert!(!output.status.success());
    let _stderr = String::from_utf8_lossy(&output.stderr);
    // In JSON mode, the error should be on stderr as JSON
    // Check that either stderr contains JSON error or the process exited with proper code
    assert_eq!(output.status.code(), Some(129));
}

#[test]
fn machine_switch_single_line() {
    let repo = create_committed_repo_via_cli();
    let _ = run_libra_command(&["switch", "-c", "feature"], repo.path());
    let output = run_libra_command(&["--machine", "switch", "main"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let non_empty_lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        non_empty_lines.len(),
        1,
        "machine mode should output exactly 1 non-empty line, got: {:?}",
        non_empty_lines
    );
    // Verify it's valid JSON
    let _: serde_json::Value =
        serde_json::from_str(non_empty_lines[0]).expect("machine output should be valid JSON");
}

#[test]
fn json_schema_has_all_fields() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["--json", "switch", "-c", "test-schema"], repo.path());
    assert!(output.status.success());
    let json = parse_json_stdout(&output);
    let data = &json["data"];

    // Verify all expected fields exist
    assert!(
        data.get("previous_branch").is_some(),
        "missing previous_branch"
    );
    assert!(
        data.get("previous_commit").is_some(),
        "missing previous_commit"
    );
    assert!(data.get("branch").is_some(), "missing branch");
    assert!(data.get("commit").is_some(), "missing commit");
    assert!(data.get("created").is_some(), "missing created");
    assert!(data.get("detached").is_some(), "missing detached");
    assert!(data.get("already_on").is_some(), "missing already_on");
    assert!(data.get("tracking").is_some(), "missing tracking");
}

#[tokio::test]
#[serial(cwd)]
async fn json_switch_track_has_tracking_fields() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    let head = Head::current_commit().await.unwrap();
    let output = run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );
    assert_cli_success(&output, "add origin remote for track test");

    Branch::update_branch(
        "refs/remotes/origin/feature",
        &head.to_string(),
        Some("origin"),
    )
    .await
    .unwrap();

    let output = run_libra_command(
        &["--json", "switch", "--track", "origin/feature"],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "json success should keep stderr clean, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["ok"], true);
    assert_eq!(json["data"]["branch"], "feature");
    assert_eq!(json["data"]["created"], true);
    assert_eq!(json["data"]["detached"], false);
    assert_eq!(json["data"]["already_on"], false);
    assert_eq!(json["data"]["tracking"]["remote"], "origin");
    assert_eq!(json["data"]["tracking"]["remote_branch"], "feature");
}

#[tokio::test]
#[serial(cwd)]
async fn json_switch_guess_has_tracking_fields_and_clean_stderr() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    let head = Head::current_commit().await.unwrap();
    let output = run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );
    assert_cli_success(&output, "add origin remote for guess json test");

    Branch::update_branch(
        "refs/remotes/origin/guessed",
        &head.to_string(),
        Some("origin"),
    )
    .await
    .unwrap();

    // No --track / --guess: DWIM guess is on by default and produces the same
    // tracking envelope as --track.
    let output = run_libra_command(&["--json", "switch", "guessed"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "json success should keep stderr clean, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["ok"], true);
    assert_eq!(json["data"]["branch"], "guessed");
    assert_eq!(json["data"]["created"], true);
    assert_eq!(json["data"]["detached"], false);
    assert_eq!(json["data"]["already_on"], false);
    assert_eq!(json["data"]["tracking"]["remote"], "origin");
    assert_eq!(json["data"]["tracking"]["remote_branch"], "guessed");
}

#[test]
fn json_error_branch_not_found_reports_code_and_hints() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["--json", "switch", "nonexistent"], repo.path());
    assert_eq!(output.status.code(), Some(129));
    assert!(
        output.stdout.is_empty(),
        "json errors should keep stdout empty, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let parsed: serde_json::Value = serde_json::from_str(stderr.trim())
        .unwrap_or_else(|e| panic!("expected JSON on stderr, got: {stderr}\nerror: {e}"));
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error_code"], "LBR-CLI-003");
    assert_eq!(parsed["message"], "branch 'nonexistent' not found");
    assert!(
        parsed["hints"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .any(|hint| hint
                .as_str()
                .unwrap_or_default()
                .contains("libra switch -c nonexistent")),
        "expected create hint in JSON error, got: {parsed}"
    );
}
