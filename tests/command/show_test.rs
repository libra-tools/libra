//! Tests for the show command, verifying correct display of commits and tags.
//! Tests use CLI commands via the libra binary.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::{path::PathBuf, process::Command};

use git_internal::internal::object::{commit::Commit, tree::Tree};
use libra::{
    command::load_object,
    internal::{db::get_db_conn_instance, head::Head, model::reference},
    utils::{
        error::StableErrorCode, object_ext::TreeExt, output::OutputConfig, test::ChangeDirGuard,
    },
};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serial_test::serial;

use super::{
    assert_cli_success, create_committed_repo_via_cli, loose_object_path, parse_cli_error_stderr,
    parse_json_stdout, run_libra_command,
};

/// Initialize a temporary repository using CLI.
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

/// Configure user identity for commits using CLI.
fn configure_user_identity(temp_path: &std::path::Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["config", "user.name", "Test User"])
        .output()
        .expect("Failed to configure user.name");

    if !output.status.success() {
        panic!(
            "Failed to configure user.name: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["config", "user.email", "test@example.com"])
        .output()
        .expect("Failed to configure user.email");

    if !output.status.success() {
        panic!(
            "Failed to configure user.email: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Create a commit with a file using CLI.
fn create_commit(temp_path: &std::path::Path, filename: &str, content: &str, message: &str) {
    // Create file
    std::fs::write(temp_path.join(filename), content).expect("Failed to create file");

    // Add file
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["add", filename])
        .output()
        .expect("Failed to add file");

    if !output.status.success() {
        panic!(
            "Failed to add file: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Commit
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["commit", "-m", message, "--no-verify"])
        .output()
        .expect("Failed to commit");

    if !output.status.success() {
        panic!(
            "Failed to commit: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Create a lightweight tag using CLI.
fn create_lightweight_tag(temp_path: &std::path::Path, tag_name: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["tag", tag_name])
        .output()
        .expect("Failed to create lightweight tag");

    if !output.status.success() {
        panic!(
            "Failed to create tag: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Create an annotated tag using CLI.
fn create_annotated_tag(temp_path: &std::path::Path, tag_name: &str, message: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["tag", tag_name, "-m", message])
        .output()
        .expect("Failed to create annotated tag");

    if !output.status.success() {
        panic!(
            "Failed to create annotated tag: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn test_show_cli_badref_returns_cli_exit_code() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["show", "badref"], repo.path());
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert!(stderr.contains("fatal: bad revision 'badref'"));
    assert!(stderr.contains("Error-Code: LBR-CLI-003"));
    assert!(stderr.contains("Hint: use 'libra log --oneline' to see available commits"));
}

#[test]
fn test_show_patch_with_stat_emits_stat_then_patch() {
    // `--patch-with-stat` (Git's `-p --stat`) emits the diffstat summary line AND
    // the full patch body, with the stat appearing before the patch.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("tracked.txt"), "first\nsecond\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "tracked.txt"], p), "stage edit");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "edit tracked", "--no-verify"], p),
        "commit edit",
    );

    let output = run_libra_command(&["show", "--patch-with-stat", "HEAD"], p);
    assert_cli_success(&output, "show --patch-with-stat HEAD");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    let stat_pos = stdout
        .find("file changed")
        .expect("diffstat summary line is present");
    let patch_pos = stdout.find("@@ ").expect("patch hunk header is present");
    assert!(
        stdout.contains("diff --git"),
        "the full patch is emitted: {stdout}"
    );
    assert!(
        stat_pos < patch_pos,
        "the diffstat appears before the patch: {stdout}"
    );

    // The stat block matches `show --stat` (the flag reuses the same diffstat).
    let stat_only = run_libra_command(&["show", "--stat", "HEAD"], p);
    let stat_only_out = String::from_utf8_lossy(&stat_only.stdout);
    let summary_line = stat_only_out
        .lines()
        .find(|l| l.contains("file changed"))
        .expect("show --stat summary line");
    assert!(
        stdout.contains(summary_line),
        "--patch-with-stat reuses the --stat summary line ({summary_line:?}): {stdout}"
    );
}

#[test]
fn test_show_summary_reports_created_files() {
    // The initial commit creates `tracked.txt`, so `show --summary` on the root
    // commit (diffed against the empty tree) must list it as a create-mode entry
    // and must not emit the full patch body.
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["show", "--summary", "HEAD"], repo.path());
    assert_cli_success(&output, "show --summary HEAD");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("create mode") && stdout.contains("tracked.txt"),
        "show --summary should list the created file: {stdout}"
    );
    assert!(
        !stdout.contains("\n+") && !stdout.contains("@@ "),
        "show --summary should not print the patch body: {stdout}"
    );
}

#[test]
fn test_show_pretty_custom_format() {
    let repo = create_committed_repo_via_cli();

    // -s skips the patch so only the formatted header is emitted.
    let output = run_libra_command(
        &["show", "-s", "--pretty=format:HASH=%h SUBJECT=%s", "HEAD"],
        repo.path(),
    );
    assert_cli_success(&output, "show -s --pretty=format:...");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("HASH=") && stdout.contains("SUBJECT="),
        "custom pretty template should drive the header, got: {stdout}"
    );
    // The default "commit <40-hex>\nAuthor:" header must NOT appear under --pretty.
    assert!(
        !stdout.contains("Author:"),
        "--pretty should replace the default header, got: {stdout}"
    );
}

#[test]
fn test_show_cli_outside_repository_returns_repo_not_found() {
    let temp = tempfile::tempdir().expect("failed to create tempdir");

    let output = run_libra_command(&["show", "HEAD"], temp.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-001");
    assert_eq!(report.category, "repo");
    assert!(
        stderr.contains("fatal: not a libra repository"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn test_show_json_commit_output_includes_type_and_files() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "show", "HEAD"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "show");
    assert_eq!(json["data"]["type"], "commit");
    assert_eq!(json["data"]["subject"], "base");
    assert!(json["data"]["files"].as_array().is_some());
}

#[test]
fn test_show_quiet_suppresses_human_output() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--quiet", "show", "HEAD"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "unexpected stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[tokio::test]
#[serial]
async fn test_show_non_quiet_uses_forced_pager() {
    if cfg!(windows) {
        return;
    }

    use libra::{
        command::show::{ShowArgs, execute_safe},
        utils::{
            pager::LIBRA_PAGER_ENV,
            test::{ChangeDirGuard, ScopedEnvVar},
        },
    };

    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());
    let missing_bin_dir = tempfile::tempdir().expect("failed to create missing-bin dir");
    let _path = ScopedEnvVar::set("PATH", missing_bin_dir.path());
    let _pager = ScopedEnvVar::set(LIBRA_PAGER_ENV, "always");

    let args = ShowArgs {
        no_abbrev_commit: false,
        no_show_signature: false,
        no_expand_tabs: false,
        no_notes: false,
        no_mailmap: false,
        object: Some("HEAD".to_string()),
        no_patch: true,
        oneline: false,
        pretty: None,
        format: None,
        date: None,
        abbrev_commit: false,
        name_only: false,
        name_status: false,
        raw: false,
        stat: false,
        patch_with_stat: false,
        summary: false,
        pathspec: vec![],
    };

    let err = execute_safe(args, &OutputConfig::default())
        .await
        .expect_err("forced pager should be initialized for non-quiet show output");
    assert_eq!(err.stable_code(), StableErrorCode::IoWriteFailed);
    assert!(
        err.message().contains("failed to execute pager"),
        "unexpected pager error: {}",
        err.message()
    );
}

#[tokio::test]
#[serial]
async fn test_show_quiet_still_validates_patch_generation() {
    use libra::command::show::{ShowArgs, execute_safe};

    let repo = create_committed_repo_via_cli();

    let tracked_blob = {
        let _guard = ChangeDirGuard::new(repo.path());
        let head = Head::current_commit().await.expect("expected HEAD commit");
        let commit: Commit = load_object(&head).expect("expected HEAD commit object");
        let tree: Tree = load_object(&commit.tree_id).expect("expected HEAD tree");
        tree.get_plain_items()
            .into_iter()
            .find(|(path, _)| path == &PathBuf::from("tracked.txt"))
            .map(|(_, hash)| hash.to_string())
            .expect("expected tracked.txt blob in HEAD tree")
    };
    std::fs::remove_file(loose_object_path(repo.path(), &tracked_blob))
        .expect("failed to delete committed blob");
    std::fs::write(
        repo.path().join("tracked.txt"),
        "mutated worktree fallback\n",
    )
    .expect("failed to mutate worktree file");

    let _guard = ChangeDirGuard::new(repo.path());
    let args = ShowArgs {
        no_abbrev_commit: false,
        no_show_signature: false,
        no_expand_tabs: false,
        no_notes: false,
        no_mailmap: false,
        object: Some("HEAD".to_string()),
        no_patch: false,
        oneline: false,
        pretty: None,
        format: None,
        date: None,
        abbrev_commit: false,
        name_only: false,
        name_status: false,
        raw: false,
        stat: false,
        patch_with_stat: false,
        summary: false,
        pathspec: vec![],
    };
    let output = OutputConfig {
        quiet: true,
        ..OutputConfig::default()
    };

    let err = execute_safe(args, &output)
        .await
        .expect_err("quiet show should still validate patch generation");
    assert_eq!(err.stable_code(), StableErrorCode::RepoCorrupt);
    assert!(
        err.message().contains("failed to load blob object"),
        "unexpected error: {}",
        err.message()
    );
}

/// Quiet --stat uses tree-level comparison (same as human --stat), so missing
/// blob contents do not cause a failure — matching the human path semantics.
#[tokio::test]
#[serial]
async fn test_show_quiet_stat_succeeds_with_missing_blob_like_human_path() {
    use libra::command::show::{ShowArgs, execute_safe};

    let repo = create_committed_repo_via_cli();

    let tracked_blob = {
        let _guard = ChangeDirGuard::new(repo.path());
        let head = Head::current_commit().await.expect("expected HEAD commit");
        let commit: Commit = load_object(&head).expect("expected HEAD commit object");
        let tree: Tree = load_object(&commit.tree_id).expect("expected HEAD tree");
        tree.get_plain_items()
            .into_iter()
            .find(|(path, _)| path == &PathBuf::from("tracked.txt"))
            .map(|(_, hash)| hash.to_string())
            .expect("expected tracked.txt blob in HEAD tree")
    };
    std::fs::remove_file(loose_object_path(repo.path(), &tracked_blob))
        .expect("failed to delete committed blob");

    let _guard = ChangeDirGuard::new(repo.path());
    let args = ShowArgs {
        no_abbrev_commit: false,
        no_show_signature: false,
        no_expand_tabs: false,
        no_notes: false,
        no_mailmap: false,
        object: Some("HEAD".to_string()),
        no_patch: false,
        oneline: false,
        pretty: None,
        format: None,
        date: None,
        abbrev_commit: false,
        name_only: false,
        name_status: false,
        raw: false,
        stat: true,
        patch_with_stat: false,
        summary: false,
        pathspec: vec![],
    };
    let output = OutputConfig {
        quiet: true,
        ..OutputConfig::default()
    };

    // --stat only needs tree-level file lists, not blob contents, so quiet
    // mode should succeed even when the blob object is missing.
    execute_safe(args, &output)
        .await
        .expect("quiet show --stat should succeed with tree-only validation");
}

#[tokio::test]
#[serial]
async fn test_show_json_commit_refs_are_best_effort_on_corrupt_branch_metadata() {
    let repo = create_committed_repo_via_cli();

    let create_branch = run_libra_command(&["branch", "topic"], repo.path());
    assert!(
        create_branch.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create_branch.stderr)
    );

    let _guard = ChangeDirGuard::new(repo.path());
    let db = get_db_conn_instance().await;
    let topic = reference::Entity::find()
        .filter(reference::Column::Kind.eq(reference::ConfigKind::Branch))
        .filter(reference::Column::Name.eq("topic"))
        .filter(reference::Column::Remote.is_null())
        .one(&db)
        .await
        .unwrap()
        .expect("expected topic branch row");
    let mut topic: reference::ActiveModel = topic.into();
    topic.commit = Set(Some("not-a-valid-hash".to_string()));
    topic.update(&db).await.unwrap();

    let output = run_libra_command(&["--json", "show", "HEAD"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "show");
    assert_eq!(json["data"]["type"], "commit");
    let refs = json["data"]["refs"]
        .as_array()
        .expect("refs should be an array");
    assert!(
        refs.iter().any(|value| value == "HEAD -> main"),
        "expected HEAD ref to survive best-effort ref collection, got: {refs:?}"
    );
}

#[tokio::test]
#[serial]
async fn test_show_patch_fails_when_commit_blob_is_missing() {
    let repo = create_committed_repo_via_cli();

    let tracked_blob = {
        let _guard = ChangeDirGuard::new(repo.path());
        let head = Head::current_commit().await.expect("expected HEAD commit");
        let commit: Commit = load_object(&head).expect("expected HEAD commit object");
        let tree: Tree = load_object(&commit.tree_id).expect("expected HEAD tree");
        tree.get_plain_items()
            .into_iter()
            .find(|(path, _)| path == &PathBuf::from("tracked.txt"))
            .map(|(_, hash)| hash.to_string())
            .expect("expected tracked.txt blob in HEAD tree")
    };
    std::fs::remove_file(loose_object_path(repo.path(), &tracked_blob))
        .expect("failed to delete committed blob");
    std::fs::write(
        repo.path().join("tracked.txt"),
        "mutated worktree fallback\n",
    )
    .expect("failed to mutate worktree file");

    let output = run_libra_command(&["show", "HEAD"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert!(
        stderr.contains("failed to load blob object"),
        "expected repo corruption error, got: {stderr}"
    );
}

#[test]
fn test_show_json_annotated_tag_hash_preserves_tag_schema() {
    let repo = init_temp_repo();
    configure_user_identity(repo.path());
    create_commit(repo.path(), "tracked.txt", "tracked\n", "base");
    create_annotated_tag(repo.path(), "v1.0.0", "release notes");

    let show_ref = run_libra_command(&["show-ref", "--tags", "v1.0.0"], repo.path());
    assert!(
        show_ref.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&show_ref.stderr)
    );
    let stdout = String::from_utf8_lossy(&show_ref.stdout);
    let tag_hash = stdout
        .split_whitespace()
        .next()
        .expect("show-ref should return the tag object hash");

    let output = run_libra_command(&["--json", "show", tag_hash], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "show");
    assert_eq!(json["data"]["type"], "tag");
    assert_eq!(json["data"]["tag_name"], "v1.0.0");
    assert_eq!(json["data"]["message"], "release notes");
    assert_eq!(json["data"]["target_type"], "commit");
    assert!(json["data"]["tagger_name"].as_str().is_some());
}

#[test]
fn test_show_hex_like_tag_name_falls_back_to_ref_resolution() {
    let repo = init_temp_repo();
    configure_user_identity(repo.path());
    create_commit(repo.path(), "tracked.txt", "tracked\n", "base");

    let hex_like_tag = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    create_lightweight_tag(repo.path(), hex_like_tag);

    let human_output = run_libra_command(&["show", hex_like_tag], repo.path());
    assert!(
        human_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&human_output.stderr)
    );
    let human_stdout = String::from_utf8_lossy(&human_output.stdout);
    assert!(
        human_stdout.contains("base"),
        "expected human output to resolve the tag ref, got: {human_stdout}"
    );

    let json_output = run_libra_command(&["--json", "show", hex_like_tag], repo.path());
    assert!(
        json_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&json_output.stderr)
    );
    let json = parse_json_stdout(&json_output);
    assert_eq!(json["command"], "show");
    assert_eq!(json["data"]["type"], "commit");
    assert_eq!(json["data"]["subject"], "base");
}

#[test]
fn test_show_json_commit_output_respects_pathspec_filters() {
    let repo = create_committed_repo_via_cli();
    std::fs::write(repo.path().join("tracked.txt"), "tracked\nupdated\n")
        .expect("failed to update tracked file");
    std::fs::write(repo.path().join("other.txt"), "other\n").expect("failed to create other file");

    let add_output = run_libra_command(&["add", "tracked.txt", "other.txt"], repo.path());
    assert!(
        add_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&add_output.stderr)
    );

    let commit_output = run_libra_command(&["commit", "-m", "update", "--no-verify"], repo.path());
    assert!(
        commit_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&commit_output.stderr)
    );

    let unfiltered = run_libra_command(&["--json", "show", "HEAD"], repo.path());
    assert!(
        unfiltered.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unfiltered.stderr)
    );
    let unfiltered_json = parse_json_stdout(&unfiltered);
    assert_eq!(
        unfiltered_json["data"]["files"]
            .as_array()
            .expect("files should be an array")
            .len(),
        2
    );

    let filtered = run_libra_command(&["--json", "show", "HEAD", "tracked.txt"], repo.path());
    assert!(
        filtered.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&filtered.stderr)
    );

    let filtered_json = parse_json_stdout(&filtered);
    let files = filtered_json["data"]["files"]
        .as_array()
        .expect("files should be an array");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["path"], "tracked.txt");
    assert_eq!(files[0]["status"], "modified");
}

#[tokio::test]
#[serial]
async fn test_show_tree_output_uses_git_modes_and_types() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());
    let head = Head::current_commit().await.unwrap();
    let commit: Commit = load_object(&head).unwrap();
    let tree_hash = commit.tree_id.to_string();

    let human = run_libra_command(&["show", &tree_hash], repo.path());
    assert!(
        human.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&human.stderr)
    );
    let human_stdout = String::from_utf8_lossy(&human.stdout);
    assert!(
        human_stdout.contains("100644 blob"),
        "expected git tree mode/type in human output, got: {human_stdout}"
    );
    assert!(
        human_stdout.contains("\ttracked.txt"),
        "expected tracked entry in human output, got: {human_stdout}"
    );

    let output = run_libra_command(&["--json", "show", &tree_hash], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "show");
    assert_eq!(json["data"]["type"], "tree");
    let tracked_entry = json["data"]["entries"]
        .as_array()
        .expect("tree entries should be an array")
        .iter()
        .find(|entry| entry["name"] == "tracked.txt")
        .expect("tracked.txt should be present in tree output");
    assert_eq!(tracked_entry["mode"], "100644");
    assert_eq!(tracked_entry["object_type"], "blob");
}

