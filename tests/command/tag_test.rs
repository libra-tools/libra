//! Tests tag creation and listing flows for lightweight and annotated tags.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::collections::HashSet;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use clap::Parser;
#[cfg(unix)]
use libra::utils::path;
use libra::{
    command::tag::{self, TagArgs},
    internal::{
        branch::Branch, config::ConfigKv, db::get_db_conn_instance, model::reference,
        tag as internal_tag,
    },
    utils::{
        error::StableErrorCode,
        output::OutputConfig,
        test::{ChangeDirGuard, setup_with_new_libra_in},
    },
};
use sea_orm::{ActiveModelTrait, Set};
use serial_test::serial;
use tempfile::tempdir;

use super::*;

// Test helpers and utilities for tag tests.
// These helpers work with the internal tag API (`internal::tag`) rather than the CLI
// because some tests need to create tags directly and inspect internal objects.

async fn setup_user_identity() {
    // Configure a predictable user identity for annotated tag creation
    ConfigKv::set("user.name", "Test User", false)
        .await
        .unwrap();
    ConfigKv::set("user.email", "test@example.com", false)
        .await
        .unwrap();
}

#[test]
fn test_tag_json_create_output_keeps_lightweight_message_null() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "tag", "v1.0"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "tag");
    assert_eq!(json["data"]["action"], "create");
    assert_eq!(json["data"]["name"], "v1.0");
    assert!(json["data"]["hash"].as_str().is_some());
    assert!(
        json["data"]["message"].is_null(),
        "expected lightweight create message to stay null, got: {json}"
    );
}

#[test]
fn test_tag_json_list_keeps_lightweight_message_null() {
    let repo = create_committed_repo_via_cli();

    let create_lightweight = run_libra_command(&["tag", "v1.0"], repo.path());
    assert_cli_success(&create_lightweight, "tag v1.0");

    let create_annotated = run_libra_command(&["tag", "-m", "Release v1.1", "v1.1"], repo.path());
    assert_cli_success(&create_annotated, "tag -m Release v1.1 v1.1");

    let output = run_libra_command(&["--json", "tag", "-l", "-n", "1"], repo.path());
    assert_cli_success(&output, "tag --json -l -n 1");

    let json = parse_json_stdout(&output);
    let tags = json["data"]["tags"]
        .as_array()
        .expect("expected tags array");
    let lightweight = tags
        .iter()
        .find(|entry| entry["name"] == "v1.0")
        .expect("expected lightweight tag entry");
    let annotated = tags
        .iter()
        .find(|entry| entry["name"] == "v1.1")
        .expect("expected annotated tag entry");

    assert!(
        lightweight["message"].is_null(),
        "unexpected lightweight tag: {lightweight}"
    );
    assert_eq!(annotated["message"], "Release v1.1");
}

#[test]
fn test_tag_list_filters_by_glob_pattern() {
    let repo = create_committed_repo_via_cli();
    assert_cli_success(
        &run_libra_command(&["tag", "v1.0"], repo.path()),
        "tag v1.0",
    );
    assert_cli_success(
        &run_libra_command(&["tag", "v2.0"], repo.path()),
        "tag v2.0",
    );

    let output = run_libra_command(&["--json", "tag", "-l", "v1*"], repo.path());
    assert_cli_success(&output, "tag --json -l v1*");
    let json = parse_json_stdout(&output);
    let names: Vec<String> = json["data"]["tags"]
        .as_array()
        .expect("expected tags array")
        .iter()
        .map(|entry| entry["name"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        names.contains(&"v1.0".to_string()),
        "v1.0 should match the glob 'v1*': {names:?}"
    );
    assert!(
        !names.contains(&"v2.0".to_string()),
        "v2.0 should NOT match the glob 'v1*': {names:?}"
    );
}

#[test]
fn test_tag_contains_filter() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    std::fs::write(p.join("a.txt"), "1\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add a");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    assert_cli_success(&run_libra_command(&["tag", "v1"], p), "tag v1");

    std::fs::write(p.join("b.txt"), "2\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.txt"], p), "add b");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );
    assert_cli_success(&run_libra_command(&["tag", "v2"], p), "tag v2");

    let head = run_libra_command(&["rev-parse", "HEAD"], p);
    let c2 = String::from_utf8_lossy(&head.stdout).trim().to_string();

    // Only v2 (at c2) contains c2; v1 (at c1) does not.
    let out = run_libra_command(&["--json", "tag", "--contains", &c2], p);
    assert_cli_success(&out, "tag --contains c2");
    let json = parse_json_stdout(&out);
    let names: Vec<String> = json["data"]["tags"]
        .as_array()
        .expect("expected tags array")
        .iter()
        .map(|entry| entry["name"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        names.contains(&"v2".to_string()),
        "v2 should contain c2: {names:?}"
    );
    assert!(
        !names.contains(&"v1".to_string()),
        "v1 should NOT contain c2: {names:?}"
    );
}

#[test]
fn test_tag_sign_embeds_pgp_signature() {
    let repo = create_committed_repo_via_cli();

    let out = run_libra_command(&["tag", "-s", "-m", "signed release", "v1.0"], repo.path());
    assert_cli_success(&out, "tag -s -m 'signed release' v1.0");

    // The signed annotated tag object must embed the armored PGP signature
    // after the message.
    let show = run_libra_command(&["cat-file", "-p", "v1.0"], repo.path());
    assert_cli_success(&show, "cat-file -p v1.0");
    let body = String::from_utf8_lossy(&show.stdout);
    assert!(
        body.contains("-----BEGIN PGP SIGNATURE-----"),
        "tag -s should embed a PGP signature block: {body}"
    );
    assert!(
        body.contains("signed release"),
        "tag message should precede the signature: {body}"
    );
}

#[test]
fn test_tag_sign_requires_message() {
    let repo = create_committed_repo_via_cli();
    // `-s` without `-m` is rejected at parse time (Libra has no tag editor).
    let out = run_libra_command(&["tag", "-s", "v1.0"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(129),
        "tag -s without -m should be a usage error"
    );
}

#[test]
fn test_tag_verify_accepts_own_signature() {
    let repo = create_committed_repo_via_cli();
    assert_cli_success(
        &run_libra_command(&["tag", "-s", "-m", "signed release", "v1.0"], repo.path()),
        "tag -s -m signed v1.0",
    );

    let out = run_libra_command(&["tag", "-v", "v1.0"], repo.path());
    assert_cli_success(&out, "tag -v v1.0");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Good signature for tag 'v1.0'"),
        "tag -v should accept the signature it produced: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn test_tag_verify_rejects_unsigned_tag() {
    let repo = create_committed_repo_via_cli();
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "plain annotated", "v1.0"], repo.path()),
        "tag -m plain v1.0",
    );

    // An annotated-but-unsigned tag has no signature to verify.
    let out = run_libra_command(&["tag", "-v", "v1.0"], repo.path());
    assert!(
        !out.status.success(),
        "tag -v on an unsigned tag should fail, stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn test_tag_create_outputs_concise_confirmation() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["tag", "v1.0"], repo.path());
    assert_cli_success(&output, "tag v1.0");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Created lightweight tag 'v1.0' at "),
        "unexpected stdout: {stdout}"
    );
}

