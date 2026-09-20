//! Tests reset command modes (soft/mixed/hard) and resulting state changes.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
use libra::utils::error::StableErrorCode;
use libra::{
    command::{
        branch::{self, BranchArgs},
        remove::{self, RemoveArgs},
        reset::{self, ResetArgs},
        status::{changes_to_be_committed, changes_to_be_staged},
    },
    internal::{
        branch::{Branch as InternalBranch, TRACES_BRANCH},
        config::ConfigKv,
    },
    utils::test::setup_with_new_libra_in,
};

use super::*;

async fn setup_reset_user_identity() {
    ConfigKv::set("user.name", "Test User", false)
        .await
        .unwrap();
    ConfigKv::set("user.email", "test@example.com", false)
        .await
        .unwrap();
}

#[test]
fn test_reset_cli_outside_repository_returns_fatal_128() {
    let temp = tempdir().unwrap();
    let output = run_libra_command(&["reset", "HEAD"], temp.path());
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal: not a libra repository"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn test_reset_unborn_head_returns_repo_state_error() {
    fn copy_dir_recursive(from: &std::path::Path, to: &std::path::Path) {
        fs::create_dir_all(to).expect("failed to create destination directory");
        for entry in fs::read_dir(from).expect("failed to read source directory") {
            let entry = entry.expect("failed to read source entry");
            let source_path = entry.path();
            let destination_path = to.join(entry.file_name());
            if entry
                .file_type()
                .expect("failed to read source file type")
                .is_dir()
            {
                copy_dir_recursive(&source_path, &destination_path);
            } else {
                fs::copy(&source_path, &destination_path)
                    .expect("failed to copy object into unborn repository");
            }
        }
    }

    let source_repo = create_committed_repo_via_cli();
    let source_head = run_libra_command(&["show-ref", "--heads", "main"], source_repo.path());
    assert_cli_success(&source_head, "show-ref --heads main");
    let commit_hash = String::from_utf8_lossy(&source_head.stdout)
        .split_whitespace()
        .next()
        .expect("show-ref should return the main commit hash")
        .to_string();

    let target_repo = tempdir().unwrap();
    init_repo_via_cli(target_repo.path());
    copy_dir_recursive(
        &source_repo.path().join(".libra/objects"),
        &target_repo.path().join(".libra/objects"),
    );

    let output = run_libra_command(&["reset", "--hard", &commit_hash], target_repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-003");
    assert!(
        stderr.contains("HEAD is unborn"),
        "expected unborn HEAD message, got: {stderr}"
    );
}

#[test]
fn test_reset_json_output_reports_target_commit() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\nsecond\n").unwrap();
    let add_output = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert!(
        add_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&add_output.stderr)
    );
    let commit_output = run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path());
    assert!(
        commit_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&commit_output.stderr)
    );

    let output = run_libra_command(&["--json", "reset", "--hard", "HEAD~1"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "reset");
    assert_eq!(json["data"]["mode"], "hard");
    assert_eq!(json["data"]["subject"], "base");
    assert_eq!(json["data"]["files_restored"], 1);
}

#[test]
fn test_reset_json_hard_head_clean_repo_reports_zero_restores() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "reset", "--hard", "HEAD"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["mode"], "hard");
    assert_eq!(json["data"]["files_restored"], 0);
}

#[test]
fn test_reset_json_hard_head_reports_actual_restored_files() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\nupdated\n").unwrap();

    let output = run_libra_command(&["--json", "reset", "--hard", "HEAD"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["mode"], "hard");
    assert_eq!(json["data"]["files_restored"], 1);
    assert_eq!(
        fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
        "tracked\n"
    );
}

#[test]
fn test_reset_hard_with_pathspec_returns_usage_error() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\nupdated\n").unwrap();

    let output = run_libra_command(
        &["reset", "--hard", "HEAD", "--", "tracked.txt"],
        repo.path(),
    );
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        stderr.contains("Cannot do hard reset with paths."),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn test_reset_soft_with_pathspec_returns_usage_error() {
    // PathspecWithSoft is documented in docs/development/commands/reset.md and mapped
    // to CliInvalidArguments (LBR-CLI-002, exit 129). The --hard variant
    // already has coverage above; this pins the --soft side too.
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\nupdated\n").unwrap();

    let output = run_libra_command(
        &["reset", "--soft", "HEAD", "--", "tracked.txt"],
        repo.path(),
    );
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        stderr.contains("is not compatible with --soft reset")
            || stderr.contains("--soft only moves HEAD"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn test_reset_onto_locked_branch_rejects_intent() {
    // Libra refuses to `reset` onto its managed locked branches
    // (`main`, `intent`, `traces`) — see
    // src/internal/branch.rs::is_locked_branch and the early guard in
    // src/command/reset.rs::run_reset. Locked-target rejection maps to
    // CliInvalidTarget (LBR-CLI-003, exit 129); no integration test
    // exercised it before this patch, so a regression that removed the
    // guard could have shipped silently.
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["reset", "intent"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        stderr.contains("intent"),
        "expected the locked branch name in the message, got: {stderr}"
    );
}

/// opencode.md OC-Phase 3 acceptance criterion 5 requires that
/// `reset` refuse to land user work on `traces`, the same way
/// the existing `intent` guard does. Functionally
/// `is_locked_revision` already covers both branches, but missing
/// integration coverage means a regression that pulled
/// `TRACES_BRANCH` out of `is_locked_branch` would ship
/// silently. This test pins the contract end-to-end.
#[test]
fn test_reset_onto_locked_branch_rejects_traces() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["reset", "traces"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        stderr.contains("traces"),
        "expected the traces branch name in the message, got: {stderr}"
    );
}

/// Revision suffixes (`traces~1`, `traces^`) must also
/// be refused. `is_locked_revision` strips revision modifiers
/// before checking the locked list; without this regression a
/// user-typed `reset traces~2` would escape the guard.
#[test]
fn test_reset_onto_locked_branch_rejects_traces_suffix() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["reset", "traces~1"], repo.path());
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-003");
}