/// Test that show can display a lightweight tag.
#[tokio::test]
async fn test_show_lightweight_tag() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);
    create_commit(temp_path, "file.txt", "content", "Initial commit");

    // Create a lightweight tag
    create_lightweight_tag(temp_path, "v1.0-light");

    // Show the tag via CLI
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["show", "v1.0-light", "--no-patch"])
        .output()
        .expect("Failed to execute show command");

    assert!(
        output.status.success(),
        "show command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("commit"),
        "Output should contain 'commit': {}",
        stdout
    );
    assert!(
        stdout.contains("Initial commit"),
        "Output should contain commit message: {}",
        stdout
    );
}

/// Test that show displays an annotated tag with its metadata.
#[tokio::test]
async fn test_show_annotated_tag() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);
    create_commit(temp_path, "file.txt", "content", "Initial commit");

    // Create an annotated tag with a message
    create_annotated_tag(temp_path, "v1.0-annotated", "Release v1.0.0");

    // Show the annotated tag via CLI
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["show", "v1.0-annotated", "--no-patch"])
        .output()
        .expect("Failed to execute show command");

    assert!(
        output.status.success(),
        "show command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Annotated tag should show tag info
    assert!(
        stdout.contains("tag"),
        "Output should contain 'tag': {}",
        stdout
    );
    assert!(
        stdout.contains("v1.0-annotated"),
        "Output should contain tag name: {}",
        stdout
    );
    assert!(
        stdout.contains("Release v1.0.0"),
        "Output should contain tag message: {}",
        stdout
    );
    assert!(
        stdout.contains("Test User"),
        "Output should contain tagger name: {}",
        stdout
    );
}