#[test]
fn test_annotated_tag_create_outputs_concise_confirmation() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["tag", "-m", "Release v1.1", "v1.1"], repo.path());
    assert_cli_success(&output, "tag -m Release v1.1 v1.1");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Created annotated tag 'v1.1' at "),
        "unexpected stdout: {stdout}"
    );
}

#[test]
fn test_tag_json_delete_output_includes_deleted_hash() {
    let repo = create_committed_repo_via_cli();

    let create_output = run_libra_command(&["tag", "v1.0"], repo.path());
    assert_cli_success(&create_output, "tag v1.0");

    let output = run_libra_command(&["--json", "tag", "-d", "v1.0"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "tag");
    assert_eq!(json["data"]["action"], "delete");
    assert_eq!(json["data"]["name"], "v1.0");
    assert!(json["data"]["hash"].as_str().is_some());
}

#[tokio::test]
#[serial(cwd)]
async fn test_tag_json_delete_missing_target_emits_null_hash() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());
    insert_broken_tag_ref("missing-target", None).await;

    let output = run_libra_command(&["--json", "tag", "-d", "missing-target"], repo.path());
    assert_cli_success(&output, "json tag delete should remove ref without target");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "tag");
    assert_eq!(json["data"]["action"], "delete");
    assert_eq!(json["data"]["name"], "missing-target");
    assert!(
        json["data"]["hash"].is_null(),
        "expected null hash for missing target, got: {json}"
    );
    assert!(
        internal_tag::find_tag_ref("missing-target")
            .await
            .expect("lookup missing-target tag ref")
            .is_none(),
        "tag ref should be deleted"
    );
}

#[test]
fn test_tag_missing_name_action_flags_return_usage_errors() {
    let repo = create_committed_repo_via_cli();
    let cases = [
        (vec!["tag", "-d"], "tag name is required for --delete"),
        (vec!["tag", "-f"], "tag name is required for --force"),
        (
            vec!["tag", "-m", "annotated release"],
            "tag name is required when using --message",
        ),
    ];

    for (args, expected_message) in cases {
        let output = run_libra_command(&args, repo.path());
        let (stderr, report) = parse_cli_error_stderr(&output.stderr);

        assert_eq!(output.status.code(), Some(129), "args: {:?}", args);
        assert_eq!(report.error_code, "LBR-CLI-002", "args: {:?}", args);
        assert!(
            stderr.contains(expected_message),
            "expected stderr to contain '{expected_message}', got: {stderr}"
        );
    }
}

#[test]
fn test_tag_missing_name_usage_outranks_repo_not_found_outside_repo() {
    let cwd = tempdir().expect("failed to create non-repo directory");

    let output = run_libra_command(&["tag", "-d"], cwd.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        stderr.contains("tag name is required for --delete"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        !stderr.contains("not a Libra repository"),
        "usage error should outrank repo precondition, got: {stderr}"
    );
}

#[test]
fn test_tag_json_missing_name_action_flags_return_usage_errors() {
    let repo = create_committed_repo_via_cli();
    let cases = [
        (
            vec!["--json", "tag", "-d"],
            "tag name is required for --delete",
        ),
        (
            vec!["--json", "tag", "-f"],
            "tag name is required for --force",
        ),
        (
            vec!["--json", "tag", "-m", "annotated release"],
            "tag name is required when using --message",
        ),
    ];

    for (args, expected_message) in cases {
        let output = run_libra_command(&args, repo.path());
        let stderr = String::from_utf8_lossy(&output.stderr);
        let report: serde_json::Value =
            serde_json::from_slice(&output.stderr).expect("expected stderr JSON in --json mode");

        assert_eq!(output.status.code(), Some(129), "args: {:?}", args);
        assert!(
            output.stdout.is_empty(),
            "json error should keep stdout empty, args: {:?}, stdout: {}",
            args,
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(report["error_code"], "LBR-CLI-002", "args: {:?}", args);
        assert!(
            stderr.contains(expected_message),
            "expected stderr to contain '{expected_message}', got: {stderr}"
        );
    }
}

#[test]
fn test_tag_quiet_delete_suppresses_stdout() {
    let repo = create_committed_repo_via_cli();

    let create_output = run_libra_command(&["tag", "v1.0"], repo.path());
    assert_cli_success(&create_output, "tag v1.0");

    let delete_output = run_libra_command(&["--quiet", "tag", "-d", "v1.0"], repo.path());
    assert_cli_success(&delete_output, "quiet tag delete");
    assert!(
        delete_output.stdout.is_empty(),
        "quiet delete should keep stdout empty, got: {}",
        String::from_utf8_lossy(&delete_output.stdout)
    );

    let list_output = run_libra_command(&["tag", "-l"], repo.path());
    assert_cli_success(&list_output, "tag -l");
    let stdout = String::from_utf8_lossy(&list_output.stdout);
    assert!(
        !stdout.lines().any(|line| line.trim() == "v1.0"),
        "deleted tag should not be listed, got: {stdout}"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_tag_delete_allows_invalid_target_hash() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());
    insert_broken_tag_ref("broken", Some("not-a-valid-object-id")).await;

    let output = run_libra_command(&["tag", "-d", "broken"], repo.path());
    assert_cli_success(&output, "tag delete should remove broken ref");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Deleted tag 'broken'"),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("(was not-a-v"),
        "delete output should include abbreviated target hash, got: {stdout}"
    );
    assert!(
        internal_tag::find_tag_ref("broken")
            .await
            .expect("lookup broken tag ref")
            .is_none(),
        "broken tag ref should be deleted"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_tag_json_delete_allows_invalid_target_hash() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());
    insert_broken_tag_ref("broken-json", Some("not-a-valid-object-id")).await;

    let output = run_libra_command(&["--json", "tag", "-d", "broken-json"], repo.path());
    assert_cli_success(&output, "json tag delete should remove broken ref");
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "tag");
    assert_eq!(json["data"]["action"], "delete");
    assert_eq!(json["data"]["name"], "broken-json");
    assert_eq!(json["data"]["hash"], "not-a-valid-object-id");
    assert!(
        internal_tag::find_tag_ref("broken-json")
            .await
            .expect("lookup broken json tag ref")
            .is_none(),
        "broken tag ref should be deleted"
    );
}

/// Return the full ref name for a tag (e.g. "refs/tags/v1.0").
fn ref_name(tag: &str) -> String {
    format!("refs/tags/{tag}")
}

/// List tag names returned by `internal_tag::list()`.
/// `internal_tag::list()` returns bare tag names (without the "refs/tags/" prefix).
async fn list_tag_refs() -> Vec<String> {
    internal_tag::list()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|t| t.name)
        .collect()
}

