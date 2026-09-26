//! CLI error code validation for push command error paths.
//!
//! **Layer:** L1 — all tests are in-process, no network required.

use std::{fs, path::Path};

use super::{
    assert_cli_success, create_committed_repo_via_cli, parse_cli_error_stderr, run_libra_command,
};

// ---------------------------------------------------------------------------
// DetachedHead → LBR-REPO-003 / exit 128
// ---------------------------------------------------------------------------

#[test]
fn test_push_detached_head_returns_repo_state_invalid() {
    let repo = create_committed_repo_via_cli();

    // Get full commit hash from log
    let log_out = run_libra_command(&["log"], repo.path());
    let stdout = String::from_utf8_lossy(&log_out.stdout);
    let hash = stdout
        .lines()
        .find(|l| l.starts_with("commit "))
        .and_then(|l| l.strip_prefix("commit "))
        .map(|h| h.trim())
        .expect("expected commit hash in log output");

    // Detach HEAD using switch --detach
    let switch_out = run_libra_command(&["switch", "--detach", hash], repo.path());
    assert!(
        switch_out.status.success(),
        "switch --detach failed: {}",
        String::from_utf8_lossy(&switch_out.stderr)
    );

    // Add remote so we don't hit NoRemoteConfigured first
    let _ = run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );

    let output = run_libra_command(&["push"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-003");
    assert!(stderr.contains("HEAD is detached"));
}

// ---------------------------------------------------------------------------
// NoRemoteConfigured → LBR-REPO-003 / exit 128
// ---------------------------------------------------------------------------

#[test]
fn test_push_no_remote_returns_repo_state_invalid() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["push"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-003");
    assert!(
        stderr.contains("no configured push destination"),
        "stderr: {stderr}"
    );
    assert!(
        report.hints.iter().any(|h| h.contains("libra remote add")),
        "should hint about adding a remote"
    );
}

// ---------------------------------------------------------------------------
// RemoteNotFound → LBR-CLI-003 / exit 129
// ---------------------------------------------------------------------------

#[test]
fn test_push_remote_not_found_returns_cli_invalid_target() {
    let repo = create_committed_repo_via_cli();

    // Add a remote named "origin" so fuzzy match can be tested
    let _ = run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );

    // Push to a non-existent remote "upstream"
    let output = run_libra_command(&["push", "upstream", "main"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        stderr.contains("not found"),
        "stderr should mention remote not found: {stderr}"
    );
}

#[test]
fn test_push_remote_not_found_with_fuzzy_suggestion() {
    let repo = create_committed_repo_via_cli();

    // Add a remote named "origin"
    let _ = run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );

    // Push to "origni" (typo of "origin", edit distance 2)
    let output = run_libra_command(&["push", "origni", "main"], repo.path());
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        report
            .hints
            .iter()
            .any(|h| h.contains("did you mean") && h.contains("origin")),
        "should suggest 'origin' as fuzzy match, hints: {:?}",
        report.hints
    );
}

// ---------------------------------------------------------------------------
// InvalidRefspec → LBR-CLI-002 / exit 129
// ---------------------------------------------------------------------------

#[test]
fn test_push_invalid_refspec_returns_cli_invalid_arguments() {
    let repo = create_committed_repo_via_cli();

    let _ = run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );

    let output = run_libra_command(&["push", "origin", "main:"], repo.path());
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-002");
}

// ---------------------------------------------------------------------------
// SourceRefNotFound → LBR-CLI-003 / exit 129
// ---------------------------------------------------------------------------