/// Test that show can handle multiple commits with different tags.
#[tokio::test]
async fn test_show_multiple_tags() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);
    create_commit(temp_path, "file.txt", "content v1", "Feature one");

    // Create first tag on initial commit
    create_lightweight_tag(temp_path, "v0.1.0");

    // Make second commit
    create_commit(temp_path, "file.txt", "content v2", "Feature two");

    // Create second tag on latest commit
    create_lightweight_tag(temp_path, "v0.2.0");

    // Show first tag via CLI
    let output1 = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["show", "v0.1.0", "--no-patch"])
        .output()
        .expect("Failed to execute show command");

    assert!(
        output1.status.success(),
        "show v0.1.0 failed: {}",
        String::from_utf8_lossy(&output1.stderr)
    );

    let stdout1 = String::from_utf8_lossy(&output1.stdout);
    assert!(
        stdout1.contains("Feature one"),
        "v0.1.0 should show 'Feature one': {}",
        stdout1
    );

    // Show second tag via CLI
    let output2 = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["show", "v0.2.0", "--no-patch"])
        .output()
        .expect("Failed to execute show command");

    assert!(
        output2.status.success(),
        "show v0.2.0 failed: {}",
        String::from_utf8_lossy(&output2.stderr)
    );

    let stdout2 = String::from_utf8_lossy(&output2.stdout);
    assert!(
        stdout2.contains("Feature two"),
        "v0.2.0 should show 'Feature two': {}",
        stdout2
    );
}