/// Find a tag by name.
/// Accepts either a full ref ("refs/tags/<name>") or a bare name ("<name>").
async fn get_tag_by_name(full_ref: &str) -> Option<internal_tag::Tag> {
    // Support both full ref (refs/tags/...) and bare tag name
    let search = full_ref.strip_prefix("refs/tags/").unwrap_or(full_ref);
    internal_tag::list()
        .await
        .ok()?
        .into_iter()
        .find(|t| t.name == search)
}

/// Returns true if a tag with the given bare name exists.
async fn tag_exists(name: &str) -> bool {
    let full = ref_name(name);
    get_tag_by_name(&full).await.is_some()
}

async fn insert_broken_tag_ref(name: &str, target: Option<&str>) {
    let db = get_db_conn_instance().await;
    let row = reference::ActiveModel {
        name: Set(Some(ref_name(name))),
        kind: Set(reference::ConfigKind::Tag),
        commit: Set(target.map(str::to_string)),
        remote: Set(None),
        ..Default::default()
    };
    row.insert(&db)
        .await
        .expect("failed to insert broken tag reference");
}

/// Read the object id the tag points to (as a string), if present.
async fn read_tag_oid(name: &str) -> Option<String> {
    let full = ref_name(name);
    let tag = get_tag_by_name(&full).await?;

    match &tag.object {
        internal_tag::TagObject::Commit(c) => Some(c.id.to_string()),
        internal_tag::TagObject::Tag(t) => Some(t.object_hash.to_string()),
        internal_tag::TagObject::Tree(tr) => Some(tr.id.to_string()),
        internal_tag::TagObject::Blob(b) => Some(b.id.to_string()),
    }
}

/// Return a set of bare tag names currently present (no refs/tags/ prefix).
async fn list_tag_names() -> HashSet<String> {
    list_tag_refs().await.into_iter().collect()
}

/// Assert the tag exists; provide helpful failure message.
async fn assert_tag_exists(name: &str) {
    assert!(tag_exists(name).await, "Tag does not exist: {}", name);
}

/// Assert the tag is absent; provide helpful failure message.
async fn assert_tag_absent(name: &str) {
    assert!(!tag_exists(name).await, "Tag still exists: {}", name);
}

#[cfg(unix)]
fn collect_directory_modes(path: &std::path::Path, modes: &mut Vec<(std::path::PathBuf, u32)>) {
    let metadata = std::fs::metadata(path).expect("failed to stat path");
    modes.push((path.to_path_buf(), metadata.permissions().mode()));
    for entry in std::fs::read_dir(path).expect("failed to read directory") {
        let entry = entry.expect("failed to read directory entry");
        let child = entry.path();
        if child.is_dir() {
            collect_directory_modes(&child, modes);
        }
    }
}

#[cfg(unix)]
fn set_directory_mode_recursive(path: &std::path::Path, mode: u32) {
    let mut modes = Vec::new();
    collect_directory_modes(path, &mut modes);
    for (dir, _) in modes {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode))
            .expect("failed to update directory permissions");
    }
}

#[cfg(unix)]
fn restore_directory_modes(modes: &[(std::path::PathBuf, u32)]) {
    for (dir, mode) in modes.iter().rev() {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(*mode))
            .expect("failed to restore directory permissions");
    }
}

// --- Shared setup helpers ---

/// Create a new temporary repo, set it as current dir, set up identity, add a file and commit.
/// Returns the TempDir and a ChangeDirGuard so the caller can keep the guard alive for test duration.
async fn setup_repo_with_commit() -> (tempfile::TempDir, ChangeDirGuard) {
    setup_repo_with_commit_with("content", "Initial commit").await
}

/// Same as `setup_repo_with_commit` but allows specifying file content and commit message.
async fn setup_repo_with_commit_with(
    content: &str,
    commit_msg: &str,
) -> (tempfile::TempDir, ChangeDirGuard) {
    let temp = tempdir().unwrap();
    // Switch working dir to the temp repo; keep the tempdir alive by returning it along with the guard.
    let guard = ChangeDirGuard::new(temp.path());
    setup_with_new_libra_in(temp.path()).await;
    setup_user_identity().await;

    std::fs::write("file.txt", content).unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".into()],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;

    commit::execute(CommitArgs {
        message: Some(commit_msg.into()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: true,
        author: None,
        ..Default::default()
    })
    .await;

    (temp, guard)
}

#[test]
fn test_tag_cli_duplicate_tag_returns_conflict_exit_code_without_stdout() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["tag", "v1"], repo.path());
    assert_cli_success(&output, "failed to create initial tag");

    let output = run_libra_command(&["tag", "v1"], repo.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert!(stdout.trim().is_empty(), "unexpected stdout: {stdout}");
    assert!(stderr.contains("fatal: tag 'v1' already exists"));
    assert!(stderr.contains("Error-Code: LBR-CONFLICT-002"));
}

