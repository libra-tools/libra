//! CLI-level tests for the `cloud` command.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use super::*;

/// Running `cloud sync` outside a repository should return exit code 128.
#[test]
fn test_cloud_cli_outside_repository_returns_fatal_128() {
    let temp = tempdir().unwrap();

    let output = run_libra_command(&["cloud", "sync"], temp.path());
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal: not a libra repository"),
        "unexpected stderr: {stderr}"
    );
}

/// `cloud status` is local-only and supports structured output without requiring
/// Cloudflare credentials.
#[test]
fn test_cloud_status_json_output_empty_repo() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["--json", "cloud", "status"], repo.path());
    assert_cli_success(&output, "cloud status --json failed");
    assert!(output.stderr.is_empty());

    let json = parse_json_stdout(&output);
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "cloud.status");
    assert!(
        json["data"]["repo_id"]
            .as_str()
            .is_some_and(|repo_id| !repo_id.is_empty()),
        "repo_id should be populated: {json}"
    );
    assert_eq!(json["data"]["total_objects"], 0);
    assert_eq!(json["data"]["synced"], 0);
    assert_eq!(json["data"]["pending"], 0);
    assert_eq!(json["data"]["synced_percent"], 0);
    assert_eq!(json["data"]["by_type"].as_array().unwrap().len(), 0);
    assert!(json["data"].get("unsynced_objects").is_none());
}

/// `--machine` emits the same status envelope as one NDJSON record.
#[test]
fn test_cloud_status_machine_output_empty_repo() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["--machine", "cloud", "status"], repo.path());
    assert_cli_success(&output, "cloud status --machine failed");
    assert!(output.stderr.is_empty());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.lines().count(), 1, "unexpected stdout: {stdout}");
    let json: serde_json::Value =
        serde_json::from_str(stdout.trim_end()).expect("machine stdout should be JSON");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "cloud.status");
    assert_eq!(json["data"]["total_objects"], 0);
}

/// `cloud sync --quiet` should not emit legacy stdout progress even when
/// preflight env validation fails.
#[test]
fn test_cloud_sync_quiet_preflight_failure_has_no_stdout_progress() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["--quiet", "cloud", "sync"], repo.path());
    assert_ne!(output.status.code(), Some(0));
    assert!(
        output.stdout.is_empty(),
        "quiet sync should not print legacy progress to stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        !human.contains("Starting cloud sync..."),
        "quiet sync must not leak legacy start message: {human}"
    );
    assert_eq!(
        report.details.get("operation"),
        Some(&serde_json::json!("sync"))
    );
    assert_eq!(
        report.details.get("component"),
        Some(&serde_json::json!("cloud"))
    );
}

/// `cloud sync --json` failure path should avoid emitting legacy human progress.
#[test]
fn test_cloud_sync_json_preflight_failure_has_no_human_progress() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["--json=compact", "cloud", "sync"], repo.path());
    assert_ne!(output.status.code(), Some(0));
    assert!(
        output.stdout.is_empty(),
        "failed json sync should not emit success envelope: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        !human.contains("Starting cloud sync..."),
        "json sync must not leak legacy start message: {human}"
    );
    assert_eq!(
        report.details.get("operation"),
        Some(&serde_json::json!("sync"))
    );
    assert_eq!(
        report.details.get("component"),
        Some(&serde_json::json!("cloud"))
    );
}

/// `cloud sync --json --progress=json` should emit NDJSON progress events on
/// stderr without leaking legacy human progress lines.
#[test]
fn test_cloud_sync_json_progress_emits_start_event() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(
        &["--json=compact", "--progress=json", "cloud", "sync"],
        repo.path(),
    );
    assert_ne!(output.status.code(), Some(0));
    assert!(
        output.stdout.is_empty(),
        "failed json sync should not emit success envelope: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("\"event\":\"cloud_sync.start\""),
        "expected cloud sync progress start event in stderr: {stderr}"
    );
    assert!(
        !stderr.contains("Starting cloud sync..."),
        "json progress mode must not leak legacy start message: {stderr}"
    );
}