/// Test that show handles non-existent tags gracefully.
#[tokio::test]
async fn test_show_nonexistent_tag() {
    let temp_dir = init_temp_repo();
    let temp_path = temp_dir.path();

    configure_user_identity(temp_path);
    create_commit(temp_path, "file.txt", "content", "Initial commit");

    // Show a non-existent tag via CLI
    let output = Command::new(env!("CARGO_BIN_EXE_libra"))
        .current_dir(temp_path)
        .args(["show", "nonexistent-tag"])
        .output()
        .expect("Failed to execute show command");

    // Should fail with error
    assert!(
        !output.status.success(),
        "show command should fail for non-existent tag"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("bad revision") || stderr.contains("fatal"),
        "Error output should indicate bad revision: {}",
        stderr
    );
}

/// Test that `show::execute_safe` returns a structured `CliError` for an
/// invalid object reference when called through the API.
#[tokio::test]
#[serial]
async fn test_show_execute_safe_bad_ref_returns_cli_error() {
    use libra::{
        command::show::{ShowArgs, execute_safe},
        utils::test::{self, ChangeDirGuard},
    };
    use tempfile::tempdir;

    let temp = tempdir().expect("failed to create temp dir");
    test::setup_with_new_libra_in(temp.path()).await;
    let _guard = ChangeDirGuard::new(temp.path());

    let args = ShowArgs {
        no_abbrev_commit: false,
        no_show_signature: false,
        no_expand_tabs: false,
        no_notes: false,
        no_mailmap: false,
        object: Some("nonexistent_ref_abc123".to_string()),
        no_patch: false,
        oneline: false,
        pretty: None,
        format: None,
        date: None,
        abbrev_commit: false,
        name_only: false,
        name_status: false,
        raw: false,
        stat: false,
        patch_with_stat: false,
        summary: false,
        pathspec: vec![],
    };
    let result = execute_safe(args, &OutputConfig::default()).await;
    assert!(result.is_err(), "execute_safe should fail for bad ref");
    let err = result.unwrap_err();
    assert_eq!(
        err.exit_code(),
        129,
        "bad revision should map to the invalid-target exit code"
    );
    assert_eq!(err.stable_code().as_str(), "LBR-CLI-003");
    assert!(
        err.message().contains("bad revision") || err.message().contains("unknown revision"),
        "error should mention bad revision, got: {}",
        err.message()
    );
}