#[test]
fn test_tag_cli_unborn_head_returns_repo_state_error() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["tag", "v1"], repo.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert!(stdout.trim().is_empty(), "unexpected stdout: {stdout}");
    assert_eq!(report.error_code, "LBR-REPO-003");
    assert_eq!(report.category, "repo");
    assert!(
        stderr.contains("fatal: Cannot create tag: HEAD does not point to a commit"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        report
            .hints
            .iter()
            .any(|hint| hint == "create a commit first before tagging HEAD."),
        "expected repo-state hint, got: {:?}",
        report.hints
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_tag_cli_corrupt_head_storage_returns_repo_corrupt() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());
    Branch::update_branch("main", "not-a-valid-hash", None)
        .await
        .unwrap();

    let output = run_libra_command(&["tag", "v1"], repo.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert!(stdout.trim().is_empty(), "unexpected stdout: {stdout}");
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert_eq!(report.category, "repo");
    assert!(
        stderr.contains("failed to resolve HEAD commit"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        stderr.contains("stored branch reference 'main' is corrupt"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn test_tag_json_unborn_head_returns_repo_state_error() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["--json", "tag", "v1"], repo.path());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("expected stderr JSON in --json mode");

    assert_eq!(output.status.code(), Some(128));
    assert!(
        output.stdout.is_empty(),
        "json error should keep stdout empty, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(report["error_code"], "LBR-REPO-003");
    assert_eq!(report["category"], "repo");
    assert_eq!(
        report["message"],
        "Cannot create tag: HEAD does not point to a commit"
    );
}

// Test cases

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_basic_tag_creation() {
    // Create an isolated temporary repository and ensure a commit exists.
    let (_temp, _guard) = setup_repo_with_commit().await;

    // Create a lightweight tag that points to HEAD commit.
    internal_tag::create("v1.0.0", None, false, false)
        .await
        .unwrap();

    // Verify tag presence and that we can read the pointed object id.
    assert_tag_exists("v1.0.0").await;
    assert!(
        read_tag_oid("v1.0.0").await.is_some(),
        "Should be able to read tag OID"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_tag_with_message() {
    // Create a tag with an annotation message (annotated tag) and verify presence.
    let (_temp, _guard) = setup_repo_with_commit_with("content", "Commit with message").await;

    // Annotated tag creation (includes tagger and message fields internally).
    internal_tag::create("v1.0.1", Some("Release v1.0.1".into()), false, false)
        .await
        .unwrap();

    assert_tag_exists("v1.0.1").await;
    assert!(read_tag_oid("v1.0.1").await.is_some());

    // Verify the annotated tag object contains the expected message.
    let result = internal_tag::find_tag_and_commit("v1.0.1").await;
    assert!(
        result.is_ok(),
        "find_tag_and_commit failed: {:?}",
        result.err()
    );
    let opt = result.unwrap();
    let (object, _commit) = opt.expect("Annotated tag not found");
    if let internal_tag::TagObject::Tag(tag_object) = object {
        assert_eq!(tag_object.message, "Release v1.0.1");
    } else {
        panic!("Expected annotated Tag object");
    }
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_force_tag() {
    // Verify that forcing a tag replaces the ref target.
    let (_temp, _guard) = setup_repo_with_commit_with("v1", "First").await;

    internal_tag::create("v1.0", Some("Initial".into()), false, false)
        .await
        .unwrap();
    assert_tag_exists("v1.0").await;
    let before = read_tag_oid("v1.0").await;

    // Make second commit with updated content
    std::fs::write("file.txt", "v2").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".into()],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("Second".into()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: true,
        author: None,
        ..Default::default()
    })
    .await;

    // Use CLI path for force update to exercise both CLI and internal logic.
    tag::execute(TagArgs {
        name: Some("v1.0".into()),
        file: None,
        edit: false,
        list: false,
        delete: false,
        message: Some("Updated".into()),
        force: true,
        n_lines: None,
        points_at: None,
        contains: None,
        no_contains: None,
        merged: None,
        no_merged: None,
        sort: None,
        column: None,
        sign: false,
        no_sign: false,
        no_column: false,
        verify: false,
    })
    .await;
    let after = read_tag_oid("v1.0").await;
    assert!(
        before.is_some() && after.is_some() && before != after,
        "force update should change OID (before: {:?}, after: {:?})",
        before,
        after
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_force_tag_store_failure_preserves_existing_ref() {
    if skip_permission_denied_test_if_root("test_force_tag_store_failure_preserves_existing_ref") {
        return;
    }

    let (_temp, _guard) = setup_repo_with_commit_with("content", "Base").await;

    internal_tag::create("v1.0", Some("Initial".into()), false, false)
        .await
        .unwrap();
    let (before_object, _) = internal_tag::find_tag_and_commit("v1.0")
        .await
        .unwrap()
        .expect("tag should exist before failed force update");
    let before_message = match before_object {
        internal_tag::TagObject::Tag(tag_object) => tag_object.message,
        other => panic!("expected annotated tag before update, got {other:?}"),
    };

    let objects_dir = path::objects();
    let mut original_modes = Vec::new();
    collect_directory_modes(&objects_dir, &mut original_modes);
    set_directory_mode_recursive(&objects_dir, 0o555);

    let result = internal_tag::create("v1.0", Some("Updated".into()), true, false).await;

    restore_directory_modes(&original_modes);

    assert!(
        matches!(result, Err(internal_tag::CreateTagError::StoreObject(_))),
        "expected store failure, got: {result:?}"
    );

    let (after_object, _) = internal_tag::find_tag_and_commit("v1.0")
        .await
        .unwrap()
        .expect("original tag should remain after failed force update");
    match after_object {
        internal_tag::TagObject::Tag(tag_object) => {
            assert_eq!(tag_object.message, before_message);
        }
        other => panic!("expected annotated tag after failed update, got {other:?}"),
    }
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_internal_create_returns_metadata_for_annotated_tag() {
    let (_temp, _guard) = setup_repo_with_commit_with("content", "Base").await;

    let created = internal_tag::create("v1.0", Some("Release v1.0".into()), false, false)
        .await
        .unwrap();

    assert_eq!(created.name, "v1.0");
    assert!(created.annotated);
    assert_eq!(created.message.as_deref(), Some("Release v1.0"));

    let tag = get_tag_by_name("v1.0")
        .await
        .expect("created tag should exist");
    let stored_hash = match tag.object {
        internal_tag::TagObject::Tag(tag_object) => tag_object.id.to_string(),
        other => panic!("expected annotated tag object, got {other:?}"),
    };
    assert_eq!(created.target.to_string(), stored_hash);
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_list_tags() {
    // Verify listing returns created tag names.
    let (_temp, _guard) = setup_repo_with_commit_with("content", "Base").await;

    internal_tag::create("v1.0.0", None, false, false)
        .await
        .unwrap();
    internal_tag::create("v2.0.0", None, false, false)
        .await
        .unwrap();

    let names = list_tag_names().await;
    assert!(names.contains("v1.0.0"));
    assert!(names.contains("v2.0.0"));
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_delete_tag() {
    // Verify delete removes the tag ref.
    let (_temp, _guard) = setup_repo_with_commit_with("content", "Delete base").await;

    internal_tag::create("to-delete", None, false, false)
        .await
        .unwrap();
    assert_tag_exists("to-delete").await;

    tag::execute(TagArgs {
        name: Some("to-delete".into()),
        file: None,
        edit: false,
        list: false,
        delete: true,
        message: None,
        force: false,
        n_lines: None,
        points_at: None,
        contains: None,
        no_contains: None,
        merged: None,
        no_merged: None,
        sort: None,
        column: None,
        sign: false,
        no_sign: false,
        no_column: false,
        verify: false,
    })
    .await;
    assert_tag_absent("to-delete").await;
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_annotation_lines_tag() {
    let (_temp, _guard) = setup_repo_with_commit_with("lightweight-tag", "First").await;

    // lightweight tag creation
    internal_tag::create("v1.0.0", None, false, false)
        .await
        .unwrap();

    // Make second commit with updated content
    std::fs::write("file.txt", "annotation-tag").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".into()],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("Second".into()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: true,
        author: None,
        ..Default::default()
    })
    .await;

    // Make second tag with single line annotation
    tag::execute(TagArgs {
        name: Some("v1.0.1".into()),
        file: None,
        edit: false,
        list: false,
        delete: false,
        message: Some("Single line annotation message".into()),
        force: false,
        n_lines: None,
        points_at: None,
        contains: None,
        no_contains: None,
        merged: None,
        no_merged: None,
        sort: None,
        column: None,
        sign: false,
        no_sign: false,
        no_column: false,
        verify: false,
    })
    .await;

    std::fs::write("file.txt", "annotation-multi-line-tag").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["file.txt".into()],
        all: false,
        update: false,
        verbose: false,
        dry_run: false,
        ignore_errors: false,
        refresh: false,
        force: false,

        pathspec_from_file: None,
        pathspec_file_nul: false,
        chmod: None,
        renormalize: false,
        ignore_missing: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("Third".into()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: false,
        all: false,
        no_verify: true,
        author: None,
        ..Default::default()
    })
    .await;

    // Make third tag with multi line annotation
    tag::execute(TagArgs {
        name: Some("v1.0.3".into()),
        file: None,
        edit: false,
        list: false,
        delete: false,
        message: Some("multi\nline\nannotation\ntag".into()),
        force: false,
        n_lines: None,
        points_at: None,
        contains: None,
        no_contains: None,
        merged: None,
        no_merged: None,
        sort: None,
        column: None,
        sign: false,
        no_sign: false,
        no_column: false,
        verify: false,
    })
    .await;

    let output1 = tag::render_tags(4).await.unwrap();

    // Split the output into lines
    let output_lines1: Vec<&str> = output1
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect();

    // v1.0.0 (lightweight tag)
    assert!(output_lines1.contains(&"v1.0.0               First"));

    // v1.0.1 (single line tag)
    assert!(output_lines1.contains(&"v1.0.1               Single line annotation message"));

    // v1.0.3 (multi line tag)
    assert!(output_lines1.contains(&"v1.0.3               multi"));
    assert!(output_lines1.contains(&"line"));
    assert!(output_lines1.contains(&"annotation"));
    assert!(output_lines1.contains(&"tag"));

    let output2 = tag::render_tags(2).await.unwrap();

    // Split the output into lines
    let output_lines2: Vec<&str> = output2
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect();

    // v1.0.0 (lightweight tag)
    assert!(output_lines2.contains(&"v1.0.0               First"));

    // v1.0.1 (single line tag)
    assert!(output_lines2.contains(&"v1.0.1               Single line annotation message"));

    // v1.0.3 (multi line tag)
    assert!(output_lines2.contains(&"v1.0.3               multi"));
    assert!(output_lines2.contains(&"line"));
    assert!(!output_lines2.contains(&"annotation"));
    assert!(!output_lines2.contains(&"tag"));
}

/// `libra rev-parse <rev>` → trimmed OID string (panics on failure).
fn tag_rev_parse(repo: &std::path::Path, rev: &str) -> String {
    let out = run_libra_command(&["rev-parse", rev], repo);
    assert_cli_success(&out, "rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Collect the `name` field of every tag in a `--json tag` list envelope.
fn tag_list_names(output: &std::process::Output) -> HashSet<String> {
    let json = parse_json_stdout(output);
    json["data"]["tags"]
        .as_array()
        .expect("expected tags array")
        .iter()
        .map(|entry| {
            entry["name"]
                .as_str()
                .expect("tag entry missing name")
                .to_string()
        })
        .collect()
}

/// `--points-at <object>` peels each tag to its commit and keeps only those
/// resolving to the requested object. A lightweight tag points straight at a
/// commit; an annotated tag peels through its tag object to the same commit,
/// so both surface when the second commit is requested.
#[test]
fn test_tag_points_at_filters_to_matching_commit() {
    let repo = create_committed_repo_via_cli();

    // The base commit already exists; tag it lightweight.
    let base = tag_rev_parse(repo.path(), "HEAD");
    assert_cli_success(
        &run_libra_command(&["tag", "v-base"], repo.path()),
        "tag v-base",
    );

    // Add a second commit; tag it both lightweight and annotated.
    std::fs::write(repo.path().join("second.txt"), "second\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "second.txt"], repo.path()),
        "add second",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path()),
        "commit second",
    );
    let second = tag_rev_parse(repo.path(), "HEAD");
    assert_cli_success(
        &run_libra_command(&["tag", "v-second"], repo.path()),
        "tag v-second",
    );
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "annotated second", "a-second"], repo.path()),
        "tag a-second",
    );

    // --points-at <base>: only the lightweight tag on the base commit.
    let out = run_libra_command(&["--json", "tag", "--points-at", &base], repo.path());
    assert_cli_success(&out, "tag --points-at base");
    assert_eq!(tag_list_names(&out), HashSet::from(["v-base".to_string()]));

    // --points-at <second>: lightweight v-second AND annotated a-second
    // (the annotated tag peels through its tag object to the second commit).
    let out = run_libra_command(&["--json", "tag", "--points-at", &second], repo.path());
    assert_cli_success(&out, "tag --points-at second");
    assert_eq!(
        tag_list_names(&out),
        HashSet::from(["v-second".to_string(), "a-second".to_string()]),
    );
}

/// An unresolvable `--points-at` revision fails with a `not a valid object
/// name` message rather than a raw resolver error.
#[test]
fn test_tag_points_at_invalid_object_errors() {
    let repo = create_committed_repo_via_cli();
    let out = run_libra_command(&["tag", "--points-at", "definitely-not-a-ref"], repo.path());
    assert!(
        !out.status.success(),
        "expected failure for invalid --points-at object",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a valid object name"),
        "expected 'not a valid object name' in stderr, got: {stderr}",
    );
}

#[test]
fn test_tag_sort_by_refname() {
    let repo = create_committed_repo_via_cli();
    assert_cli_success(
        &run_libra_command(&["tag", "v3.0"], repo.path()),
        "tag v3.0",
    );
    assert_cli_success(
        &run_libra_command(&["tag", "v1.0"], repo.path()),
        "tag v1.0",
    );
    assert_cli_success(
        &run_libra_command(&["tag", "v2.0"], repo.path()),
        "tag v2.0",
    );

    let out = run_libra_command(&["tag", "--sort=refname"], repo.path());
    assert_cli_success(&out, "tag --sort=refname");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let names: Vec<&str> = stdout.lines().collect();
    assert_eq!(names, vec!["v1.0", "v2.0", "v3.0"]);

    let out = run_libra_command(&["tag", "--sort=-refname"], repo.path());
    assert_cli_success(&out, "tag --sort=-refname");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let names: Vec<&str> = stdout.lines().collect();
    assert_eq!(names, vec!["v3.0", "v2.0", "v1.0"]);
}

#[test]
fn test_tag_sort_invalid_key_errors() {
    let repo = create_committed_repo_via_cli();
    let out = run_libra_command(&["tag", "--sort=bogus"], repo.path());
    assert!(
        !out.status.success(),
        "expected failure for invalid sort key"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unsupported tag sort key"),
        "expected sort key error in stderr, got: {stderr}",
    );
}

#[test]
fn test_tag_merged_filters_reachable_tags() {
    let repo = create_committed_repo_via_cli();

    // Tag the current HEAD
    assert_cli_success(
        &run_libra_command(&["tag", "v-base"], repo.path()),
        "tag v-base",
    );

    // Make a second commit and tag it
    std::fs::write(repo.path().join("new.txt"), "new\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "new.txt"], repo.path()),
        "add new.txt",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path()),
        "commit second",
    );
    assert_cli_success(
        &run_libra_command(&["tag", "v-second"], repo.path()),
        "tag v-second",
    );

    // --merged HEAD: both tags should be reachable from HEAD
    let out = run_libra_command(&["tag", "--merged", "HEAD"], repo.path());
    assert_cli_success(&out, "tag --merged HEAD");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("v-base"),
        "v-base should be merged: {stdout}"
    );
    assert!(
        stdout.contains("v-second"),
        "v-second should be merged: {stdout}"
    );

    // --no-merged HEAD: no tags should be unreachable from HEAD
    let out = run_libra_command(&["tag", "--no-merged", "HEAD"], repo.path());
    assert_cli_success(&out, "tag --no-merged HEAD");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("v-base"),
        "v-base should not be in no-merged: {stdout}"
    );
    assert!(
        !stdout.contains("v-second"),
        "v-second should not be in no-merged: {stdout}"
    );
}

#[test]
fn test_tag_dash_f_reads_message_from_file_and_stdin() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // `-F <file>` creates an annotated tag carrying the file's content.
    std::fs::write(p.join("tagmsg.txt"), "annotated from file\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["tag", "-F", "tagmsg.txt", "v-file"], p),
        "tag -F file",
    );
    let shown = run_libra_command(&["cat-file", "-p", "v-file"], p);
    assert_cli_success(&shown, "cat-file -p v-file");
    assert!(
        String::from_utf8_lossy(&shown.stdout).contains("annotated from file"),
        "annotated message from file: {}",
        String::from_utf8_lossy(&shown.stdout)
    );

    // `-F -` reads the message from standard input.
    let out = run_libra_command_with_stdin(&["tag", "-F", "-", "v-stdin"], p, "from stdin\n");
    assert_cli_success(&out, "tag -F -");
    let shown2 = run_libra_command(&["cat-file", "-p", "v-stdin"], p);
    assert_cli_success(&shown2, "cat-file -p v-stdin");
    assert!(
        String::from_utf8_lossy(&shown2.stdout).contains("from stdin"),
        "annotated message from stdin: {}",
        String::from_utf8_lossy(&shown2.stdout)
    );

    // `-F` conflicts with `-m`.
    let both = run_libra_command(&["tag", "-F", "tagmsg.txt", "-m", "x", "v-both"], p);
    assert!(!both.status.success(), "-F conflicts with -m");
}

#[test]
fn test_tag_dash_f_read_error_and_create_only_validation() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // A missing `-F` file fails with the stable read-error code and creates no tag.
    let out = run_libra_command(&["--json", "tag", "-F", "nope.txt", "v-missing"], p);
    assert!(!out.status.success(), "missing -F file should fail");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("LBR-IO-001"),
        "stable read-error code expected: {combined}"
    );
    let tags = run_libra_command(&["tag", "-l"], p);
    assert!(
        !String::from_utf8_lossy(&tags.stdout).contains("v-missing"),
        "no tag should be created on a read error"
    );

    // `-m`/`-F` are create-only: rejected when combined with delete/list modes.
    std::fs::write(p.join("m.txt"), "x\n").unwrap();
    for combo in [
        vec!["tag", "-d", "-F", "m.txt", "v1"],
        vec!["tag", "-l", "-F", "m.txt", "v1"],
        vec!["tag", "-d", "-m", "x", "v1"],
    ] {
        let r = run_libra_command(&combo, p);
        assert!(
            !r.status.success(),
            "{combo:?} should be a create-only usage error"
        );
    }
}

