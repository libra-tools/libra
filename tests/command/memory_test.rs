use super::*;
use crate::helpers::memory_cli::{assert_success, init, run, stderr_json, stdout_json};

#[test]
fn memory_search_show_status_rebuild_empty_repository_contract() {
    let repo = tempdir().expect("create Memory CLI repository");
    init(repo.path());

    let status = run(&["--json", "memory", "status"], repo.path());
    assert_success(&status, "memory status should inspect an empty repository");
    let status_json = stdout_json(&status);
    assert_eq!(status_json["command"], "memory.status");
    assert_eq!(status_json["data"]["memory_ref"], Value::Null);
    assert_eq!(status_json["data"]["projection_state"], "empty");
    assert_eq!(status_json["data"]["fts5_enabled"], true);

    let search = run(
        &["--json", "memory", "search", "authentication retry"],
        repo.path(),
    );
    assert_success(&search, "empty Memory search should succeed");
    let search_json = stdout_json(&search);
    assert_eq!(search_json["command"], "memory.search");
    assert_eq!(search_json["data"]["items"], serde_json::json!([]));

    let dry_run = run(&["--json", "memory", "rebuild", "--dry-run"], repo.path());
    assert_success(&dry_run, "empty Memory rebuild dry-run should succeed");
    let dry_run_json = stdout_json(&dry_run);
    assert_eq!(dry_run_json["command"], "memory.rebuild");
    assert_eq!(dry_run_json["data"]["dry_run"], true);
    assert_eq!(dry_run_json["data"]["changed"], false);

    let invalid_show = run(&["--json", "memory", "show", "not-a-note-id"], repo.path());
    assert!(!invalid_show.status.success());
    let report = stderr_json(&invalid_show);
    assert_eq!(report["error_code"], "LBR-MEMORY-QUERY-INVALID");
    assert_eq!(report["category"], "cli");
}

#[test]
fn memory_help_lists_the_stable_surface_and_examples() {
    let repo = tempdir().expect("create Memory help repository");
    let output = run(&["memory", "--help"], repo.path());
    assert_success(&output, "memory help should not require a repository");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in ["search", "show", "status", "rebuild", "EXAMPLES:"] {
        assert!(
            stdout.contains(expected),
            "missing {expected} in Memory help"
        );
    }
}