#[tokio::test]
#[serial(cwd)]
async fn test_reset_refuses_ai_managed_current_branch() {
    let repo = create_committed_repo_via_cli();
    {
        let _guard = ChangeDirGuard::new(repo.path());
        Head::update_result(Head::Branch(TRACES_BRANCH.to_string()), None)
            .await
            .expect("point HEAD at traces");
    }

    let output = run_libra_command(&["reset", "--hard", "HEAD"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(
        stderr.contains("refusing to reset locked current branch 'traces'"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn test_reset_json_pathspec_omits_previous_commit() {
    // Pathspec resets do not move HEAD, so the JSON schema documented in
    // docs/commands/reset.md (line 130: "previous_commit is null for
    // pathspec-only resets") must emit `null`. Code historically captured
    // current HEAD into this field even for pathspec resets, contradicting
    // the user contract; this test pins the documented behavior.
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\nupdated\n").unwrap();
    let add_output = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert!(
        add_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&add_output.stderr)
    );

    let output = run_libra_command(
        &["--json", "reset", "HEAD", "--", "tracked.txt"],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "reset");
    assert_eq!(json["data"]["mode"], "mixed");
    assert!(
        json["data"]["previous_commit"].is_null(),
        "pathspec resets must emit previous_commit=null, got: {}",
        json["data"]["previous_commit"]
    );
    assert_eq!(json["data"]["files_unstaged"], 1);
    assert_eq!(json["data"]["files_restored"], 0);
    let pathspecs = json["data"]["pathspecs"]
        .as_array()
        .expect("pathspecs should be an array");
    assert_eq!(pathspecs.len(), 1);
    assert_eq!(pathspecs[0], "tracked.txt");
}

#[test]
fn reset_bare_pathspec_unstages_file_like_git() {
    // Given: a tracked file is modified and staged.
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\nupdated\n").unwrap();
    let add_output = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert_cli_success(&add_output, "stage tracked.txt");

    // When: reset receives the file as a bare positional, matching `git reset <path>`.
    let output = run_libra_command(&["reset", "tracked.txt"], repo.path());

    // Then: the file is unstaged but the worktree modification remains.
    assert_cli_success(&output, "reset tracked.txt");
    let status = run_libra_command(&["status", "--short"], repo.path());
    assert_cli_success(&status, "status --short");
    assert_eq!(String::from_utf8_lossy(&status.stdout), " M tracked.txt\n");
}

#[test]
fn reset_double_dash_pathspec_unstages_file_named_like_revision() {
    // Given: a tracked file is literally named HEAD and has staged content.
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("HEAD"), "head file\n").unwrap();
    let add = run_libra_command(&["add", "HEAD"], repo.path());
    assert_cli_success(&add, "add HEAD file");
    let commit = run_libra_command(
        &["commit", "-m", "add head file", "--no-verify"],
        repo.path(),
    );
    assert_cli_success(&commit, "commit HEAD file");
    fs::write(repo.path().join("HEAD"), "head file\nupdated\n").unwrap();
    let add = run_libra_command(&["add", "HEAD"], repo.path());
    assert_cli_success(&add, "stage HEAD file update");

    // When: `--` explicitly marks the following token as a pathspec.
    let output = run_libra_command(&["reset", "--", "HEAD"], repo.path());

    // Then: the revision-like filename is treated as a path and unstaged.
    assert_cli_success(&output, "reset -- HEAD");
    let status = run_libra_command(&["status", "--short"], repo.path());
    assert_cli_success(&status, "status --short");
    assert_eq!(String::from_utf8_lossy(&status.stdout), " M HEAD\n");
}

#[test]
fn reset_double_dash_preserves_dash_prefixed_pathspecs() {
    // Given: tracked files have names that would otherwise parse as flags.
    let repo = create_committed_repo_via_cli();
    for name in ["-h", "--help"] {
        fs::write(repo.path().join(name), format!("{name}\n")).unwrap();
        let add = run_libra_command(&["add", "--", name], repo.path());
        assert_cli_success(&add, "add dash-prefixed file");
    }
    let commit = run_libra_command(
        &["commit", "-m", "add dash-prefixed files", "--no-verify"],
        repo.path(),
    );
    assert_cli_success(&commit, "commit dash-prefixed files");

    for name in ["-h", "--help"] {
        fs::write(repo.path().join(name), format!("{name}\nupdated\n")).unwrap();
        let add = run_libra_command(&["add", "--", name], repo.path());
        assert_cli_success(&add, "stage dash-prefixed file update");

        // When: `--` marks the dash-prefixed token as a pathspec.
        let output = run_libra_command(&["reset", "--", name], repo.path());

        // Then: reset treats the token as a filename instead of a reset/help flag.
        assert_cli_success(&output, "reset dash-prefixed pathspec");
        let status = run_libra_command(&["status", "--short"], repo.path());
        assert_cli_success(&status, "status --short");
        let stdout = String::from_utf8_lossy(&status.stdout);
        assert!(
            stdout.contains(&format!(" M {name}\n")),
            "expected {name} to be unstaged, got: {stdout}",
        );
    }
}

#[test]
fn reset_bare_revision_path_ambiguity_errors_like_git() {
    // Given: a token names both a branch and a tracked path.
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("feature"), "branch/path collision\n").unwrap();
    let add = run_libra_command(&["add", "feature"], repo.path());
    assert_cli_success(&add, "add feature file");
    let commit = run_libra_command(
        &["commit", "-m", "add feature file", "--no-verify"],
        repo.path(),
    );
    assert_cli_success(&commit, "commit feature file");
    let branch = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&branch, "create feature branch");
    fs::write(
        repo.path().join("feature"),
        "branch/path collision\nupdated\n",
    )
    .unwrap();
    let add = run_libra_command(&["add", "feature"], repo.path());
    assert_cli_success(&add, "stage feature file update");

    // When: reset receives the ambiguous token without `--`.
    let output = run_libra_command(&["reset", "feature"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    // Then: it refuses to guess between revision and pathspec.
    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        stderr.contains("ambiguous argument 'feature': both revision and filename"),
        "unexpected stderr: {stderr}"
    );
    let status = run_libra_command(&["status", "--short"], repo.path());
    assert_cli_success(&status, "status --short");
    assert_eq!(String::from_utf8_lossy(&status.stdout), "M  feature\n");
}

#[test]
fn reset_soft_revision_does_not_probe_index_for_pathspec_disambiguation() {
    // Given: a normal repository whose index is unreadable.
    let repo = create_committed_repo_via_cli();
    fs::write(
        repo.path().join(".libra").join("index"),
        b"corrupted-index-data",
    )
    .unwrap();

    // When: reset receives an unambiguous revision target.
    let output = run_libra_command(&["reset", "--soft", "HEAD"], repo.path());

    // Then: soft reset resolves the revision without probing pathspec state.
    assert_cli_success(&output, "reset --soft HEAD with corrupt index");
}

#[test]
fn test_reset_json_hard_with_pathspec_returns_usage_error() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\nupdated\n").unwrap();

    let output = run_libra_command(
        &["--json", "reset", "--hard", "HEAD", "--", "tracked.txt"],
        repo.path(),
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("expected stderr JSON in --json mode");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert!(
        output.stdout.is_empty(),
        "stdout should stay empty on JSON error"
    );
    assert_eq!(report["error_code"], "LBR-CLI-002");
    assert!(
        stderr.contains("Cannot do hard reset with paths."),
        "unexpected stderr: {stderr}"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_reset_corrupt_head_reference_returns_repo_corrupt() {
    let repo = create_committed_repo_via_cli();
    let target_commit = {
        let _guard = ChangeDirGuard::new(repo.path());
        // Migrated from lossy `InternalBranch::find_branch` per docs/development/commands/branch.md —
        // storage errors no longer collapse into "main branch should exist".
        InternalBranch::find_branch_result("main", None)
            .await
            .expect("failed to query main branch")
            .expect("main branch should exist")
            .commit
            .to_string()
    };
    {
        let _guard = ChangeDirGuard::new(repo.path());
        InternalBranch::update_branch("main", "not-a-valid-hash", None)
            .await
            .unwrap();
    }

    let output = run_libra_command(&["reset", &target_commit], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert!(
        stderr.contains("stored HEAD reference is corrupt"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        stderr.contains("stored branch reference 'main' is corrupt"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        !stderr.contains("HEAD is unborn"),
        "reset should not misreport corrupt HEAD as unborn: {stderr}"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_reset_corrupt_target_branch_returns_repo_corrupt() {
    // Use a non-locked branch as the target. Libra now refuses to `reset`
    // onto locked branches (`main`, `intent`, `traces`) — see
    // `src/internal/branch.rs::is_locked_branch` and the early check in
    // `src/command/reset.rs::run_reset` — so corrupting `main` and then
    // calling `reset main` short-circuits at the locked-target guard with
    // `LBR-CLI-003` instead of reaching the corrupt-branch resolution we
    // want this test to exercise. Creating a `feature-corrupt-target`
    // branch (locked-list check does not match it) and corrupting that
    // branch's commit lets `reset` reach the
    // `CommitBaseError::CorruptReference` → `ResetError::RevisionCorrupt`
    // mapping.
    const TARGET_BRANCH: &str = "feature-corrupt-target";

    let repo = create_committed_repo_via_cli();
    {
        let _guard = ChangeDirGuard::new(repo.path());
        // Seed the branch with the current main tip so reset would
        // otherwise be a no-op happy path, then corrupt it.
        InternalBranch::update_branch(TARGET_BRANCH, "not-a-valid-hash", None)
            .await
            .unwrap();
    }

    let output = run_libra_command(&["reset", TARGET_BRANCH], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-002");
    let resolve_msg = format!("failed to resolve branch '{TARGET_BRANCH}'");
    let corrupt_msg = format!("stored branch reference '{TARGET_BRANCH}' is corrupt");
    assert!(stderr.contains(&resolve_msg), "unexpected stderr: {stderr}");
    assert!(stderr.contains(&corrupt_msg), "unexpected stderr: {stderr}");
    assert!(
        !stderr.contains("invalid reference"),
        "reset should not misclassify corrupt branch storage as invalid target: {stderr}"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_reset_pathspec_surfaces_subtree_corruption_as_repo_corrupt() {
    let repo = create_committed_repo_via_cli();
    fs::create_dir_all(repo.path().join("dir")).unwrap();
    fs::write(repo.path().join("dir").join("nested.txt"), "nested\n").unwrap();

    let add = run_libra_command(&["add", "dir/nested.txt"], repo.path());
    assert_cli_success(&add, "add dir/nested.txt");

    let commit = run_libra_command(&["commit", "-m", "nested", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit nested");

    {
        let _guard = ChangeDirGuard::new(repo.path());
        // Migrated from lossy `InternalBranch::find_branch` per docs/development/commands/branch.md.
        let head = InternalBranch::find_branch_result("main", None)
            .await
            .expect("failed to query main branch")
            .expect("main branch should exist")
            .commit;
        let commit: Commit = load_object(&head).expect("load HEAD commit");
        let tree: Tree = load_object(&commit.tree_id).expect("load root tree");
        let dir_item = tree
            .tree_items
            .iter()
            .find(|item| item.name == "dir")
            .expect("expected dir subtree");
        let dir_hash = dir_item.id.to_string();
        let object_path = repo
            .path()
            .join(".libra")
            .join("objects")
            .join(&dir_hash[..2])
            .join(&dir_hash[2..]);
        fs::write(object_path, b"corrupt subtree").unwrap();
    }

    let output = run_libra_command(&["reset", "HEAD", "--", "dir/nested.txt"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert!(
        stderr.contains("failed to load tree"),
        "unexpected stderr: {stderr}"
    );
}

#[cfg(unix)]
#[tokio::test]
#[serial(cwd)]
async fn test_reset_hard_io_failure_rolls_back_index_and_keeps_head() {
    if skip_permission_denied_test_if_root(
        "test_reset_hard_io_failure_rolls_back_index_and_keeps_head",
    ) {
        return;
    }

    let temp_path = tempdir().unwrap();
    let _guard = ChangeDirGuard::new(temp_path.path());
    setup_with_new_libra_in(temp_path.path()).await;
    setup_reset_user_identity().await;

    fs::write("base.txt", "base\n").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec![".libraignore".to_string(), "base.txt".to_string()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("base".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    fs::write("tracked.txt", "tracked\n").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["tracked.txt".to_string()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("add tracked".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    let head_before = Head::current_commit().await.unwrap();
    let original_mode = fs::metadata(temp_path.path()).unwrap().permissions().mode();
    fs::set_permissions(temp_path.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

    let result = reset::execute_safe(
        ResetArgs {
            target: Some("HEAD~1".to_string()),
            soft: false,
            mixed: false,
            hard: true,
            merge: false,
            keep: false,
            pathspecs: vec![],
            pathspec_separator: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            no_refresh: false,
            patch: false,
            auto_advance: false,
            no_auto_advance: false,
        },
        &libra::utils::output::OutputConfig::default(),
    )
    .await;

    fs::set_permissions(
        temp_path.path(),
        std::fs::Permissions::from_mode(original_mode),
    )
    .unwrap();

    let error = result.expect_err("hard reset should fail when tracked file removal is denied");
    assert_eq!(error.stable_code(), StableErrorCode::IoWriteFailed);
    assert_eq!(Head::current_commit().await.unwrap(), head_before);
    assert!(temp_path.path().join("tracked.txt").exists());
    assert!(
        changes_to_be_committed().await.is_empty(),
        "failed hard reset should restore the index to match HEAD"
    );
    assert!(
        changes_to_be_staged().unwrap().modified.is_empty()
            && changes_to_be_staged().unwrap().deleted.is_empty()
            && changes_to_be_staged().unwrap().new.is_empty(),
        "failed hard reset should restore the working tree to match HEAD"
    );
}

/// Setup a standard test repository with 4 commits and branches
async fn setup_standard_repo(
    temp_path: &std::path::Path,
) -> (ObjectHash, ObjectHash, ObjectHash, ObjectHash) {
    test::setup_with_new_libra_in(temp_path).await;

    fs::write("1.txt", "content 1").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["1.txt".to_string()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("commit 1: add 1.txt".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;
    let commit1 = Head::current_commit().await.unwrap();
    branch::execute(BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: Some("1".to_string()),
        commit_hash: None,
        list: false,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
        track: None,
        no_track: false,
        unset_upstream: None,
        edit_description: None,
        show_current: false,
        rename: vec![],
        copy: vec![],
        copy_force: vec![],
        remotes: false,
        all: false,
        contains: vec![],
        no_contains: vec![],
        points_at: None,
        merged: None,
        no_merged: None,
        sort: None,
        ignore_case: false,
        column: None,
        verbose: 0,
    })
    .await;

    fs::write("2.txt", "content 2").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["2.txt".to_string()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("commit 2: add 2.txt".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;
    let commit2 = Head::current_commit().await.unwrap();
    branch::execute(BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: Some("2".to_string()),
        commit_hash: None,
        list: false,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
        track: None,
        no_track: false,
        unset_upstream: None,
        edit_description: None,
        show_current: false,
        rename: vec![],
        copy: vec![],
        copy_force: vec![],
        remotes: false,
        all: false,
        contains: vec![],
        no_contains: vec![],
        points_at: None,
        merged: None,
        no_merged: None,
        sort: None,
        ignore_case: false,
        column: None,
        verbose: 0,
    })
    .await;

    fs::write("3.txt", "content 3").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["3.txt".to_string()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("commit 3: add 3.txt".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;
    let commit3 = Head::current_commit().await.unwrap();
    branch::execute(BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: Some("3".to_string()),
        commit_hash: None,
        list: false,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
        track: None,
        no_track: false,
        unset_upstream: None,
        edit_description: None,
        show_current: false,
        rename: vec![],
        copy: vec![],
        copy_force: vec![],
        remotes: false,
        all: false,
        contains: vec![],
        no_contains: vec![],
        points_at: None,
        merged: None,
        no_merged: None,
        sort: None,
        ignore_case: false,
        column: None,
        verbose: 0,
    })
    .await;

    fs::write("4.txt", "content 4").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["4.txt".to_string()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("commit 4: add 4.txt".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;
    let commit4 = Head::current_commit().await.unwrap();
    branch::execute(BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: Some("4".to_string()),
        commit_hash: None,
        list: false,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
        track: None,
        no_track: false,
        unset_upstream: None,
        edit_description: None,
        show_current: false,
        rename: vec![],
        copy: vec![],
        copy_force: vec![],
        remotes: false,
        all: false,
        contains: vec![],
        no_contains: vec![],
        points_at: None,
        merged: None,
        no_merged: None,
        sort: None,
        ignore_case: false,
        column: None,
        verbose: 0,
    })
    .await;

    (commit1, commit2, commit3, commit4)
}

/// Setup the standard test state: modify files and stage some changes
async fn setup_test_state() {
    fs::write("3.txt", "content 3\nnew line").unwrap();
    fs::write("4.txt", "content 4\nnew line").unwrap();

    fs::write("5.txt", "new line").unwrap();

    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["3.txt".to_string()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;
}

#[tokio::test]
#[serial(cwd)]
/// Tests soft reset: only moves HEAD pointer, preserves index and working directory
async fn test_reset_soft() {
    let temp_path = tempdir().unwrap();
    let _guard = ChangeDirGuard::new(temp_path.path());

    let (commit1, _, _, _) = setup_standard_repo(temp_path.path()).await;
    setup_test_state().await;

    // Perform soft reset to commit 1
    reset::execute(ResetArgs {
        target: Some("1".to_string()), // Reset to branch 1
        soft: true,
        mixed: false,
        hard: false,
        merge: false,
        keep: false,
        pathspecs: vec![],
        pathspec_separator: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        no_refresh: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    // Verify HEAD moved to commit 1
    let current_commit = Head::current_commit().await.unwrap();
    assert_eq!(current_commit, commit1);

    // Verify all files still exist in working directory
    assert!(fs::metadata("1.txt").is_ok());
    assert!(fs::metadata("2.txt").is_ok());
    assert!(fs::metadata("3.txt").is_ok());
    assert!(fs::metadata("4.txt").is_ok());
    assert!(fs::metadata("5.txt").is_ok());

    // Verify file contents are preserved (including modifications)
    assert_eq!(fs::read_to_string("3.txt").unwrap(), "content 3\nnew line");
    assert_eq!(fs::read_to_string("4.txt").unwrap(), "content 4\nnew line");
    assert_eq!(fs::read_to_string("5.txt").unwrap(), "new line");

    // Verify index still has staged changes (3.txt should be staged)
    let staged = libra::command::status::changes_to_be_committed().await;
    assert!(
        !staged.is_empty(),
        "Staged changes should be preserved in soft reset"
    );
}

#[tokio::test]
#[serial(cwd)]
/// Tests mixed reset: moves HEAD and resets index, preserves working directory
async fn test_reset_mixed() {
    let temp_path = tempdir().unwrap();
    let _guard = ChangeDirGuard::new(temp_path.path());

    let (commit1, _, _, _) = setup_standard_repo(temp_path.path()).await;
    setup_test_state().await;

    // Perform mixed reset (default) to commit 1
    reset::execute(ResetArgs {
        target: Some("1".to_string()), // Reset to branch 1
        soft: false,
        mixed: false, // false means default (mixed)
        hard: false,
        merge: false,
        keep: false,
        pathspecs: vec![],
        pathspec_separator: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        no_refresh: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    // Verify HEAD moved to commit 1
    let current_commit = Head::current_commit().await.unwrap();
    assert_eq!(current_commit, commit1);

    // Verify all files still exist in working directory
    assert!(fs::metadata("1.txt").is_ok());
    assert!(fs::metadata("2.txt").is_ok());
    assert!(fs::metadata("3.txt").is_ok());
    assert!(fs::metadata("4.txt").is_ok());
    assert!(fs::metadata("5.txt").is_ok());

    // Verify file contents are preserved (including modifications)
    assert_eq!(fs::read_to_string("3.txt").unwrap(), "content 3\nnew line");
    assert_eq!(fs::read_to_string("4.txt").unwrap(), "content 4\nnew line");
    assert_eq!(fs::read_to_string("5.txt").unwrap(), "new line");

    // Verify index was reset (no staged changes)
    let staged = libra::command::status::changes_to_be_committed().await;
    assert!(staged.is_empty(), "Index should be reset in mixed reset");

    // Verify unstaged changes exist (2.txt, 3.txt, 4.txt should be untracked/modified)
    let unstaged = changes_to_be_staged().unwrap();
    assert!(
        !unstaged.new.is_empty() || !unstaged.modified.is_empty(),
        "Should have unstaged changes after mixed reset"
    );
}

#[tokio::test]
#[serial(cwd)]
/// Tests hard reset: moves HEAD, resets index and working directory
async fn test_reset_hard() {
    let temp_path = tempdir().unwrap();
    let _guard = ChangeDirGuard::new(temp_path.path());

    let (commit1, _, _, _) = setup_standard_repo(temp_path.path()).await;
    setup_test_state().await;

    // Perform hard reset to commit 1
    reset::execute(ResetArgs {
        target: Some("1".to_string()), // Reset to branch 1
        soft: false,
        mixed: false,
        hard: true,
        merge: false,
        keep: false,
        pathspecs: vec![],
        pathspec_separator: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        no_refresh: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    // Verify HEAD moved to commit 1
    let current_commit = Head::current_commit().await.unwrap();
    assert_eq!(current_commit, commit1);

    // Verify working directory was reset - only 1.txt should exist from commit 1
    assert!(fs::metadata("1.txt").is_ok());
    assert!(
        fs::metadata("2.txt").is_err(),
        "2.txt should be removed by hard reset"
    );
    assert!(
        fs::metadata("3.txt").is_err(),
        "3.txt should be removed by hard reset"
    );
    assert!(
        fs::metadata("4.txt").is_err(),
        "4.txt should be removed by hard reset"
    );

    // Untracked files should remain
    assert!(
        fs::metadata("5.txt").is_ok(),
        "Untracked files should remain after hard reset"
    );

    // Verify file content was restored to commit 1 state
    assert_eq!(fs::read_to_string("1.txt").unwrap(), "content 1");
    assert_eq!(fs::read_to_string("5.txt").unwrap(), "new line");

    // Verify index was reset
    let staged = libra::command::status::changes_to_be_committed().await;
    assert!(staged.is_empty(), "Index should be reset in hard reset");

    // Verify only untracked files remain
    let unstaged = changes_to_be_staged().unwrap();
    assert!(
        !unstaged.new.is_empty(),
        "Should have untracked files (5.txt)"
    );
    assert!(
        unstaged.modified.is_empty(),
        "Should have no modified files"
    );
    assert!(unstaged.deleted.is_empty(), "Should have no deleted files");
}

#[tokio::test]
#[serial(cwd)]
async fn test_reset_mixed_same_target_resets_index_without_moving_head() {
    let temp_path = tempdir().unwrap();
    let _guard = ChangeDirGuard::new(temp_path.path());
    setup_with_new_libra_in(temp_path.path()).await;
    setup_reset_user_identity().await;

    fs::write("tracked.txt", "tracked\n").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["tracked.txt".to_string()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("base".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;
    let head_before = Head::current_commit().await.unwrap();

    fs::write("tracked.txt", "tracked\nstaged\n").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["tracked.txt".to_string()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    reset::execute_safe(
        ResetArgs {
            target: Some("HEAD".to_string()),
            soft: false,
            mixed: true,
            hard: false,
            merge: false,
            keep: false,
            pathspecs: vec![],
            pathspec_separator: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            no_refresh: false,
            patch: false,
            auto_advance: false,
            no_auto_advance: false,
        },
        &libra::utils::output::OutputConfig::default(),
    )
    .await
    .expect("mixed reset to HEAD should succeed");

    assert_eq!(Head::current_commit().await.unwrap(), head_before);
    assert!(
        changes_to_be_committed().await.is_empty(),
        "mixed reset to HEAD should unstage tracked changes"
    );
    let unstaged = changes_to_be_staged().unwrap();
    assert!(
        unstaged
            .modified
            .iter()
            .any(|path| path.file_name().and_then(|name| name.to_str()) == Some("tracked.txt")),
        "tracked.txt should remain modified in the worktree after mixed reset"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_reset_hard_same_target_restores_worktree_and_removes_staged_additions() {
    let temp_path = tempdir().unwrap();
    let _guard = ChangeDirGuard::new(temp_path.path());
    setup_with_new_libra_in(temp_path.path()).await;
    setup_reset_user_identity().await;

    fs::write("tracked.txt", "tracked\n").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["tracked.txt".to_string()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("base".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    fs::write("tracked.txt", "tracked\nmodified\n").unwrap();
    fs::write("new.txt", "new\n").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["new.txt".to_string()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    reset::execute_safe(
        ResetArgs {
            target: Some("HEAD".to_string()),
            soft: false,
            mixed: false,
            hard: true,
            merge: false,
            keep: false,
            pathspecs: vec![],
            pathspec_separator: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            no_refresh: false,
            patch: false,
            auto_advance: false,
            no_auto_advance: false,
        },
        &libra::utils::output::OutputConfig::default(),
    )
    .await
    .expect("hard reset to HEAD should succeed");

    assert_eq!(fs::read_to_string("tracked.txt").unwrap(), "tracked\n");
    assert!(
        fs::metadata("new.txt").is_err(),
        "hard reset to HEAD should remove staged additions not present in the target tree"
    );
    assert!(
        changes_to_be_committed().await.is_empty(),
        "hard reset to HEAD should clear staged changes"
    );
    let unstaged = changes_to_be_staged().unwrap();
    assert!(
        unstaged.modified.is_empty(),
        "tracked changes should be restored"
    );
    assert!(
        unstaged.deleted.is_empty(),
        "tracked deletions should be cleared"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_reset_hard_removes_paths_tracked_only_by_head_tree() {
    let temp_path = tempdir().unwrap();
    let _guard = ChangeDirGuard::new(temp_path.path());
    setup_with_new_libra_in(temp_path.path()).await;
    setup_reset_user_identity().await;

    fs::write("base.txt", "base\n").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["base.txt".to_string()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("base".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    fs::write("tracked.txt", "tracked\n").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["tracked.txt".to_string()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("add tracked".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    remove::execute(RemoveArgs {
        pathspec: vec!["tracked.txt".to_string()],
        cached: true,
        recursive: false,
        force: false,
        dry_run: false,
        ignore_unmatch: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        sparse: false,
    })
    .await;
    fs::write("tracked.txt", "tracked\nstill here\n").unwrap();

    reset::execute_safe(
        ResetArgs {
            target: Some("HEAD~1".to_string()),
            soft: false,
            mixed: false,
            hard: true,
            merge: false,
            keep: false,
            pathspecs: vec![],
            pathspec_separator: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            no_refresh: false,
            patch: false,
            auto_advance: false,
            no_auto_advance: false,
        },
        &libra::utils::output::OutputConfig::default(),
    )
    .await
    .expect("hard reset should remove files tracked by HEAD even when absent from the index");

    assert!(
        fs::metadata("tracked.txt").is_err(),
        "hard reset should remove tracked.txt because the target commit does not contain it"
    );
    assert!(
        changes_to_be_committed().await.is_empty(),
        "hard reset should clear staged deletions"
    );
    let unstaged = changes_to_be_staged().unwrap();
    assert!(
        unstaged.deleted.is_empty(),
        "hard reset should not leave tracked.txt as a deleted path"
    );
}

#[tokio::test]
#[serial(cwd)]
/// Tests reset with HEAD~ syntax
async fn test_reset_with_head_reference() {
    let temp_path = tempdir().unwrap();
    let _guard = ChangeDirGuard::new(temp_path.path());

    let (_, _, _, _) = setup_standard_repo(temp_path.path()).await;
    let second_commit = Head::current_commit().await.unwrap();

    // Reset using HEAD~ syntax
    reset::execute(ResetArgs {
        target: Some("HEAD~1".to_string()),
        soft: false,
        mixed: true,
        hard: false,
        merge: false,
        keep: false,
        pathspecs: vec![],
        pathspec_separator: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        no_refresh: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    // Verify HEAD moved back one commit
    let current_commit = Head::current_commit().await.unwrap();
    assert_ne!(current_commit, second_commit);

    // Verify working directory still has files
    assert!(fs::metadata("1.txt").is_ok());
    assert!(fs::metadata("4.txt").is_ok());

    // Verify index was reset (4.txt should be untracked)
    let unstaged = changes_to_be_staged().unwrap();
    assert!(
        unstaged
            .new
            .iter()
            .any(|path| path.file_name().unwrap() == "4.txt")
    );
}

#[tokio::test]
#[serial(cwd)]
/// Tests reset on a branch (should move branch pointer, not create detached HEAD)
async fn test_reset_on_branch() {
    let temp_path = tempdir().unwrap();
    let _guard = ChangeDirGuard::new(temp_path.path());

    let (commit1, _, _, _) = setup_standard_repo(temp_path.path()).await;

    // Verify we're on a branch before reset
    let head_before = Head::current().await;
    match head_before {
        Head::Branch(branch_name) => {
            assert_eq!(branch_name, "main"); // Default branch name

            // Perform reset
            reset::execute(ResetArgs {
                target: Some(commit1.to_string()),
                soft: true,
                mixed: false,
                hard: false,
                merge: false,
                keep: false,
                pathspecs: vec![],
                pathspec_separator: false,
                pathspec_from_file: None,
                pathspec_file_nul: false,
                no_refresh: false,
                patch: false,
                auto_advance: false,
                no_auto_advance: false,
            })
            .await;

            // Verify we're still on the same branch after reset
            let head_after = Head::current().await;
            match head_after {
                Head::Branch(branch_name_after) => {
                    assert_eq!(branch_name_after, branch_name);
                }
                Head::Detached(_) => {
                    panic!("Reset should not create detached HEAD when on a branch");
                }
            }

            // Verify the branch pointer moved
            let current_commit = Head::current_commit().await.unwrap();
            assert_eq!(current_commit, commit1);
        }
        Head::Detached(_) => {
            panic!("Should be on a branch initially");
        }
    }
}

#[tokio::test]
#[serial(cwd)]
/// Tests reset --hard skips and preserves ignored directories and their contents
async fn test_reset_hard_skips_ignored_directories() {
    let temp_path = tempdir().unwrap();
    let _guard = ChangeDirGuard::new(temp_path.path());
    setup_with_new_libra_in(temp_path.path()).await;
    setup_reset_user_identity().await;

    fs::write("file1.txt", "initial content\n").unwrap();
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["file1.txt".to_string()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("first commit".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    // Create .libraignore ignoring a directory
    fs::write(".libraignore", "ignored_dir/\n").unwrap();

    // Create the ignored directory and a file in it
    let ignored_dir = temp_path.path().join("ignored_dir");
    fs::create_dir_all(&ignored_dir).unwrap();
    let ignored_file = ignored_dir.join("file2.txt");
    fs::write(&ignored_file, "ignored file content\n").unwrap();

    // Modify the tracked file
    fs::write("file1.txt", "modified content\n").unwrap();

    // Perform hard reset
    reset::execute(ResetArgs {
        target: Some("HEAD".to_string()),
        soft: false,
        mixed: false,
        hard: true,
        merge: false,
        keep: false,
        pathspecs: vec![],
        pathspec_separator: false,
        pathspec_from_file: None,
        pathspec_file_nul: false,
        no_refresh: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    // Verify tracked file is restored
    assert_eq!(
        fs::read_to_string("file1.txt").unwrap(),
        "initial content\n"
    );

    // Verify ignored directory and file are preserved and not deleted
    assert!(ignored_dir.exists());
    assert!(ignored_file.exists());
    assert_eq!(
        fs::read_to_string(&ignored_file).unwrap(),
        "ignored file content\n"
    );
}

// ---------------------------------------------------------------------------
// `--pathspec-from-file` / `--pathspec-file-nul` / `--no-refresh` (Git-compat
// bulk pathspec input). These exercise the black-box CLI surface end to end.
// ---------------------------------------------------------------------------

/// Build a committed repo, then create and stage `a.txt` and `b.txt` (neither
/// present in HEAD), so a pathspec reset can selectively unstage them.
fn repo_with_two_staged_files() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("a.txt"), "a\n").unwrap();
    fs::write(repo.path().join("b.txt"), "b\n").unwrap();
    let out = run_libra_command(&["add", "a.txt", "b.txt"], repo.path());
    assert_cli_success(&out, "failed to stage a.txt/b.txt");
    repo
}

#[test]
fn reset_pathspec_from_file_resets_listed_paths() {
    let repo = repo_with_two_staged_files();
    fs::write(repo.path().join("paths.txt"), "a.txt\n").unwrap();
    let out = run_libra_command(
        &["--json", "reset", "--pathspec-from-file=paths.txt"],
        repo.path(),
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["mode"], "mixed");
    let pathspecs = json["data"]["pathspecs"]
        .as_array()
        .expect("pathspecs array");
    assert_eq!(pathspecs.len(), 1, "only the listed path is unstaged");
    assert_eq!(pathspecs[0], "a.txt");
    assert_eq!(json["data"]["files_unstaged"], 1);
    // Pathspec resets never move HEAD, so the schema promises a null previous.
    assert!(json["data"]["previous_commit"].is_null());
}

#[test]
fn reset_pathspec_from_file_missing_path_errors() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("paths.txt"), "nonexistent.txt\n").unwrap();
    let out = run_libra_command(
        &["--json", "reset", "--pathspec-from-file=paths.txt"],
        repo.path(),
    );
    assert_eq!(out.status.code(), Some(129));
    let report: serde_json::Value =
        serde_json::from_slice(&out.stderr).expect("expected stderr JSON in --json mode");
    assert_eq!(report["error_code"], "LBR-CLI-003");
}

#[test]
fn reset_pathspec_from_stdin_dash() {
    let repo = repo_with_two_staged_files();
    let out = run_libra_command_with_stdin(
        &["--json", "reset", "--pathspec-from-file=-"],
        repo.path(),
        "a.txt\n",
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("expected JSON stdout");
    assert_eq!(json["data"]["files_unstaged"], 1);
    assert_eq!(json["data"]["pathspecs"][0], "a.txt");
}

#[test]
fn reset_pathspec_file_nul_uses_nul_separator() {
    let repo = repo_with_two_staged_files();
    // NUL-separated, no trailing separator.
    fs::write(repo.path().join("paths.txt"), "a.txt\0b.txt").unwrap();
    let out = run_libra_command(
        &[
            "--json",
            "reset",
            "--pathspec-from-file=paths.txt",
            "--pathspec-file-nul",
        ],
        repo.path(),
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["files_unstaged"], 2);
}

#[test]
fn reset_pathspec_file_newline_default() {
    let repo = repo_with_two_staged_files();
    // CRLF line endings and a blank line: `\r` stripped, empty item dropped.
    fs::write(repo.path().join("paths.txt"), "a.txt\r\nb.txt\n\n").unwrap();
    let out = run_libra_command(
        &["--json", "reset", "--pathspec-from-file=paths.txt"],
        repo.path(),
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["files_unstaged"], 2);
}

#[test]
fn reset_pathspec_from_file_treats_quotes_literally() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tracked\nmore\n").unwrap();
    let out = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert_cli_success(&out, "failed to stage tracked.txt");
    // A double-quoted line is taken literally (no Git C-style decoding), so it
    // does NOT resolve to `tracked.txt`; the literal path is unmatched.
    fs::write(repo.path().join("paths.txt"), "\"tracked.txt\"\n").unwrap();
    let out = run_libra_command(
        &["--json", "reset", "--pathspec-from-file=paths.txt"],
        repo.path(),
    );
    assert_eq!(out.status.code(), Some(129));
    let report: serde_json::Value =
        serde_json::from_slice(&out.stderr).expect("expected stderr JSON in --json mode");
    assert_eq!(report["error_code"], "LBR-CLI-003");
    assert!(
        report["message"]
            .as_str()
            .unwrap_or_default()
            .contains("\"tracked.txt\""),
        "literal quotes should appear in the unmatched path: {}",
        report["message"]
    );
}

#[test]
fn reset_pathspec_from_file_conflicts_with_cli_pathspec() {
    let repo = repo_with_two_staged_files();
    fs::write(repo.path().join("paths.txt"), "a.txt\n").unwrap();
    // Explicit `HEAD` target so the trailing `b.txt` is parsed as a pathspec
    // (otherwise clap binds the first positional to <target>).
    let out = run_libra_command(
        &[
            "--json",
            "reset",
            "HEAD",
            "--pathspec-from-file=paths.txt",
            "--",
            "b.txt",
        ],
        repo.path(),
    );
    assert_eq!(out.status.code(), Some(129));
    let report: serde_json::Value =
        serde_json::from_slice(&out.stderr).expect("expected stderr JSON in --json mode");
    assert_eq!(report["error_code"], "LBR-CLI-002");
    assert!(
        report["message"]
            .as_str()
            .unwrap_or_default()
            .contains("pathspec-from-file"),
        "unexpected message: {}",
        report["message"]
    );
}

#[test]
fn reset_pathspec_from_file_large_set() {
    let repo = create_committed_repo_via_cli();
    // A representative bulk set proving streaming parse + batch processing
    // (the plan's 10k figure is a soft perf target, not a hard assertion).
    const N: usize = 1500;
    let mut listing = String::new();
    for i in 0..N {
        let name = format!("bulk_{i}.txt");
        fs::write(repo.path().join(&name), "x\n").unwrap();
        listing.push_str(&name);
        listing.push('\n');
    }
    let out = run_libra_command(&["add", "."], repo.path());
    assert_cli_success(&out, "failed to stage bulk files");
    // Write the listing AFTER staging so paths.txt itself stays untracked.
    fs::write(repo.path().join("paths.txt"), &listing).unwrap();
    let out = run_libra_command(
        &["--json", "reset", "--pathspec-from-file=paths.txt"],
        repo.path(),
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["files_unstaged"].as_u64(), Some(N as u64));
}

#[test]
fn reset_pathspec_from_file_invalid_utf8_path() {
    let repo = create_committed_repo_via_cli();
    // Raw invalid UTF-8 bytes as a single NUL-delimited pathspec.
    fs::write(repo.path().join("paths.bin"), [0xff, 0xfe, 0x00]).unwrap();
    let out = run_libra_command(
        &[
            "--json",
            "reset",
            "--pathspec-from-file=paths.bin",
            "--pathspec-file-nul",
        ],
        repo.path(),
    );
    assert_eq!(out.status.code(), Some(129));
    let report: serde_json::Value =
        serde_json::from_slice(&out.stderr).expect("expected stderr JSON in --json mode");
    assert_eq!(report["error_code"], "LBR-CLI-002");
}

#[test]
fn reset_no_refresh_is_noop() {
    let repo = create_committed_repo_via_cli();
    let out = run_libra_command(&["--json", "reset", "--no-refresh", "HEAD"], repo.path());
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["mode"], "mixed");
}

#[test]
fn reset_pathspec_file_nul_alone_is_noop() {
    let repo = create_committed_repo_via_cli();
    // `--pathspec-file-nul` without `--pathspec-from-file` only switches the
    // separator; with no pathspec source it is an inert no-op (full reset).
    let out = run_libra_command(
        &["--json", "reset", "--pathspec-file-nul", "HEAD"],
        repo.path(),
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["mode"], "mixed");
    assert!(
        json["data"]["pathspecs"]
            .as_array()
            .expect("pathspecs array")
            .is_empty()
    );
}

#[test]
fn reset_json_with_quiet_still_emits_json() {
    let repo = create_committed_repo_via_cli();
    let out = run_libra_command(
        &["--json", "--quiet", "reset", "--hard", "HEAD"],
        repo.path(),
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_json_stdout(&out);
    assert_eq!(json["command"], "reset");
    assert_eq!(json["data"]["mode"], "hard");
}

#[test]
fn reset_pathspec_from_file_rejects_escape() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("paths.txt"), "../escape.txt\n").unwrap();
    let out = run_libra_command(
        &["--json", "reset", "--pathspec-from-file=paths.txt"],
        repo.path(),
    );
    assert_eq!(out.status.code(), Some(129));
    let report: serde_json::Value =
        serde_json::from_slice(&out.stderr).expect("expected stderr JSON in --json mode");
    assert_eq!(report["error_code"], "LBR-CLI-002");
    assert!(
        report["message"]
            .as_str()
            .unwrap_or_default()
            .contains("outside the repository"),
        "unexpected message: {}",
        report["message"]
    );
}

#[test]
fn test_reset_literal_pathspecs_global() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    std::fs::write(p.join("x.txt"), "x\n").unwrap();
    std::fs::write(p.join("*.txt"), "star\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "x.txt", "*.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit",
    );
    std::fs::write(p.join("x.txt"), "x2\n").unwrap();
    std::fs::write(p.join("*.txt"), "star2\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "x.txt", "*.txt"], p), "stage");
    assert_cli_success(
        &run_libra_command(&["--literal-pathspecs", "reset", "--", "*.txt"], p),
        "reset literal",
    );
    let cached = run_libra_command(&["diff", "--cached", "--name-only"], p);
    let names = String::from_utf8_lossy(&cached.stdout);
    assert!(names.contains("x.txt"), "x.txt still staged: {names}");
    assert!(!names.contains("*.txt"), "*.txt was reset: {names}");
}

/// FIX-AD-01: `reset -- <pathspec>` unstages every matching path through the
/// shared pathspec engine, so a glob covers `x.txt` as well as the literal
/// `*.txt` (Git parity).
#[test]
fn test_reset_pathspec_glob_unstages_all_matches() {
    let repo = tempdir().unwrap();
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    std::fs::write(p.join("x.txt"), "x\n").unwrap();
    std::fs::write(p.join("*.txt"), "star\n").unwrap();
    std::fs::write(p.join("notes.md"), "md\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "x.txt", "*.txt", "notes.md"], p),
        "add",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit",
    );
    std::fs::write(p.join("x.txt"), "x2\n").unwrap();
    std::fs::write(p.join("*.txt"), "star2\n").unwrap();
    std::fs::write(p.join("notes.md"), "md2\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "x.txt", "*.txt", "notes.md"], p),
        "stage",
    );

    assert_cli_success(
        &run_libra_command(&["reset", "--", "*.txt"], p),
        "reset glob",
    );
    let cached = run_libra_command(&["diff", "--cached", "--name-only"], p);
    let names = String::from_utf8_lossy(&cached.stdout);
    assert!(
        !names.contains("x.txt"),
        "x.txt unstaged by the glob: {names}"
    );
    assert!(!names.contains("*.txt"), "*.txt unstaged: {names}");
    assert!(names.contains("notes.md"), "notes.md untouched: {names}");

    // `:(exclude)` pairs with the include spec — it must never form an
    // exclude-only set that unstages the whole index (FIX-AD-01 review P0-1).
    assert_cli_success(
        &run_libra_command(&["add", "*.txt", "x.txt", "notes.md"], p),
        "re-stage",
    );
    let out = run_libra_command(&["reset", "--", "*.txt", ":(exclude)x.txt"], p);
    assert_cli_success(&out, "reset with an exclude spec");
    let cached = run_libra_command(&["diff", "--cached", "--name-only"], p);
    let names = String::from_utf8_lossy(&cached.stdout);
    assert!(
        !names.contains("*.txt"),
        "*.txt reset by the include spec: {names}"
    );
    assert!(
        names.contains("x.txt"),
        "x.txt is excluded, so it stays staged: {names}"
    );
    assert!(
        names.contains("notes.md"),
        "a path the user did not name must stay staged: {names}"
    );
}

/// A repo whose `feature` branch has a commit conflicting with `main`
/// (`shared.txt`) followed by a clean one (`clean.txt`); `main` diverges.
fn seq_conflict_repo() -> (tempfile::TempDir, String, String) {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let commit = |msg: &str| {
        assert_cli_success(
            &run_libra_command(&["commit", "-m", msg, "--no-verify"], p),
            "commit",
        );
    };
    std::fs::write(p.join("shared.txt"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add base");
    commit("init shared");
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], p),
        "branch",
    );
    std::fs::write(p.join("shared.txt"), "feature\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add f1");
    commit("f1 edit");
    let f1 = super::cherry_pick_test::cp_rev_parse(p, "HEAD");
    std::fs::write(p.join("clean.txt"), "clean\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "clean.txt"], p), "add f2");
    commit("f2 clean");
    let f2 = super::cherry_pick_test::cp_rev_parse(p, "HEAD");
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");
    std::fs::write(p.join("shared.txt"), "main\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add main");
    commit("m edit");
    (repo, f1, f2)
}

fn seq_status(p: &std::path::Path) -> String {
    let out = run_libra_command(&["status"], p);
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Two-branch repo whose `feature` edit conflicts with `main` on `shared.txt`.
fn merge_conflict_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let commit = |msg: &str| {
        assert_cli_success(
            &run_libra_command(&["commit", "-m", msg, "--no-verify"], p),
            "commit",
        );
    };
    std::fs::write(p.join("shared.txt"), "top\nl1\nORIG\nl3\nbottom\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add base");
    commit("base shared");
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    std::fs::write(p.join("shared.txt"), "top\nl1\nFEATURE\nl3\nbottom\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add feature");
    commit("feature edit");
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    std::fs::write(p.join("shared.txt"), "top\nl1\nMAIN\nl3\nbottom\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add main");
    commit("main edit");
    repo
}

/// M-SEQ S5a/S5b (#477 HF-26): a whole-tree reset ends an in-progress merge;
/// a pathspec reset leaves merge-state.json alone.
#[test]
fn test_reset_clears_merge_state_matrix() {
    for (row, mode) in [("S5a", "--hard"), ("S5b", "--mixed")] {
        let repo = merge_conflict_repo();
        let p = repo.path();
        assert_eq!(
            run_libra_command(&["merge", "feature"], p).status.code(),
            Some(128),
            "{row} merge conflicts"
        );
        assert!(
            p.join(".libra/merge-state.json").exists(),
            "{row}: merge state is present"
        );
        assert_cli_success(
            &run_libra_command(&["reset", mode], p),
            &format!("{row} reset"),
        );
        assert!(
            !p.join(".libra/merge-state.json").exists(),
            "{row}: reset {mode} clears merge-state.json"
        );
        let abort = run_libra_command(&["merge", "--abort"], p);
        let abort_err = String::from_utf8_lossy(&abort.stderr);
        assert!(
            !abort.status.success(),
            "{row}: merge --abort has nothing to abort"
        );
        assert!(
            abort_err.contains("no merge in progress"),
            "{row}: {abort_err}"
        );
    }

    let repo = merge_conflict_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["merge", "feature"], p).status.code(),
        Some(128),
        "pathspec merge conflicts"
    );
    assert_cli_success(
        &run_libra_command(&["reset", "shared.txt"], p),
        "pathspec reset",
    );
    assert!(
        p.join(".libra/merge-state.json").exists(),
        "pathspec reset leaves merge-state.json"
    );
    assert_cli_success(
        &run_libra_command(&["merge", "--abort"], p),
        "cleanup abort",
    );
}

/// M-SEQ S6 failure (#477 HF-26): injected autostash promotion failure keeps
/// both sidecars and names `libra merge --abort`.
#[test]
fn test_reset_merge_autostash_promotion_failure_keeps_state() {
    let repo = merge_conflict_repo();
    let p = repo.path();
    std::fs::write(p.join("unrelated.txt"), "precious\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "unrelated.txt"], p),
        "add dirty",
    );
    assert_eq!(
        run_libra_command(&["merge", "feature", "--autostash"], p)
            .status
            .code(),
        Some(128),
        "autostash merge conflicts"
    );
    assert!(p.join(".libra/merge-state.json").exists());
    assert!(p.join(".libra/merge-autostash.json").exists());
    let reset = run_libra_command_with_env(
        &["reset", "--hard"],
        p,
        &[("LIBRA_TEST_MERGE_AUTOSTASH_PROMOTE", "fail")],
    );
    assert_cli_success(&reset, "reset still succeeds");
    let stderr = String::from_utf8_lossy(&reset.stderr);
    assert!(
        stderr.contains("libra merge --abort"),
        "warning names merge --abort: {stderr}"
    );
    assert!(
        p.join(".libra/merge-state.json").exists(),
        "merge-state.json kept"
    );
    assert!(
        p.join(".libra/merge-autostash.json").exists(),
        "merge-autostash.json kept"
    );
    let list = run_libra_command(&["stash", "list"], p);
    assert!(
        String::from_utf8_lossy(&list.stdout)
            .lines()
            .all(|line| line.trim().is_empty()),
        "autostash was not promoted: {}",
        String::from_utf8_lossy(&list.stdout)
    );
}

/// M-SEQ S0-S2, S4a, S8, S9a (#477 HF-01, ADR-HF-03): a whole-tree reset ends a
/// stopped single-commit cherry-pick/revert, leaves rebase alone, and a pathspec
/// reset changes no sequence state.
#[test]
fn test_reset_clears_single_pick_and_revert_state_matrix() {
    // S0: `--abort` still ends the sequence (regression guard).
    let (repo, f1, f2) = seq_conflict_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1], p).status.code(),
        Some(128),
        "S0 pick conflicts"
    );
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--abort"], p),
        "S0 abort",
    );
    assert!(!seq_status(p).contains("cherry-pick"), "S0: no state left");
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "-n", &f2], p),
        "S0: the next pick runs",
    );

    // S1 / S2: reset --hard / --mixed end a stopped single-commit pick.
    for (row, mode) in [("S1", "--hard"), ("S2", "--mixed")] {
        let (repo, f1, f2) = seq_conflict_repo();
        let p = repo.path();
        assert_eq!(
            run_libra_command(&["cherry-pick", &f1], p).status.code(),
            Some(128),
            "{row} pick conflicts"
        );
        assert_cli_success(
            &run_libra_command(&["reset", mode], p),
            &format!("{row} reset"),
        );
        assert!(
            !seq_status(p).contains("cherry-pick"),
            "{row}: reset {mode} ends the stopped pick: {}",
            seq_status(p)
        );
        if mode == "--mixed" {
            assert!(
                std::fs::read_to_string(p.join("shared.txt"))
                    .unwrap()
                    .contains("<<<<<<<"),
                "{row}: --mixed keeps the working tree"
            );
        }
        assert_cli_success(
            &run_libra_command(&["cherry-pick", &f2], p),
            &format!("{row}: the next pick runs"),
        );
    }

    // S4a: reset --hard ends a stopped single-commit revert.
    let (repo, f1, _f2) = seq_conflict_repo();
    let p = repo.path();
    let main_commit = super::cherry_pick_test::cp_rev_parse(p, "HEAD");
    assert_eq!(
        run_libra_command(&["revert", "--no-edit", &f1], p)
            .status
            .code(),
        Some(128),
        "S4a revert conflicts"
    );
    assert_cli_success(&run_libra_command(&["reset", "--hard"], p), "S4a reset");
    assert!(
        !seq_status(p).contains("revert"),
        "S4a: no revert state left"
    );
    assert_cli_success(
        &run_libra_command(&["revert", "--no-edit", &main_commit], p),
        "S4a: the next revert runs",
    );

    // S8: a stopped rebase is left alone.
    let (repo, _f1, _f2) = seq_conflict_repo();
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["switch", "feature"], p), "S8 switch");
    assert_eq!(
        run_libra_command(&["rebase", "main"], p).status.code(),
        Some(128),
        "S8 rebase conflicts"
    );
    assert_cli_success(&run_libra_command(&["reset", "--hard"], p), "S8 reset");
    assert!(
        seq_status(p).contains("rebase"),
        "S8: rebase state survives: {}",
        seq_status(p)
    );
    assert_cli_success(&run_libra_command(&["rebase", "--abort"], p), "S8 abort");

    // S9a: a pathspec reset changes no sequence state.
    let (repo, f1, f2) = seq_conflict_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &f1, &f2], p)
            .status
            .code(),
        Some(128),
        "S9a pick conflicts"
    );
    let before = super::cherry_pick_test::repo_table_rows(p, "sequence_state");
    assert_cli_success(
        &run_libra_command(&["reset", "shared.txt"], p),
        "S9a pathspec reset",
    );
    assert_eq!(
        super::cherry_pick_test::repo_table_rows(p, "sequence_state"),
        before,
        "S9a: a pathspec reset leaves the sequence untouched"
    );
    // A successful reset must not forget a stop while conflict stages remain.
    // Preserve both single-item recovery and a multi-item sequence's options.
    for mode in ["--soft", "--merge"] {
        for multiple in [false, true] {
            for reverting in [false, true] {
                let (repo, first, second) = if reverting {
                    multi_revert_repo()
                } else {
                    seq_conflict_repo()
                };
                let p = repo.path();
                let operation = if reverting { "revert" } else { "cherry-pick" };
                let file = if reverting { "a.txt" } else { "shared.txt" };
                let original_head = super::cherry_pick_test::cp_rev_parse(p, "HEAD");
                let original_content = fs::read(p.join(file)).expect("pre-operation content");
                let mut args = vec![operation];
                if reverting {
                    args.push("--no-edit");
                }
                args.push(&first);
                if multiple {
                    args.push(&second);
                }
                assert_eq!(
                    run_libra_command(&args, p).status.code(),
                    Some(128),
                    "operation conflicts"
                );
                if reverting {
                    use git_internal::internal::index::{Index, IndexEntry};

                    // Revert currently stages its conflict-marker blob at stage 0.
                    // Build an unresolved delete/modify index explicitly so this
                    // matrix exercises reset's unmerged-index recovery guard.
                    let base_blob: ObjectHash =
                        super::cherry_pick_test::cp_rev_parse(p, &format!("{first}:{file}"))
                            .parse()
                            .expect("reverted blob id");
                    let ours_blob: ObjectHash = super::cherry_pick_test::cp_rev_parse(
                        p,
                        &format!("{original_head}:{file}"),
                    )
                    .parse()
                    .expect("original HEAD blob id");
                    let _hash_guard = set_hash_kind_for_test(base_blob.kind());
                    let index_path = p.join(".libra/index");
                    let mut index = Index::load(&index_path).expect("conflicted revert index");
                    for stage in 0..=3 {
                        index.remove(file, stage);
                    }
                    let base_content =
                        run_libra_command(&["cat-file", "-p", &base_blob.to_string()], p);
                    assert_cli_success(&base_content, "read reverted blob");
                    let base_size =
                        u32::try_from(base_content.stdout.len()).expect("base blob size");
                    let ours_size = u32::try_from(original_content.len()).expect("ours blob size");
                    for (stage, hash, size) in
                        [(1, base_blob, base_size), (2, ours_blob, ours_size)]
                    {
                        let mut entry = IndexEntry::new_from_blob(file.to_string(), hash, size);
                        entry.flags.stage = stage;
                        index.add(entry);
                    }
                    index.save(&index_path).expect("unmerged revert fixture");
                }
                let before_rows = super::cherry_pick_test::repo_table_rows(p, "sequence_state");
                let before_sidecar = if reverting {
                    Some(fs::read(p.join(".libra/revert-state.json")).expect("revert sidecar"))
                } else {
                    None
                };
                let before_stages = run_libra_command(&["ls-files", "--unmerged"], p);
                assert_cli_success(&before_stages, "list conflict stages");
                assert!(
                    !before_stages.stdout.is_empty(),
                    "{operation} {mode} multiple={multiple}: fixture must have an unmerged index"
                );
                let before_content = fs::read(p.join(file)).expect("conflicted worktree bytes");
                let target = if mode == "--soft" {
                    super::cherry_pick_test::cp_rev_parse(p, "HEAD~1")
                } else {
                    original_head.clone()
                };
                let reset = run_libra_command(&["reset", mode, &target], p);
                assert_cli_success(&reset, "preserving-mode reset still succeeds");
                let warning = String::from_utf8_lossy(&reset.stderr);
                assert!(
                    warning.contains("index still has unresolved conflicts")
                        && warning.contains("stopped sequences were preserved"),
                    "{operation} {mode}: {warning}"
                );
                assert_eq!(super::cherry_pick_test::cp_rev_parse(p, "HEAD"), target);
                assert_eq!(
                    super::cherry_pick_test::repo_table_rows(p, "sequence_state"),
                    before_rows
                );
                if let Some(before) = before_sidecar {
                    assert_eq!(
                        fs::read(p.join(".libra/revert-state.json")).expect("sidecar preserved"),
                        before
                    );
                }
                let after_stages = run_libra_command(&["ls-files", "--unmerged"], p);
                assert_cli_success(&after_stages, "list preserved stages");
                assert_eq!(after_stages.stdout, before_stages.stdout);
                assert_eq!(
                    fs::read(p.join(file)).expect("worktree preserved"),
                    before_content
                );
                assert_cli_success(
                    &run_libra_command(&[operation, "--abort"], p),
                    "saved recovery remains usable",
                );
                assert_eq!(
                    super::cherry_pick_test::cp_rev_parse(p, "HEAD"),
                    original_head
                );
                assert_eq!(
                    fs::read(p.join(file)).expect("abort restored content"),
                    original_content
                );
                assert!(super::cherry_pick_test::repo_table_rows(p, "sequence_state").is_empty());
                assert!(!p.join(".libra/revert-state.json").exists());
            }
        }
    }

    // Natural revert conflicts use stage 0. Keep recovery while those staged
    // markers remain; clean staged resolutions and mixed resets may conclude.
    for multiple in [false, true] {
        for scenario in ["markers", "resolved", "mixed", "missing-blob"] {
            let (repo, first, second) = multi_revert_repo();
            let p = repo.path();
            let original_head = super::cherry_pick_test::cp_rev_parse(p, "HEAD");
            let original_content = fs::read(p.join("a.txt")).expect("original content");
            let mut args = vec!["revert", "--no-edit", &first];
            if multiple {
                args.push(&second);
            }
            assert_eq!(run_libra_command(&args, p).status.code(), Some(128));
            let sidecar_path = p.join(".libra/revert-state.json");
            let before_sidecar = fs::read(&sidecar_path).expect("natural revert stop");
            let stages = run_libra_command(&["ls-files", "--unmerged"], p);
            assert_cli_success(&stages, "natural revert conflict listing");
            assert!(stages.stdout.is_empty(), "natural revert uses stage 0");
            let marker_content = fs::read(p.join("a.txt")).expect("natural conflict content");
            assert!(String::from_utf8_lossy(&marker_content).contains("<<<<<<<"));
            let index_path = p.join(".libra/index");
            let recoverable_index = fs::read(&index_path).expect("recoverable natural index");
            if scenario == "resolved" {
                fs::write(p.join("a.txt"), "resolved\n").expect("resolve content");
                assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "stage resolution");
            } else if scenario == "missing-blob" {
                use git_internal::internal::index::Index;

                let head: ObjectHash = original_head.parse().expect("HEAD hash");
                let _hash_guard = set_hash_kind_for_test(head.kind());
                let mut index = Index::load(&index_path).expect("natural index");
                let mut entry = index.remove("a.txt", 0).expect("stage-0 conflict");
                entry.hash = "f"
                    .repeat(original_head.len())
                    .parse()
                    .expect("missing blob hash");
                index.add(entry);
                index.save(&index_path).expect("missing blob index");
            }
            let before_index = fs::read(&index_path).expect("index before reset");
            let before_content = fs::read(p.join("a.txt")).expect("worktree before reset");
            let mode = if scenario == "mixed" {
                "--mixed"
            } else {
                "--soft"
            };
            let target = super::cherry_pick_test::cp_rev_parse(p, "HEAD~1");
            let preserve = matches!(scenario, "markers" | "missing-blob");
            let reset = run_libra_command(
                &["--json", "--exit-code-on-warning", "reset", mode, &target],
                p,
            );
            assert_eq!(
                reset.status.code(),
                Some(if preserve { 9 } else { 0 }),
                "natural revert {scenario} multiple={multiple}: {}",
                String::from_utf8_lossy(&reset.stderr)
            );
            let json = parse_json_stdout(&reset);
            assert_eq!(json["ok"], true);
            assert_eq!(json["data"]["commit"], target);
            assert_eq!(super::cherry_pick_test::cp_rev_parse(p, "HEAD"), target);
            assert_eq!(
                fs::read(p.join("a.txt")).expect("worktree after reset"),
                before_content
            );
            if scenario != "mixed" {
                assert_eq!(
                    fs::read(&index_path).expect("soft reset index"),
                    before_index
                );
            }
            let warning = String::from_utf8_lossy(&reset.stderr);
            if preserve {
                assert_eq!(
                    fs::read(&sidecar_path).expect("preserved stop"),
                    before_sidecar
                );
                assert!(warning.contains("libra revert --abort"), "{warning}");
                assert!(warning.contains("a.txt"), "{warning}");
                assert!(
                    warning.contains(if scenario == "markers" {
                        "staged conflict markers remain"
                    } else {
                        "could not read staged blob"
                    }),
                    "{warning}"
                );
                if scenario == "missing-blob" {
                    fs::write(&index_path, &recoverable_index)
                        .expect("repair staged object reference");
                }
            } else {
                assert!(!warning.contains("stopped revert"), "{warning}");
                if multiple {
                    let state: serde_json::Value = serde_json::from_slice(
                        &fs::read(&sidecar_path).expect("remaining revert queue"),
                    )
                    .expect("valid revert state");
                    let before: serde_json::Value =
                        serde_json::from_slice(&before_sidecar).expect("original state");
                    assert_eq!(state["stop_concluded"], true);
                    assert_eq!(state["remaining"], before["remaining"]);
                    assert_eq!(state["orig_head"], before["orig_head"]);
                } else {
                    assert!(!sidecar_path.exists(), "single resolved stop concludes");
                }
            }
            if preserve || multiple {
                assert_cli_success(
                    &run_libra_command(&["revert", "--abort"], p),
                    "natural revert recovery",
                );
                assert_eq!(
                    super::cherry_pick_test::cp_rev_parse(p, "HEAD"),
                    original_head
                );
                assert_eq!(
                    fs::read(p.join("a.txt")).expect("restored content"),
                    original_content
                );
                assert!(!sidecar_path.exists());
            }
        }
    }

    // Structured output keeps one success envelope and visible recovery warnings.
    // The warning-exit flag changes status only, not the durable reset or state.
    for output_mode in ["--json", "--machine"] {
        for exit_on_warning in [false, true] {
            let (repo, first, _second) = seq_conflict_repo();
            let p = repo.path();
            assert_eq!(
                run_libra_command(&["cherry-pick", &first], p).status.code(),
                Some(128)
            );
            let before = super::cherry_pick_test::repo_table_rows(p, "sequence_state");
            let target = super::cherry_pick_test::cp_rev_parse(p, "HEAD~1");
            let mut args = vec![output_mode];
            if exit_on_warning {
                args.push("--exit-code-on-warning");
            }
            args.extend(["reset", "--soft", &target]);
            let reset = run_libra_command(&args, p);
            assert_eq!(
                reset.status.code(),
                Some(if exit_on_warning { 9 } else { 0 })
            );
            let json = parse_json_stdout(&reset);
            assert_eq!(json["ok"], true);
            assert_eq!(json["command"], "reset");
            assert_eq!(json["data"]["commit"], target);
            assert!(json["data"].get("warnings").is_none());
            let warning = String::from_utf8_lossy(&reset.stderr);
            assert!(
                warning.contains("index still has unresolved conflicts")
                    && warning.contains("stopped sequences were preserved"),
                "{warning}"
            );
            assert_eq!(super::cherry_pick_test::cp_rev_parse(p, "HEAD"), target);
            assert_eq!(
                super::cherry_pick_test::repo_table_rows(p, "sequence_state"),
                before
            );
            assert_cli_success(
                &run_libra_command(&["cherry-pick", "--abort"], p),
                "structured warning retains recovery",
            );
        }
        // An unmerged index alone does not imply a stopped pick or revert.
        let (repo, first, _second) = seq_conflict_repo();
        let p = repo.path();
        assert_eq!(
            run_libra_command(&["cherry-pick", &first], p).status.code(),
            Some(128)
        );
        assert_cli_success(
            &run_libra_command(&["cherry-pick", "--quit"], p),
            "forget pick but retain conflict",
        );
        assert!(super::cherry_pick_test::repo_table_rows(p, "sequence_state").is_empty());
        assert!(!p.join(".libra/revert-state.json").exists());
        let stages = run_libra_command(&["ls-files", "--unmerged"], p);
        assert_cli_success(&stages, "conflicts remain after quit");
        assert!(!stages.stdout.is_empty());
        let reset = run_libra_command(
            &[
                output_mode,
                "--exit-code-on-warning",
                "reset",
                "--soft",
                "HEAD",
            ],
            p,
        );
        assert_cli_success(&reset, "no stopped state means no recovery warning");
        assert_eq!(parse_json_stdout(&reset)["ok"], true);
        assert!(
            !String::from_utf8_lossy(&reset.stderr).contains("stopped sequences were preserved")
        );
        let after = run_libra_command(&["ls-files", "--unmerged"], p);
        assert_cli_success(&after, "unowned conflicts remain");
        assert_eq!(after.stdout, stages.stdout);
    }

    // A soft reset can move HEAD without reading the index. If the subsequent
    // conclusion cannot read it, preserve the successful reset and the state.
    let (repo, first, _second) = seq_conflict_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["cherry-pick", &first], p).status.code(),
        Some(128)
    );
    let before = super::cherry_pick_test::repo_table_rows(p, "sequence_state");
    let index_path = p.join(".libra/index");
    let conflict_index = fs::read(&index_path).expect("recoverable index");
    fs::write(&index_path, b"not-an-index").expect("unreadable index fixture");
    let target = super::cherry_pick_test::cp_rev_parse(p, "HEAD~1");
    let reset = run_libra_command(&["reset", "--soft", &target], p);
    assert_cli_success(&reset, "post-reset index read failure is a warning");
    let warning = String::from_utf8_lossy(&reset.stderr);
    assert!(
        warning.contains("index could not be read")
            && warning.contains("stopped sequences were preserved"),
        "{warning}"
    );
    assert_eq!(super::cherry_pick_test::cp_rev_parse(p, "HEAD"), target);
    assert_eq!(
        super::cherry_pick_test::repo_table_rows(p, "sequence_state"),
        before
    );
    assert_eq!(
        fs::read(&index_path).expect("bad index untouched"),
        b"not-an-index"
    );
    fs::write(&index_path, conflict_index).expect("restore saved index");
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--abort"], p),
        "recovery after index repair",
    );
}

/// A repo whose `main` history makes reverting the first two commits conflict on
/// the first (a.txt was rewritten later) and leave a clean second one pending.
fn multi_revert_repo() -> (tempfile::TempDir, String, String) {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let commit = |msg: &str| {
        assert_cli_success(
            &run_libra_command(&["commit", "-m", msg, "--no-verify"], p),
            "commit",
        );
    };
    // c1 adds a.txt, c2 adds b.txt, c3 rewrites a.txt: reverting c1 conflicts
    // (a.txt changed since), while the pending revert of c2 applies cleanly.
    for (file, content, msg) in [
        ("a.txt", "one\n", "adds a"),
        ("b.txt", "bee\n", "adds b"),
        ("a.txt", "three\n", "a three"),
    ] {
        std::fs::write(p.join(file), content).unwrap();
        assert_cli_success(&run_libra_command(&["add", file], p), "add");
        commit(msg);
    }
    let c1 = super::cherry_pick_test::cp_rev_parse(p, "HEAD~2");
    let c2 = super::cherry_pick_test::cp_rev_parse(p, "HEAD~1");
    (repo, c1, c2)
}

/// M-SEQ S4a, multi-commit half (#477 HF-01/HF-02, ADR-HF-03): a whole-tree
/// reset marks a stopped multi-commit revert instead of clearing it, and
/// `--continue` then drains the remaining reverts onto the reset target.
#[test]
fn test_reset_marks_multi_revert_and_continue_refuses() {
    let (repo, c1, c2) = multi_revert_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["revert", "--no-edit", &c1, &c2], p)
            .status
            .code(),
        Some(128),
        "the first revert conflicts"
    );
    assert!(
        p.join(".libra/revert-state.json").exists(),
        "a revert is in progress"
    );
    // Reset to an explicit target other than HEAD: the skip below must keep it.
    let target = super::cherry_pick_test::cp_rev_parse(p, "HEAD~1");
    assert_cli_success(
        &run_libra_command(&["reset", "--hard", &target], p),
        "reset --hard <target>",
    );
    assert_eq!(
        super::cherry_pick_test::cp_rev_parse(p, "HEAD"),
        target,
        "the reset moved HEAD to the requested target"
    );
    assert!(
        p.join(".libra/revert-state.json").exists(),
        "the multi-commit sequence is kept, not cleared"
    );
    assert_cli_success(
        &run_libra_command(&["revert", "--continue"], p),
        "HF-02: --continue drains the remaining reverts",
    );
    assert!(
        !p.join(".libra/revert-state.json").exists(),
        "the sequence finished"
    );
    assert_eq!(
        super::cherry_pick_test::cp_rev_parse(p, "HEAD~1"),
        target,
        "--skip built on the reset target instead of restoring the original HEAD"
    );
    // --abort still restores the pre-revert state after an external reset,
    // discarding later tracked edits on the reset target.
    let (repo, c1, c2) = multi_revert_repo();
    let p = repo.path();
    let original = super::cherry_pick_test::cp_rev_parse(p, "HEAD");
    let original_content = fs::read(p.join("a.txt")).expect("original tracked bytes");
    let target = super::cherry_pick_test::cp_rev_parse(p, "HEAD~1");
    assert_eq!(
        run_libra_command(&["revert", "--no-edit", &c1, &c2], p)
            .status
            .code(),
        Some(128)
    );
    assert_cli_success(
        &run_libra_command(&["reset", "--hard", &target], p),
        "reset before abort",
    );
    fs::write(p.join("a.txt"), "later tracked edit\n").expect("later edit");
    assert_eq!(super::cherry_pick_test::cp_rev_parse(p, "HEAD"), target);
    assert_cli_success(
        &run_libra_command(&["revert", "--abort"], p),
        "abort restores pre-revert state",
    );
    assert_eq!(super::cherry_pick_test::cp_rev_parse(p, "HEAD"), original);
    assert_eq!(
        fs::read(p.join("a.txt")).expect("restored tracked bytes"),
        original_content
    );
    assert!(!p.join(".libra/revert-state.json").exists());
}

/// ADR-HF-03 item 5 (#477 HF-01): when concluding the cherry-pick half fails,
/// the reset still succeeds, warns, and STOPS — the revert state a later step
/// would have touched is left exactly as it was.
#[test]
fn test_reset_conclusion_failure_stops_before_revert_state() {
    for output_mode in [None, Some("--json"), Some("--machine")] {
        for exit_on_warning in [false, true] {
            let (repo, c1, c2) = multi_revert_repo();
            let p = repo.path();
            assert_eq!(
                run_libra_command(&["revert", "--no-edit", &c1, &c2], p)
                    .status
                    .code(),
                Some(128),
                "the first revert conflicts"
            );
            let before = std::fs::read(p.join(".libra/revert-state.json")).expect("revert state");
            let mut args = Vec::new();
            if let Some(mode) = output_mode {
                args.push(mode);
            }
            if exit_on_warning {
                args.push("--exit-code-on-warning");
            }
            args.extend(["reset", "--hard"]);
            let out = spawn_libra_command_with_env(
                &args,
                p,
                &[
                    ("LIBRA_TEST", "1"),
                    ("LIBRA_TEST_RESET_FAIL_CONCLUDE_CHERRY_PICK", "1"),
                ],
            )
            .wait_with_output()
            .expect("wait for libra");
            assert_eq!(
                out.status.code(),
                Some(if exit_on_warning { 9 } else { 0 }),
                "the reset itself still succeeds: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            if output_mode.is_some() {
                let json = parse_json_stdout(&out);
                assert_eq!(json["ok"], true);
                assert_eq!(json["command"], "reset");
                assert_eq!(json["data"]["mode"], "hard");
                assert!(json.get("warnings").is_none());
                assert!(json["data"].get("warnings").is_none());
            }
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                stderr.contains("stopped cherry-pick state could not be updated")
                    && stderr.contains("libra cherry-pick --quit"),
                "the warning names the leftover state and its recovery command: {stderr}"
            );
            assert_eq!(
                std::fs::read(p.join(".libra/revert-state.json")).expect("revert state kept"),
                before,
                "the ordered contract stops before touching revert state"
            );
        }
    }
}

/// #477 HF-01 (Codex R5): the conclusion is fenced. When a concurrent `--quit`
/// plus a fresh revert replaces the sidecar between the conclusion's read and
/// its write, the new owner's state survives byte-for-byte instead of being
/// overwritten by the stale snapshot — and the reset still succeeds.
#[test]
fn test_reset_conclusion_does_not_clobber_a_reclaimed_revert_state() {
    let (repo, c1, c2) = multi_revert_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["revert", "--no-edit", &c1, &c2], p)
            .status
            .code(),
        Some(128),
        "the first revert conflicts"
    );
    let reclaimed = r#"{"reclaimed_by":"a concurrent revert"}"#;
    let out = spawn_libra_command_with_env(
        &["reset", "--hard"],
        p,
        &[
            ("LIBRA_TEST", "1"),
            ("LIBRA_TEST_REVERT_RECLAIM_BEFORE_CONCLUDE", reclaimed),
        ],
    )
    .wait_with_output()
    .expect("wait for libra");
    assert_eq!(
        out.status.code(),
        Some(0),
        "the reset itself still succeeds: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(p.join(".libra/revert-state.json")).expect("revert state"),
        reclaimed,
        "the state the concurrent revert wrote is left exactly as it is"
    );
}

/// #477 HF-01 (Codex R6): the conclusion ends only the stop the reset actually
/// observed. A cherry-pick/revert started in the window AFTER the reset moved
/// the tree belongs to whoever started it, and the reset leaves both states
/// exactly as that starter wrote them.
#[test]
fn test_reset_does_not_conclude_a_sequence_started_after_it() {
    let (repo, c1, c2) = multi_revert_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["revert", "--no-edit", &c1, &c2], p)
            .status
            .code(),
        Some(128),
        "the first revert conflicts"
    );
    let started_after = "started-after-the-reset";
    let out = spawn_libra_command_with_env(
        &["reset", "--hard"],
        p,
        &[
            ("LIBRA_TEST", "1"),
            ("LIBRA_TEST_RESET_START_SEQUENCE_AFTER_RESET", started_after),
        ],
    )
    .wait_with_output()
    .expect("wait for libra");
    assert_eq!(
        out.status.code(),
        Some(0),
        "the reset itself still succeeds: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(p.join(".libra/revert-state.json")).expect("revert state"),
        started_after,
        "the revert that started after the reset keeps its own state"
    );
    let rows = super::cherry_pick_test::repo_table_rows(p, "sequence_state");
    assert_eq!(
        rows.len(),
        1,
        "the new sequence row is still there: {rows:?}"
    );
    assert!(
        rows[0].contains(started_after) && !rows[0].contains("stop_concluded"),
        "the new sequence row is neither cleared nor marked: {}",
        rows[0]
    );
}

/// #477 HF-01 (Codex R7): a stopped state the reset cannot even READ is still
/// leftover state, so it owes the user the ADR-HF-03 item 5 warning naming the
/// recovery command — never a silent "nothing to conclude". The cherry-pick
/// half failing also stops the ordered contract before revert state is touched.
#[test]
fn test_reset_warns_when_a_stopped_state_cannot_be_read() {
    for (seam, phrase, command) in [
        (
            "LIBRA_TEST_RESET_FAIL_SNAPSHOT_CHERRY_PICK",
            "stopped cherry-pick state could not be read",
            "libra cherry-pick --quit",
        ),
        (
            "LIBRA_TEST_RESET_FAIL_SNAPSHOT_REVERT",
            "stopped revert state could not be read",
            "libra revert --abort",
        ),
    ] {
        let (repo, c1, c2) = multi_revert_repo();
        let p = repo.path();
        assert_eq!(
            run_libra_command(&["revert", "--no-edit", &c1, &c2], p)
                .status
                .code(),
            Some(128),
            "{seam}: the first revert conflicts"
        );
        let before = std::fs::read(p.join(".libra/revert-state.json")).expect("revert state");
        let out = spawn_libra_command_with_env(
            &["reset", "--hard"],
            p,
            &[("LIBRA_TEST", "1"), (seam, "1")],
        )
        .wait_with_output()
        .expect("wait for libra");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{seam}: the reset itself still succeeds: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(phrase) && stderr.contains(command),
            "{seam}: the warning names the leftover state and its recovery command: {stderr}"
        );
        assert_eq!(
            std::fs::read(p.join(".libra/revert-state.json")).expect("revert state kept"),
            before,
            "{seam}: an unreadable snapshot never mutates state"
        );
    }
    // A readable legacy sidecar can still be unsafe to adopt: active or
    // removed linked worktrees make unmarked common-storage state ambiguous.
    // Cover both deletion (no remaining commit) and re-stamping (multi-commit),
    // with proven-main ownership as a positive control in every row.
    for removed_link in [false, true] {
        for pending in [false, true] {
            let (repo, c1, c2) = multi_revert_repo();
            let p = repo.path();
            let parent = tempfile::tempdir().expect("linked worktree parent");
            let linked = parent.path().join("linked");
            let linked_name = linked.to_str().expect("test path is UTF-8");
            assert_cli_success(
                &run_libra_command(&["worktree", "add", linked_name], p),
                "register linked history",
            );
            if removed_link {
                assert_cli_success(
                    &run_libra_command(&["worktree", "remove", linked_name], p),
                    "retain removed linked history",
                );
            }
            let args = if pending {
                vec!["revert", "--no-edit", &c1, &c2]
            } else {
                vec!["revert", "--no-edit", &c1]
            };
            assert_eq!(
                run_libra_command(&args, p).status.code(),
                Some(128),
                "revert conflicts"
            );
            let sidecar = p.join(".libra/revert-state.json");
            let owned: serde_json::Value =
                serde_json::from_slice(&fs::read(&sidecar).expect("owned sidecar"))
                    .expect("owned JSON");
            assert_eq!(owned["owner_scope"], "");
            let target = super::cherry_pick_test::cp_rev_parse(p, "HEAD");
            for owner in [None, Some("foreign-worktree")] {
                let mut legacy = owned.clone();
                let object = legacy.as_object_mut().expect("state object");
                object.remove("owner_scope");
                if let Some(owner) = owner {
                    object.insert("owner_scope".to_string(), owner.into());
                }
                object.insert("future_writer_field".to_string(), "preserve exactly".into());
                let before = serde_json::to_vec_pretty(&legacy).expect("legacy bytes");
                fs::write(&sidecar, &before).expect("simulate legacy common state");
                let reset = run_libra_command(&["reset", "--hard", &target], p);
                assert_cli_success(&reset, "reset succeeds while preserving ambiguous metadata");
                let stderr = String::from_utf8_lossy(&reset.stderr);
                assert!(
                    stderr.contains("COMMON storage") && stderr.contains("libra worktree doctor"),
                    "{stderr}"
                );
                assert_eq!(fs::read(&sidecar).expect("ambiguous sidecar kept"), before);
                assert_eq!(super::cherry_pick_test::cp_rev_parse(p, "HEAD"), target);
                let abort = run_libra_command(&["revert", "--abort"], p);
                assert!(
                    !abort.status.success(),
                    "ambiguous state remains inoperable"
                );
                assert_eq!(
                    fs::read(&sidecar).expect("control refusal keeps evidence"),
                    before
                );
            }
            fs::write(
                &sidecar,
                serde_json::to_vec_pretty(&owned).expect("owned state"),
            )
            .expect("restore proven-main owner");
            let reset = run_libra_command(&["reset", "--hard", &target], p);
            assert_cli_success(
                &reset,
                "proven-main state remains operable with linked history",
            );
            assert!(!String::from_utf8_lossy(&reset.stderr).contains("could not be"));
            if pending {
                let marked: serde_json::Value =
                    serde_json::from_slice(&fs::read(&sidecar).expect("marked state"))
                        .expect("marked JSON");
                assert_eq!(marked["owner_scope"], "");
                assert_eq!(marked["stop_concluded"], true);
                assert_eq!(marked["remaining"], owned["remaining"]);
            } else {
                assert!(!sidecar.exists(), "proven-main final stop is cleared");
            }
        }
    }
}

/// #477 HF-01 (Codex R8): once the revert snapshot is taken, a failure to
/// RE-READ the sidecar inside the fence is a real error, not "someone else owns
/// it now". The reset still succeeds, but warns with the recovery command
/// instead of silently reporting `Superseded`.
#[test]
fn test_reset_warns_when_the_revert_state_cannot_be_reread() {
    let (repo, c1, c2) = multi_revert_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["revert", "--no-edit", &c1, &c2], p)
            .status
            .code(),
        Some(128),
        "the first revert conflicts"
    );
    let out = spawn_libra_command_with_env(
        &["reset", "--hard"],
        p,
        &[
            ("LIBRA_TEST", "1"),
            ("LIBRA_TEST_REVERT_UNREADABLE_BEFORE_CONCLUDE", "1"),
        ],
    )
    .wait_with_output()
    .expect("wait for libra");
    assert_eq!(
        out.status.code(),
        Some(0),
        "the reset itself still succeeds: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("stopped revert state could not be updated")
            && stderr.contains("libra revert --abort"),
        "a re-read failure warns with the recovery command: {stderr}"
    );
    assert!(
        p.join(".libra/revert-state.json").exists(),
        "the conclusion did not remove anything after the failed re-read"
    );
}

/// #477 HF-01 (Codex R9): the revert sidecar lock is a real CROSS-PROCESS
/// exclusion on every platform. This test process holds the lock while a
/// `libra reset --hard` concludes: the child has already snapshotted the old
/// sidecar (the ready marker proves it) and must wait for the lock, so the new
/// sidecar written under the lock survives and the stale conclusion is dropped.
#[test]
fn test_reset_conclusion_waits_for_a_concurrent_revert_lock_holder() {
    let (repo, c1, c2) = multi_revert_repo();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["revert", "--no-edit", &c1, &c2], p)
            .status
            .code(),
        Some(128),
        "the first revert conflicts"
    );
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(p.join(".libra/revert-state.lock"))
        .expect("open revert state lock");
    lock.lock().expect("hold the revert state lock");

    let ready = p.join("conclude-ready.marker");
    let ready_env = ready.to_string_lossy().into_owned();
    let mut child = spawn_libra_command_with_env(
        &["reset", "--hard"],
        p,
        &[
            ("LIBRA_TEST", "1"),
            ("LIBRA_TEST_REVERT_CONCLUDE_READY_FILE", ready_env.as_str()),
        ],
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !ready.exists() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("the reset never reached the revert conclusion");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // The child snapshotted the old sidecar and now waits for our lock: it
    // cannot finish while we hold it (without the lock it would be done by now).
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(
        child.try_wait().expect("poll the reset").is_none(),
        "the conclusion must wait for the revert state lock"
    );
    let reclaimed = r#"{"reclaimed_by":"a concurrent revert holding the lock"}"#;
    std::fs::write(p.join(".libra/revert-state.json"), reclaimed).expect("reclaim sidecar");
    lock.unlock().expect("release the revert state lock");

    let out = child.wait_with_output().expect("wait for libra");
    assert_eq!(
        out.status.code(),
        Some(0),
        "the reset itself still succeeds: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(p.join(".libra/revert-state.json")).expect("revert state"),
        reclaimed,
        "the sidecar written under the lock survives the stale conclusion"
    );
}

/// FM-02 (M-MAT2 U4): `reset --hard` under `umask 077` materializes 700/600.
#[cfg(unix)]
#[test]
fn test_reset_hard_honors_process_umask() {
    use std::{os::unix::fs::PermissionsExt, process::Command};

    let repo = tempdir().expect("repo");
    let repo_path = repo.path();
    init_repo_via_cli(repo_path);
    configure_identity_via_cli(repo_path);
    let script = repo_path.join("run.sh");
    fs::write(&script, "#!/bin/sh\necho run\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(repo_path.join("plain.txt"), "plain\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "run.sh", "plain.txt"], repo_path),
        "stage files",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "modes", "--no-verify"], repo_path),
        "commit files",
    );
    fs::remove_file(&script).unwrap();
    fs::remove_file(repo_path.join("plain.txt")).unwrap();

    let home = repo_path.join(".libra-test-home");
    fs::create_dir_all(home.join(".config")).unwrap();
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "umask 077; exec {} reset --hard HEAD",
            env!("CARGO_BIN_EXE_libra")
        ))
        .current_dir(repo_path)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env(libra::utils::pager::LIBRA_TEST_ENV, "1")
        .output()
        .expect("reset under umask 077");
    assert!(
        output.status.success(),
        "reset under umask 077 failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::symlink_metadata(&script)
            .expect("script metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700,
        "100755 entry under umask 077 must be 700"
    );
    assert_eq!(
        fs::symlink_metadata(repo_path.join("plain.txt"))
            .expect("plain metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "100644 entry under umask 077 must be 600"
    );
}