/// `cloud sync --progress=json` in human mode should switch from legacy stdout
/// progress lines to structured stderr events.
#[test]
fn test_cloud_sync_human_progress_json_emits_event_without_stdout_progress() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["--progress=json", "cloud", "sync"], repo.path());
    assert_ne!(output.status.code(), Some(0));
    assert!(
        output.stdout.is_empty(),
        "progress=json should suppress legacy stdout progress: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("\"event\":\"cloud_sync.start\""),
        "expected cloud sync progress start event in stderr: {stderr}"
    );
    assert!(
        !stderr.contains("Starting cloud sync..."),
        "progress=json must not leak legacy start message: {stderr}"
    );
}

/// A zero batch cannot enter the object-sync loop or mark objects as synced.
/// The CLI helper clears cloud credentials, so this also pins validation ahead
/// of any remote preflight or D1/R2 side effect.
#[test]
fn test_cloud_sync_zero_batch_rejects_before_index_or_remote_write() {
    let repo = create_committed_repo_via_cli();
    let status_before = run_libra_command(&["--json", "cloud", "status"], repo.path());
    assert_cli_success(&status_before, "cloud status before rejected sync failed");
    let status_before = parse_json_stdout(&status_before);
    assert!(
        status_before["data"]["pending"]
            .as_u64()
            .is_some_and(|n| n > 0),
        "committed fixture should contain unsynced objects: {status_before}"
    );

    let sync = run_libra_command(
        &["--json=compact", "cloud", "sync", "--batch-size", "0"],
        repo.path(),
    );
    assert_ne!(sync.status.code(), Some(0));
    assert!(sync.stdout.is_empty(), "failed sync emitted success output");
    let (human, report) = parse_cli_error_stderr(&sync.stderr);
    assert!(
        !human.contains("Starting cloud sync..."),
        "rejected sync should not emit start progress: {human}"
    );
    assert!(
        report.message.contains("Batch size must be at least 1"),
        "unexpected error: {}",
        report.message
    );
    assert_eq!(
        report.details.get("operation"),
        Some(&serde_json::json!("sync"))
    );

    let status_after = run_libra_command(&["--json", "cloud", "status"], repo.path());
    assert_cli_success(&status_after, "cloud status after rejected sync failed");
    let status_after = parse_json_stdout(&status_after);
    assert_eq!(
        status_after["data"]["repo_id"],
        status_before["data"]["repo_id"]
    );
    assert_eq!(status_before["data"]["synced"], 0);
    assert_eq!(
        status_after["data"]["synced"], 0,
        "rejected sync must not mark any local object as uploaded"
    );
    // Running CLI commands may capture additional pending Agent objects.
    assert!(
        status_after["data"]["pending"]
            .as_u64()
            .expect("pending count after rejected sync")
            >= status_before["data"]["pending"]
                .as_u64()
                .expect("pending count before rejected sync")
    );
}

/// `libra cloud --help` surfaces the EXAMPLES banner so users see the
/// canonical invocation per sub-command (`status`, `sync`, `restore`)
/// plus force-sync and JSON variants without reading the design doc.
/// Cross-cutting `--help` EXAMPLES rollout per
/// `docs/development/commands/_general.md` item B.
#[test]
fn test_cloud_help_lists_examples_banner() {
    let repo = tempdir().expect("tempdir for cloud --help");
    let output = run_libra_command(&["cloud", "--help"], repo.path());
    assert!(
        output.status.success(),
        "cloud --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "cloud --help should include EXAMPLES banner, stdout: {stdout}"
    );
    for invocation in [
        "libra cloud status",
        "libra cloud sync",
        "libra cloud sync --force",
        "libra cloud restore --name my-project",
        "libra cloud restore --repo-id",
        "libra cloud --json sync",
        "libra cloud sync --progress=json",
    ] {
        assert!(
            stdout.contains(invocation),
            "cloud --help should include `{invocation}`, stdout: {stdout}"
        );
    }
}