#[tokio::test]
#[serial(cwd)]
async fn test_tag_create_only_validation_blocks_programmatic_entry() {
    // The cli.rs preflight is not the only guard: the programmatic
    // `execute_safe` entry must also reject a message source combined with a
    // non-create mode. Run OUTSIDE a repository so the error TYPE pins the
    // regression: the create-only validation fires before `require_repo`, so a
    // working guard returns the usage error (CliInvalidArguments). Without it,
    // delete-mode would reach `require_repo` and return repo-not-found instead.
    let temp = tempdir().unwrap();
    let _guard = ChangeDirGuard::new(temp.path()); // bare dir, no `.libra`

    let args = TagArgs::try_parse_from(["tag", "-d", "-F", "msg.txt", "v-keep"])
        .expect("clap should parse -d -F (the conflict is semantic, not clap-level)");
    let err = tag::execute_safe(args, &OutputConfig::default())
        .await
        .expect_err("delete + --file must be rejected before any repo/delete work");
    assert_eq!(
        err.stable_code(),
        StableErrorCode::CliInvalidArguments,
        "expected the create-only usage error, not repo-not-found: {}",
        err.message()
    );

    let args_msg = TagArgs::try_parse_from(["tag", "-d", "-m", "x", "v-keep"]).expect("clap parse");
    let err_msg = tag::execute_safe(args_msg, &OutputConfig::default())
        .await
        .expect_err("delete + --message must also be rejected");
    assert_eq!(
        err_msg.stable_code(),
        StableErrorCode::CliInvalidArguments,
        "expected the create-only usage error for -m too: {}",
        err_msg.message()
    );
}