#[test]
fn test_push_source_ref_not_found_returns_cli_invalid_target() {
    let repo = create_committed_repo_via_cli();

    let _ = run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );

    let output = run_libra_command(&["push", "origin", "nonexistent-branch"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        stderr.contains("source ref") && stderr.contains("not found"),
        "stderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// UnsupportedLocalFileRemote → LBR-CLI-003 / exit 129
// ---------------------------------------------------------------------------

#[test]
fn test_push_local_file_remote_returns_cli_invalid_target() {
    let repo = create_committed_repo_via_cli();
    let remote_dir = tempfile::tempdir().unwrap();
    // HP-07 supports pushing to a local Libra target; initialize one.
    assert_cli_success(
        &run_libra_command(&["init", "--bare"], remote_dir.path()),
        "init bare remote dir",
    );
    let _ = run_libra_command(
        &[
            "remote",
            "add",
            "origin",
            remote_dir.path().to_str().unwrap(),
        ],
        repo.path(),
    );
    let current = run_libra_command(&["branch", "--show-current"], repo.path());
    assert_cli_success(&current, "show current branch");
    let branch = String::from_utf8_lossy(&current.stdout).trim().to_string();

    let output = run_libra_command(&["push", "origin", branch.as_str()], repo.path());
    assert_cli_success(&output, "push to local Libra remote");
}

fn setup_local_upstream_current_branch() -> (tempfile::TempDir, String) {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let current = run_libra_command(&["branch", "--show-current"], p);
    assert_cli_success(&current, "show current branch");
    let base = String::from_utf8_lossy(&current.stdout).trim().to_string();
    assert_cli_success(&run_libra_command(&["branch", "alpha"], p), "create alpha");
    assert_cli_success(&run_libra_command(&["switch", "alpha"], p), "switch alpha");
    assert_cli_success(
        &run_libra_command(&["branch", "-u", &base], p),
        "set local upstream",
    );
    (repo, "alpha".to_string())
}

fn branch_config_snapshot(repo: &Path) -> String {
    let output = run_libra_command(&["config", "--get-regexp", r"^branch\."], repo);
    let mut lines: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect();
    lines.sort();
    lines.join("\n")
}

fn refs_snapshot(repo: &Path) -> String {
    let output = run_libra_command(&["show-ref"], repo);
    let mut lines: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(ToString::to_string)
        .collect();
    lines.sort();
    lines.join("\n")
}

fn fetch_head_snapshot(repo: &Path) -> String {
    fs::read_to_string(repo.join(".libra/FETCH_HEAD")).unwrap_or_default()
}

fn assert_local_upstream_network_refusal(cmd: &[&str], verb: &str, branch: &str, repo: &Path) {
    let refs_before = refs_snapshot(repo);
    let cfg_before = branch_config_snapshot(repo);
    let fetch_before = fetch_head_snapshot(repo);

    let output = run_libra_command(cmd, repo);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(129), "{stderr}");
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        stderr.contains(&format!("cannot {verb}")) && stderr.contains("local upstream"),
        "human stderr should name the local-upstream refusal: {stderr}"
    );
    assert!(
        stderr.contains(&format!("branch '{branch}'"))
            && stderr.contains("issues/480")
            && stderr.contains("HP-16"),
        "human stderr should name the branch and HP-16: {stderr}"
    );

    let mut json_cmd = vec!["--json"];
    json_cmd.extend_from_slice(cmd);
    let json_out = run_libra_command(&json_cmd, repo);
    let (_json_human, json_report) = parse_cli_error_stderr(&json_out.stderr);
    assert_eq!(json_out.status.code(), Some(129));
    assert_eq!(json_report.error_code, "LBR-CLI-003");
    assert!(
        json_report.message.contains("local upstream")
            && json_report.message.contains("issues/480 HP-16"),
        "json envelope should carry the refusal: {}",
        json_report.message
    );
    assert_eq!(
        json_report.details.get("remote").and_then(|v| v.as_str()),
        Some(".")
    );
    assert_eq!(
        json_report
            .details
            .get("upstream_kind")
            .and_then(|v| v.as_str()),
        Some("local")
    );

    assert_eq!(refs_snapshot(repo), refs_before, "refs must stay unchanged");
    assert_eq!(
        branch_config_snapshot(repo),
        cfg_before,
        "branch.* config must stay unchanged"
    );
    assert_eq!(
        fetch_head_snapshot(repo),
        fetch_before,
        "FETCH_HEAD must stay unchanged"
    );
}

/// M-UPSTREAM P8 (#477 HF-30): `push` refuses a configured local upstream
/// before any network write.
#[test]
fn test_push_refuses_local_upstream() {
    let (repo, branch) = setup_local_upstream_current_branch();
    let p = repo.path();
    assert_local_upstream_network_refusal(&["push"], "push", &branch, p);

    let explicit = run_libra_command(&["push", "."], p);
    let (stderr, report) = parse_cli_error_stderr(&explicit.stderr);
    assert!(!explicit.status.success(), "explicit '.' must still fail");
    assert!(
        !report.message.contains("local upstream") && !stderr.contains("local upstream"),
        "explicit '.' must keep the existing rejection, not the local-upstream path: {stderr} / {}",
        report.message
    );
}

#[test]
fn test_push_path_argument_reaches_local_target_check() {
    let repo = create_committed_repo_via_cli();
    let remote_dir = tempfile::tempdir().unwrap();
    // Initialize a bare local Libra target so the anonymous path push succeeds
    // (issues/480 HP-07).
    assert_cli_success(
        &run_libra_command(&["init", "--bare"], remote_dir.path()),
        "init bare target",
    );
    let current = run_libra_command(&["branch", "--show-current"], repo.path());
    assert_cli_success(&current, "show current branch");
    let branch = String::from_utf8_lossy(&current.stdout).trim().to_string();

    let output = run_libra_command(
        &["push", remote_dir.path().to_str().unwrap(), branch.as_str()],
        repo.path(),
    );
    assert_cli_success(&output, "push to anonymous local path");
}