/// Test that `show::execute_safe` returns a structured `CliError` for an
/// invalid `<rev>:<path>` pattern.
#[tokio::test]
#[serial]
async fn test_show_execute_safe_bad_rev_path_returns_cli_error() {
    use libra::{
        command::show::{ShowArgs, execute_safe},
        utils::test::{self, ChangeDirGuard},
    };
    use tempfile::tempdir;

    let temp = tempdir().expect("failed to create temp dir");
    test::setup_with_new_libra_in(temp.path()).await;
    let _guard = ChangeDirGuard::new(temp.path());

    let args = ShowArgs {
        no_abbrev_commit: false,
        no_show_signature: false,
        no_expand_tabs: false,
        no_notes: false,
        no_mailmap: false,
        object: Some("HEAD:nonexistent_file.txt".to_string()),
        no_patch: false,
        oneline: false,
        pretty: None,
        format: None,
        date: None,
        abbrev_commit: false,
        name_only: false,
        name_status: false,
        raw: false,
        stat: false,
        patch_with_stat: false,
        summary: false,
        pathspec: vec![],
    };
    let result = execute_safe(args, &OutputConfig::default()).await;
    assert!(result.is_err(), "execute_safe should fail for bad rev:path");
}

#[test]
fn test_show_machine_output_is_single_line_json() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--machine", "show", "HEAD"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let non_empty_lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        non_empty_lines.len(),
        1,
        "machine output should be exactly one non-empty line, got: {stdout}"
    );

    let parsed: serde_json::Value =
        serde_json::from_str(non_empty_lines[0]).expect("machine output should be valid JSON");
    assert_eq!(parsed["command"], "show");
    assert_eq!(parsed["data"]["type"], "commit");
}