#[test]
fn tag_column_lays_out_in_column_major_order() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    for t in ["v1", "v2", "v3", "v4"] {
        assert_cli_success(&run_libra_command(&["tag", t], p), t);
    }

    // COLUMNS=10, col_width = 2 + 2 = 4. The most columns whose total is strictly
    // < 10 is 2 (2*4 = 8 < 10; 3*4 = 12 too wide). rows = ceil(4/2) = 2.
    // Column-major (fill down then across), matching `git tag --column`:
    //   v1  v3
    //   v2  v4
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(p)
        .env("COLUMNS", "10")
        .args(["tag", "--column=always"])
        .output()
        .expect("run tag --column=always");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        vec!["v1  v3", "v2  v4"],
        "column-major layout: {stdout:?}"
    );

    // `--column=row` fills left-to-right instead (same 2x2 grid):
    //   v1  v2
    //   v3  v4
    let row = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(p)
        .env("COLUMNS", "10")
        .args(["tag", "--column=always,row"])
        .output()
        .expect("run tag --column=always,row");
    let row_out = String::from_utf8_lossy(&row.stdout);
    assert_eq!(
        row_out.lines().collect::<Vec<_>>(),
        vec!["v1  v2", "v3  v4"],
        "row-major layout: {row_out:?}"
    );

    // `never` falls back to one tag per line.
    let never = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(p)
        .env("COLUMNS", "8")
        .args(["tag", "--column=never"])
        .output()
        .expect("run tag --column=never");
    let never_out = String::from_utf8_lossy(&never.stdout);
    let never_lines: Vec<&str> = never_out.lines().collect();
    assert_eq!(
        never_lines,
        vec!["v1", "v2", "v3", "v4"],
        "never = one per line"
    );

    // An unknown mode is a usage error.
    let bogus = run_libra_command(&["tag", "--column=bogus"], p);
    assert!(!bogus.status.success(), "unknown --column mode must error");

    // The mode is validated up front, so it also errors under --json, where the
    // human column renderer is skipped entirely.
    let bogus_json = run_libra_command(&["--json", "tag", "--column=bogus"], p);
    assert!(
        !bogus_json.status.success(),
        "unknown --column mode must error even with --json (upfront validation)"
    );

    // `-m` (create) combined with the list-only `--column` is a usage error.
    let create_conflict = run_libra_command(&["tag", "-m", "msg", "--column=always", "vX"], p);
    assert!(
        !create_conflict.status.success(),
        "-m with --column must be a usage error"
    );

    // --column cannot be combined with -n.
    let conflict = run_libra_command(&["tag", "--column", "-n", "1"], p);
    assert!(!conflict.status.success(), "--column conflicts with -n");
}

#[test]
fn tag_no_sign_countermands_sign() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // `--no-sign` alone creates an unsigned tag (the default).
    let out = run_libra_command(&["tag", "--no-sign", "v-nosign"], p);
    assert_cli_success(&out, "tag --no-sign v-nosign");

    // `-s --no-sign` (last wins) countermands `-s`: an UNSIGNED annotated tag is
    // created, so there is no vault-signing attempt/error.
    let out2 = run_libra_command(&["tag", "-s", "--no-sign", "-m", "msg", "v-override"], p);
    assert_cli_success(&out2, "tag -s --no-sign -m msg v-override");

    let tags = run_libra_command(&["tag"], p);
    let listed = String::from_utf8_lossy(&tags.stdout);
    assert!(
        listed.contains("v-nosign") && listed.contains("v-override"),
        "both tags created: {listed}"
    );
}

#[test]
fn tag_no_column_countermands_column() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    run_libra_command(&["tag", "v1aaaa"], p);
    run_libra_command(&["tag", "v2bbbb"], p);

    // `--no-column` alone lists one tag per line (the default).
    let plain = run_libra_command(&["tag", "--no-column"], p);
    assert_cli_success(&plain, "tag --no-column");
    let plain_out = String::from_utf8_lossy(&plain.stdout);
    assert!(plain_out.contains("v1aaaa\n"), "one per line: {plain_out}");

    // `--column=always --no-column` (last wins) countermands `--column`, so the
    // listing is one-per-line, NOT columnar (no two names share a line).
    let out = run_libra_command(&["tag", "--column=always", "--no-column"], p);
    assert_cli_success(&out, "tag --column=always --no-column");
    let listed = String::from_utf8_lossy(&out.stdout);
    assert!(
        !listed
            .lines()
            .any(|l| l.contains("v1aaaa") && l.contains("v2bbbb")),
        "--no-column countermands --column (one per line): {listed}"
    );
}

/// End-to-end `tag -e`/`--edit`: a scripted editor composes an annotated-tag
/// message (comments stripped); with `-m` the editor is pre-filled and a no-op
/// editor keeps that seed; a comment-only buffer aborts with "no tag message".
#[cfg(unix)]
#[test]
fn tag_edit_composes_seeds_and_aborts_via_editor() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // Write a scripted editor that overwrites the buffer ($1) with `body`.
    let overwrite_editor = |name: &str, body: &str| -> String {
        let path = p.join(name);
        std::fs::write(&path, format!("#!/bin/sh\nprintf '%s' '{body}' > \"$1\"\n")).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path.to_string_lossy().into_owned()
    };

    // 1) `-e` alone composes an annotated tag from the editor; comments stripped.
    let compose = overwrite_editor("compose.sh", "composed via editor\n# ignored comment\n");
    let out = run_libra_command_with_stdin_and_env(
        &["tag", "-e", "v-edit"],
        p,
        "",
        &[("GIT_EDITOR", compose.as_str())],
    );
    assert_cli_success(&out, "tag -e compose");
    let listed = run_libra_command(&["tag", "-n1", "v-edit"], p);
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("composed via editor"),
        "annotated message composed in editor: {}",
        String::from_utf8_lossy(&listed.stdout)
    );

    // 2) `-e -m "seed"` pre-fills the editor; a no-op editor keeps the seed.
    let noop_path = p.join("noop.sh");
    std::fs::write(&noop_path, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = std::fs::metadata(&noop_path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&noop_path, perms).unwrap();
    let out = run_libra_command_with_stdin_and_env(
        &["tag", "-e", "-m", "seeded body", "v-seed"],
        p,
        "",
        &[("GIT_EDITOR", noop_path.to_string_lossy().as_ref())],
    );
    assert_cli_success(&out, "tag -e -m seed");
    let listed = run_libra_command(&["tag", "-n1", "v-seed"], p);
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("seeded body"),
        "the -m seed survives an unedited editor buffer: {}",
        String::from_utf8_lossy(&listed.stdout)
    );

    // 3) A comment-only buffer cleans to empty and aborts (exit 128).
    let empty = overwrite_editor("empty.sh", "# only a comment\n");
    let out = run_libra_command_with_stdin_and_env(
        &["tag", "-e", "v-empty"],
        p,
        "",
        &[("GIT_EDITOR", empty.as_str())],
    );
    assert_eq!(
        out.status.code(),
        Some(128),
        "empty edited message aborts: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no tag message given"),
        "abort message: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `-e`/`--edit` is a create-only mode: it requires a tag name and is rejected