#[tokio::test]
#[serial]
async fn test_show_json_blob_output_includes_content() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());
    let head = Head::current_commit().await.unwrap();
    let commit: Commit = load_object(&head).unwrap();
    let tree: Tree = load_object(&commit.tree_id).unwrap();
    let blob_hash = tree
        .get_plain_items()
        .into_iter()
        .find(|(path, _)| path == &PathBuf::from("tracked.txt"))
        .map(|(_, hash)| hash.to_string())
        .expect("expected tracked.txt blob");

    let output = run_libra_command(&["--json", "show", &blob_hash], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "show");
    assert_eq!(json["data"]["type"], "blob");
    assert!(!json["data"]["is_binary"].as_bool().unwrap());
    assert!(
        json["data"]["content"].as_str().is_some(),
        "text blob should have content"
    );
    assert!(json["data"]["size"].as_u64().unwrap() > 0);
}

#[test]
fn test_show_json_bad_revision_returns_error() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "show", "nonexistent_ref"], repo.path());
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(129));

    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert_eq!(report.exit_code, 129);
    assert!(report.message.contains("bad revision"));
}

#[test]
fn test_show_json_lightweight_tag_resolves_to_commit() {
    let repo = init_temp_repo();
    configure_user_identity(repo.path());
    create_commit(repo.path(), "file.txt", "content\n", "Initial commit");
    create_lightweight_tag(repo.path(), "v0.1");

    let output = run_libra_command(&["--json", "show", "v0.1"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["type"], "commit");
    assert_eq!(json["data"]["subject"], "Initial commit");
}

#[test]
fn test_show_name_status_reports_modified() {
    let repo = create_committed_repo_via_cli();
    std::fs::write(repo.path().join("tracked.txt"), "tracked\nmore\n").unwrap();
    let add = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert!(
        add.status.success(),
        "add: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    let commit = run_libra_command(&["commit", "-m", "modify", "--no-verify"], repo.path());
    assert!(
        commit.status.success(),
        "commit: {}",
        String::from_utf8_lossy(&commit.stderr)
    );
    let out = run_libra_command(&["show", "--name-status", "HEAD"], repo.path());
    assert!(
        out.status.success(),
        "show: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("M\ttracked.txt"),
        "expected 'M<tab>tracked.txt', got: {stdout}"
    );
}

#[test]
fn test_show_name_status_reports_added() {
    let repo = create_committed_repo_via_cli();
    let out = run_libra_command(&["show", "--name-status", "HEAD"], repo.path());
    assert!(
        out.status.success(),
        "show: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("A\ttracked.txt"),
        "base commit adds tracked.txt with status A, got: {stdout}"
    );
}

#[test]
fn show_format_aliases_pretty_and_abbrev_commit_shortens_hash() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let full_hash = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();

    // --format aliases --pretty.
    let fmt = run_libra_command(&["show", "--no-patch", "--format=%s", "HEAD"], p);
    assert_cli_success(&fmt, "show --format=%s");
    let pretty = run_libra_command(&["show", "--no-patch", "--pretty=%s", "HEAD"], p);
    assert_cli_success(&pretty, "show --pretty=%s");
    assert_eq!(
        String::from_utf8_lossy(&fmt.stdout),
        String::from_utf8_lossy(&pretty.stdout),
        "--format must alias --pretty"
    );
    assert!(
        String::from_utf8_lossy(&fmt.stdout).contains("base"),
        "subject rendered: {}",
        String::from_utf8_lossy(&fmt.stdout)
    );
    // --format conflicts with --pretty.
    let both = run_libra_command(&["show", "--format=%s", "--pretty=%s", "HEAD"], p);
    assert!(!both.status.success(), "--format conflicts with --pretty");

    // --abbrev-commit shortens the default header's commit hash.
    let abbrev = run_libra_command(&["show", "--no-patch", "--abbrev-commit", "HEAD"], p);
    assert_cli_success(&abbrev, "show --abbrev-commit");
    let abbrev_s = String::from_utf8_lossy(&abbrev.stdout).into_owned();
    assert!(
        abbrev_s.contains(&format!("commit {}", &full_hash[..7])),
        "abbreviated header present: {abbrev_s:?}"
    );
    assert!(
        !abbrev_s.contains(&full_hash),
        "full 40-char hash must not appear: {abbrev_s:?}"
    );

    // `--no-abbrev-commit` shows the full hash (the default); `--abbrev-commit
    // --no-abbrev-commit` (last wins) countermands `--abbrev-commit`, so the
    // full 40-char hash appears.
    let full = run_libra_command(&["show", "--no-patch", "--no-abbrev-commit", "HEAD"], p);
    assert_cli_success(&full, "show --no-abbrev-commit");
    assert!(
        String::from_utf8_lossy(&full.stdout).contains(&format!("commit {full_hash}")),
        "full hash header present with --no-abbrev-commit"
    );
    let override_full = run_libra_command(
        &[
            "show",
            "--no-patch",
            "--abbrev-commit",
            "--no-abbrev-commit",
            "HEAD",
        ],
        p,
    );
    assert_cli_success(&override_full, "show --abbrev-commit --no-abbrev-commit");
    assert!(
        String::from_utf8_lossy(&override_full.stdout).contains(&format!("commit {full_hash}")),
        "--no-abbrev-commit countermands --abbrev-commit (full hash)"
    );
}

#[test]
fn show_log_display_no_op_flags_are_accepted() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let plain = run_libra_command(&["show"], p);
    assert_cli_success(&plain, "show");
    let plain_hdr = String::from_utf8_lossy(&plain.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    assert!(
        plain_hdr.starts_with("commit "),
        "show prints a commit header"
    );

    // `--no-expand-tabs`/`--no-notes`/`--no-mailmap`/`--no-show-signature`
    // (Git's log/show display options) are accepted no-ops: Libra's show expands
    // no tabs, displays no notes inline, applies no mailmap, and never displays
    // commit signatures inline. They are parsed-but-unread, so each still
    // produces the same commit header. (The per-file diff ordering of `show`
    // itself is not stable across invocations, so only the header is compared.)
    for flag in [
        "--no-expand-tabs",
        "--no-notes",
        "--no-mailmap",
        "--no-show-signature",
    ] {
        let out = run_libra_command(&["show", flag], p);
        assert_cli_success(&out, &format!("show {flag}"));
        let hdr = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        assert_eq!(hdr, plain_hdr, "show {flag} prints the same commit header");
    }
}

/// `show --raw` renders the raw diff format (`:<old-mode> <new-mode> <old-sha>
/// <new-sha> <status>\t<path>`) instead of a patch, matching `git show --raw`.
#[test]
fn test_show_raw_diff_format() {
    use std::{fs, os::unix::fs::PermissionsExt};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // First commit: a regular file `f` and a file `d` to be deleted later.
    fs::write(p.join("f"), "one\n").expect("write f");
    fs::write(p.join("d"), "old\n").expect("write d");
    assert_cli_success(&run_libra_command(&["add", "f", "d"], p), "add f d");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );

    // Second commit: modify `f`, add an executable `g`, delete `d`.
    fs::write(p.join("f"), "two\n").expect("modify f");
    fs::write(p.join("g"), "new\n").expect("write g");
    fs::set_permissions(p.join("g"), fs::Permissions::from_mode(0o755)).expect("chmod g");
    fs::remove_file(p.join("d")).expect("rm d");
    assert_cli_success(
        &run_libra_command(&["add", "f", "g", "d"], p),
        "stage changes",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );

    let out = run_libra_command(&["show", "--raw", "HEAD"], p);
    assert_cli_success(&out, "show --raw");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let raw: Vec<&str> = stdout.lines().filter(|l| l.starts_with(':')).collect();

    // Modified regular file: both modes 100644, both ids present, status M.
    let modified = raw
        .iter()
        .find(|l| l.ends_with("M\tf"))
        .unwrap_or_else(|| panic!("modified line for f: {stdout}"));
    assert!(
        modified.starts_with(":100644 100644 "),
        "modified f modes: {modified}"
    );

    // Added executable: old side zeroed, new mode 100755, status A.
    let added = raw
        .iter()
        .find(|l| l.ends_with("A\tg"))
        .unwrap_or_else(|| panic!("added line for g: {stdout}"));
    assert!(
        added.starts_with(":000000 100755 0000000 "),
        "added g modes/old-zero: {added}"
    );

    // Deleted file: old mode 100644, new side zeroed, status D.
    let deleted = raw
        .iter()
        .find(|l| l.ends_with("D\td"))
        .unwrap_or_else(|| panic!("deleted line for d: {stdout}"));
    assert!(
        deleted.starts_with(":100644 000000 "),
        "deleted d modes: {deleted}"
    );
    assert!(
        deleted.contains(" 0000000 D\td"),
        "deleted d new-id zeroed: {deleted}"
    );

    // The commit header/message still precede the raw lines (no patch body).
    assert!(
        stdout.contains("commit "),
        "raw still shows the commit header"
    );
    assert!(
        !stdout.contains("diff --git"),
        "--raw replaces the patch: {stdout}"
    );
}