/// when combined with list/delete. These guards fire before the editor opens,
/// so no scripted editor is needed.
#[test]
fn tag_edit_rejected_outside_create_mode() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // Bare `-e` with no name must not silently fall into list mode.
    let no_name = run_libra_command(&["tag", "-e"], p);
    assert!(!no_name.status.success(), "bare `tag -e` requires a name");
    assert!(
        String::from_utf8_lossy(&no_name.stderr).contains("tag name is required"),
        "missing-name hint: {}",
        String::from_utf8_lossy(&no_name.stderr)
    );

    // `-e` with a listing flag (and a name, so the missing-name guard passes
    // first) is a usage error (not a silent list).
    let with_list = run_libra_command(&["tag", "-e", "-l", "v1"], p);
    assert!(
        !with_list.status.success(),
        "`tag -e -l` is rejected as a non-create mode"
    );
    assert!(
        String::from_utf8_lossy(&with_list.stderr).contains("only valid when creating a tag"),
        "create-only hint: {}",
        String::from_utf8_lossy(&with_list.stderr)
    );

    // `-e` with delete must not silently delete while ignoring the editor.
    let with_delete = run_libra_command(&["tag", "-e", "-d", "whatever"], p);
    assert!(
        !with_delete.status.success(),
        "`tag -e -d` is rejected as a non-create mode"
    );
}

/// Helper: run `tag -l --sort=refname --column=<spec>` at a fixed COLUMNS width
/// and return the output lines.
fn tag_column_lines(p: &std::path::Path, spec: &str, columns: &str) -> Vec<String> {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(p)
        .env("COLUMNS", columns)
        .args(["tag", "-l", "--sort=refname", &format!("--column={spec}")])
        .output()
        .expect("run tag --column");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn tag_column_dense_row_and_boundaries_match_git() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // Widest name is "longtag-xyz" (11) → column width 13.
    for t in [
        "v1.0",
        "v1.1",
        "v1.2",
        "v2.0",
        "v2.1",
        "v10.0",
        "alpha",
        "beta",
        "rc-1",
        "longtag-xyz",
    ] {
        assert_cli_success(&run_libra_command(&["tag", t], p), t);
    }
    let entries = |lines: &[String]| -> Vec<usize> {
        lines.iter().map(|l| l.split_whitespace().count()).collect()
    };

    // NODENSE column-major at COLUMNS=80: floor((80-1)/13)=6 columns fit, but
    // column-major recomputes cols=ceil(10/2)=5 to drop the empty trailing slot.
    let cols80 = tag_column_lines(p, "always", "80");
    assert_eq!(
        entries(&cols80),
        vec![5, 5],
        "column,nodense w=80 → 5 cols: {cols80:?}"
    );
    // First row is the heads of each (top-to-bottom) column.
    assert_eq!(
        cols80[0].split_whitespace().collect::<Vec<_>>(),
        vec!["alpha", "longtag-xyz", "v1.0", "v1.2", "v2.0"],
        "column-major fills down each column"
    );

    // NODENSE row-major at COLUMNS=80: keeps the fitted 6 columns (Git does NOT
    // recompute for row-major), filled left-to-right.
    let row80 = tag_column_lines(p, "always,row", "80");
    assert_eq!(
        entries(&row80),
        vec![6, 4],
        "row,nodense w=80 → 6 cols: {row80:?}"
    );
    assert_eq!(
        row80[0].split_whitespace().collect::<Vec<_>>(),
        vec!["alpha", "beta", "longtag-xyz", "rc-1", "v1.0", "v1.1"],
        "row-major fills left-to-right"
    );

    // Strict-`<` width boundary: 6 columns need 6*13=78, which is NOT < 78, so
    // width 78 yields 5 columns but width 79 yields 6 (row-major, no recompute).
    assert_eq!(
        entries(&tag_column_lines(p, "always,row", "78")),
        vec![5, 5]
    );
    assert_eq!(
        entries(&tag_column_lines(p, "always,row", "79")),
        vec![6, 4]
    );

    // DENSE packs more columns by sizing each to its own widest entry.
    let dense37 = tag_column_lines(p, "always,dense", "37");
    assert_eq!(
        entries(&dense37),
        vec![4, 3, 3],
        "dense w=37 → 4 cols: {dense37:?}"
    );
    // Exact padding (dense per-column widths 13/6/7): locks byte fidelity.
    assert_eq!(dense37[0], "alpha        rc-1  v1.2   v2.1");

    // `plain` is one entry per line regardless of width.
    assert_eq!(tag_column_lines(p, "plain", "200").len(), 10);
    assert_eq!(tag_column_lines(p, "always,plain", "200").len(), 10);

    // Space-separated tokens parse like comma-separated.
    assert_eq!(tag_column_lines(p, "always row", "80"), row80);

    // Later token of a kind wins: `always,never` disables columns.
    let off = tag_column_lines(p, "always,never", "80");
    assert_eq!(off.len(), 10, "never (last) disables columns: {off:?}");
}

#[test]
fn tag_column_unknown_option_is_usage_error() {
    let repo = create_committed_repo_via_cli();
    let out = run_libra_command(&["tag", "-l", "--column=always,bogus"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(129),
        "unknown column option → usage error"
    );
}

#[test]
fn tag_column_uses_display_width_for_cjk() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // Wide CJK names: "中文" is 2 chars / display width 4; "条目目录" is 4 chars /
    // display width 8. Column width is the widest DISPLAY width + 2 = 10.
    for t in ["aa", "bb", "中文", "条目目录"] {
        assert_cli_success(&run_libra_command(&["tag", t], p), t);
    }

    // COLUMNS=22: display-width sizing fits 2 columns (2*10=20 < 22). A
    // character-count implementation would size columns at 4+2=6 and wrongly fit
    // 3 columns, so this exact output locks display-width layout AND padding.
    let cols = tag_column_lines(p, "always", "22");
    assert_eq!(
        cols,
        vec![
            "aa        中文".to_string(),
            "bb        条目目录".to_string(),
        ],
        "CJK columns are laid out and padded by display width: {cols:?}"
    );

    // COLUMNS=20: the width-8 entry forces a single column (matching git).
    assert_eq!(tag_column_lines(p, "always", "20").len(), 4);
}
