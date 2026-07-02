//! Tests `libra branch` for creation, listing, deletion, renaming,
//! upstream tracking, and `--contains`/`--no-contains` filtering.
//!
//! **Layer:** L1 — deterministic, no external dependencies.
//!
//! Fixture conventions:
//! - CLI cases use `create_committed_repo_via_cli()` and exercise the
//!   binary so we cover error-code/exit-code surfaces (`LBR-CLI-003`,
//!   `LBR-REPO-002`, `LBR-REPO-003`, `LBR-IO-002`).
//! - In-process cases call `setup_with_new_libra_in()` plus an empty
//!   commit chain (`commit::execute` with `allow_empty=true`,
//!   `disable_pre=true`) and assert against `Branch::find_branch` /
//!   `Head::current()`.
//! - The `--contains` test builds a divergent two-branch graph (master:
//!   base/m1/m2, dev: base/d1/d2) and exhaustively exercises filter
//!   semantics. Several Unix-only cases force `permission-denied` writes
//!   on the SQLite file, so they `skip_permission_denied_test_if_root`.

#![cfg(test)]

use std::collections::HashSet;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use git_internal::hash::{ObjectHash, get_hash_kind};
use libra::internal::{
    config::ConfigKv,
    db::get_db_conn_instance,
    operation::{OperationQueryPage, OperationService},
};
use serial_test::serial;
use tempfile::tempdir;

use super::*;

/// Scenario: `libra branch <new> <bad-ref>` must reject the invalid start
/// point with exit 129 and a structured `LBR-CLI-003` error. Pins the CLI
/// usage error envelope.
#[test]
fn test_branch_cli_invalid_start_point_returns_cli_exit_code() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["branch", "new", "badref"], repo.path());
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert!(stderr.contains("fatal: not a valid object name: 'badref'"));
    assert!(stderr.contains("Error-Code: LBR-CLI-003"));
}

/// Scenario: `--json branch <name>` must emit `command="branch"`,
/// `data.action="create"`, `data.name=<name>` and a non-empty
/// `data.commit`. Schema pin for branch-create JSON output.
#[test]
fn test_branch_json_create_output_reports_branch() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "branch", "feature"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "branch");
    assert_eq!(json["data"]["action"], "create");
    assert_eq!(json["data"]["name"], "feature");
    assert!(json["data"]["commit"].as_str().is_some());
}

#[tokio::test]
#[serial]
async fn test_branch_create_records_operation_log() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&output, "branch feature");

    let _guard = ChangeDirGuard::new(repo.path());
    let db = get_db_conn_instance().await;
    let repo_id = ConfigKv::get("libra.repoid").await.unwrap().unwrap().value;
    let page = OperationService::list_operations_by_repo_paginated_with_conn(
        &db,
        &repo_id,
        OperationQueryPage {
            page: 1,
            per_page: 10,
        },
    )
    .await
    .unwrap();
    let op = page
        .items
        .iter()
        .find(|item| item.command_name == "branch")
        .expect("branch create should record an operation");

    let graph = OperationService::load_restore_view_by_operation_with_conn(&db, &op.op_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(graph.operation.command_name, "branch");
    assert!(
        graph
            .operation
            .description
            .starts_with("create branch feature")
    );
    assert!(
        graph
            .refs
            .iter()
            .any(|reference| reference.ref_name == "feature")
    );
}

#[tokio::test]
#[serial]
async fn test_branch_create_mints_missing_repo_id_before_operation_log() {
    let repo = create_committed_repo_via_cli();

    {
        let _guard = ChangeDirGuard::new(repo.path());
        ConfigKv::unset_all("libra.repoid").await.unwrap();
    }

    let output = run_libra_command(&["branch", "legacy-feature"], repo.path());
    assert_cli_success(&output, "branch legacy-feature without repo id");

    let _guard = ChangeDirGuard::new(repo.path());
    let repo_id = ConfigKv::get("libra.repoid")
        .await
        .unwrap()
        .expect("branch create should mint repo id")
        .value;
    uuid::Uuid::parse_str(&repo_id).expect("minted repo id should be a UUID");

    let db = get_db_conn_instance().await;
    let page = OperationService::list_operations_by_repo_paginated_with_conn(
        &db,
        &repo_id,
        OperationQueryPage {
            page: 1,
            per_page: 10,
        },
    )
    .await
    .unwrap();
    assert!(page.items.iter().any(|item| {
        item.command_name == "branch" && item.description.contains("legacy-feature")
    }));
}

/// Scenario: human-readable branch creation must print "Created branch
/// 'feature' at <hash>" on stdout. Pins the confirmation message format.
#[test]
fn test_branch_create_outputs_confirmation() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&output, "branch feature");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Created branch 'feature' at "),
        "unexpected stdout: {stdout}"
    );
}

/// Scenario: in a freshly initialised repo with no commits but registered
/// remote refs, `branch -a` must still display the unborn HEAD (`* main`)
/// plus the remote ref. Regression guard against treating "unborn" as
/// "no branches".
#[tokio::test]
#[serial]
async fn test_branch_all_shows_unborn_head_even_with_remote_refs() {
    let repo = tempdir().unwrap();
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = ChangeDirGuard::new(repo.path());

    let remote_add = run_libra_command(
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/repo.git",
        ],
        repo.path(),
    );
    assert_cli_success(&remote_add, "remote add origin");

    Branch::update_branch(
        "refs/remotes/origin/main",
        &ObjectHash::zero_str(get_hash_kind()),
        Some("origin"),
    )
    .await
    .unwrap();

    let output = run_libra_command(&["branch", "-a"], repo.path());
    assert_cli_success(&output, "branch -a on unborn repo with remotes");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("* main"),
        "expected unborn HEAD marker in stdout: {stdout}"
    );
    assert!(
        stdout.contains("origin/main"),
        "expected remote branch in stdout: {stdout}"
    );
}

/// Scenario: when `branch -d` targets a misspelled branch, the structured
/// error must include a "did you mean" suggestion based on existing branch
/// names. Pins the typo-suggestion contract (`LBR-CLI-003`, exit 129).
#[test]
fn test_branch_not_found_suggests_similar_name() {
    let repo = create_committed_repo_via_cli();

    let create = run_libra_command(&["branch", "featur"], repo.path());
    assert_cli_success(&create, "branch featur");

    let output = run_libra_command(&["branch", "-d", "feature"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        stderr.contains("did you mean 'featur'?"),
        "expected suggestion in stderr, got: {stderr}"
    );
}

/// Scenario: `branch --set-upstream-to` from detached HEAD must fail with
/// `LBR-REPO-003` (exit 128) and the message must mention "HEAD is
/// detached" plus a "checkout a branch first" hint. Pins both the error
/// tag and the user-facing remediation.
#[test]
fn test_branch_set_upstream_detached_head_returns_repo_state_error() {
    let repo = create_committed_repo_via_cli();

    let detach = run_libra_command(&["switch", "--detach", "HEAD"], repo.path());
    assert!(
        detach.status.success(),
        "detach failed: {}",
        String::from_utf8_lossy(&detach.stderr)
    );

    let output = run_libra_command(&["branch", "--set-upstream-to", "origin/main"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-003");
    assert!(stderr.contains("HEAD is detached"));
    assert!(stderr.contains("checkout a branch first"));
}

/// Scenario: `branch --set-upstream-to <remote>/<branch>` must reject a
/// remote name that has no configured `remote.<name>.url`. Previously this
/// silently wrote `branch.main.remote=origin`, leaving JSON consumers with a
/// successful set-upstream output that could not be used by later push/pull
/// flows.
#[test]
fn test_branch_set_upstream_rejects_unknown_remote() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["branch", "--set-upstream-to", "origin/main"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        stderr.contains("remote 'origin' not found"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        stderr.contains("libra remote -v"),
        "missing remediation hint in stderr: {stderr}"
    );
}

#[test]
fn test_branch_unset_upstream_clears_current_branch_tracking() {
    let repo = create_committed_repo_via_cli();
    let remote_add = run_libra_command(
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/repo.git",
        ],
        repo.path(),
    );
    assert_cli_success(&remote_add, "remote add origin");
    let set = run_libra_command(&["branch", "--set-upstream-to", "origin/main"], repo.path());
    assert_cli_success(&set, "branch --set-upstream-to origin/main");

    let unset = run_libra_command(&["branch", "--unset-upstream"], repo.path());
    assert_cli_success(&unset, "branch --unset-upstream");

    let remote = run_libra_command(&["config", "get", "branch.main.remote"], repo.path());
    assert!(
        !remote.status.success(),
        "branch.main.remote should be unset: {}",
        String::from_utf8_lossy(&remote.stdout)
    );
    let merge = run_libra_command(&["config", "get", "branch.main.merge"], repo.path());
    assert!(
        !merge.status.success(),
        "branch.main.merge should be unset: {}",
        String::from_utf8_lossy(&merge.stdout)
    );
}

#[test]
fn test_branch_points_at_filters_exact_tip() {
    let repo = create_committed_repo_via_cli();
    let head = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    assert_cli_success(&head, "rev-parse HEAD");
    let head_oid = String::from_utf8_lossy(&head.stdout).trim().to_string();
    let create = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&create, "branch feature");

    let output = run_libra_command(&["branch", "--points-at", &head_oid], repo.path());
    assert_cli_success(&output, "branch --points-at HEAD");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("main"), "expected main in output: {stdout}");
    assert!(
        stdout.contains("feature"),
        "expected feature in output: {stdout}"
    );
}

#[test]
fn test_branch_ignore_case_sorts_case_insensitively() {
    let repo = create_committed_repo_via_cli();
    let alpha = run_libra_command(&["branch", "alpha"], repo.path());
    assert_cli_success(&alpha, "branch alpha");
    let beta = run_libra_command(&["branch", "Beta"], repo.path());
    assert_cli_success(&beta, "branch Beta");

    let output = run_libra_command(&["branch", "--ignore-case"], repo.path());
    assert_cli_success(&output, "branch --ignore-case");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let alpha_pos = stdout.find("alpha").expect("alpha should be listed");
    let beta_pos = stdout.find("Beta").expect("Beta should be listed");
    assert!(
        alpha_pos < beta_pos,
        "ignore-case sorting should place alpha before Beta: {stdout}"
    );
}

/// Scenario (Unix only): if SQLite write permission is revoked
/// (`chmod 0o444`), `branch --set-upstream-to` must surface an
/// `LBR-IO-002` error mentioning the failing config key. The original
/// permission mode is restored before assertions to avoid TempDir
/// teardown failures. Skipped under root because the chmod injection
/// has no effect.
#[cfg(unix)]
#[test]
fn test_branch_set_upstream_surfaces_config_write_failure() {
    if skip_permission_denied_test_if_root("test_branch_set_upstream_surfaces_config_write_failure")
    {
        return;
    }

    let repo = create_committed_repo_via_cli();
    let remote_add = run_libra_command(
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/repo.git",
        ],
        repo.path(),
    );
    assert_cli_success(&remote_add, "remote add origin");
    let db_path = repo.path().join(".libra").join("libra.db");
    let original_mode = fs::metadata(&db_path).unwrap().permissions().mode();

    fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o444)).unwrap();
    let output = run_libra_command(&["branch", "--set-upstream-to", "origin/main"], repo.path());
    fs::set_permissions(&db_path, std::fs::Permissions::from_mode(original_mode)).unwrap();

    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-IO-002");
    assert!(
        stderr.contains("failed to persist branch config 'branch.main.remote'"),
        "unexpected stderr: {stderr}"
    );
}

/// Scenario (Unix only): if the upstream is already configured, a
/// repeat `--set-upstream-to` call must NOT touch the config file. This
/// is verified by making the SQLite file read-only between invocations
/// and confirming the second call still succeeds. Pins the "no redundant
/// write" optimisation. Skipped under root.
#[cfg(unix)]
#[test]
fn test_branch_set_upstream_idempotent_path_skips_redundant_write() {
    if skip_permission_denied_test_if_root(
        "test_branch_set_upstream_idempotent_path_skips_redundant_write",
    ) {
        return;
    }

    let repo = create_committed_repo_via_cli();
    let remote_add = run_libra_command(
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/repo.git",
        ],
        repo.path(),
    );
    assert_cli_success(&remote_add, "remote add origin");

    let first = run_libra_command(&["branch", "--set-upstream-to", "origin/main"], repo.path());
    assert_cli_success(&first, "initial set-upstream");

    let db_path = repo.path().join(".libra").join("libra.db");
    let original_mode = fs::metadata(&db_path).unwrap().permissions().mode();

    fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o444)).unwrap();
    let second = run_libra_command(&["branch", "--set-upstream-to", "origin/main"], repo.path());
    fs::set_permissions(&db_path, std::fs::Permissions::from_mode(original_mode)).unwrap();

    assert_cli_success(&second, "idempotent set-upstream");
}

/// Scenario: `branch -D <name>` must print "Deleted branch <name> (was
/// <hash>)" on stdout. Pins the force-delete confirmation message.
#[test]
fn test_branch_force_delete_outputs_confirmation() {
    let repo = create_committed_repo_via_cli();

    let create = run_libra_command(&["branch", "topic"], repo.path());
    assert_cli_success(&create, "branch topic");

    let output = run_libra_command(&["branch", "-D", "topic"], repo.path());
    assert_cli_success(&output, "branch -D topic");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Deleted branch topic (was "),
        "unexpected stdout: {stdout}"
    );
}

/// Scenario: `libra branch --help` must render the `--set-upstream-to` doc
/// in a readable form. Previously the doc comment carried garbled
/// backtick/angle-bracket escaping (`\`branchname\`>\`'s tracking …`)
/// which leaked verbatim into the help text. This guard pins the cleaned
/// wording and prevents the bad pattern from re-appearing.
#[test]
fn test_branch_set_upstream_help_is_readable() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["branch", "--help"], repo.path());
    assert_cli_success(&output, "branch --help");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("`>`'s"),
        "branch --help still contains garbled doc fragment `>`'s — see \
         src/command/branch.rs::set_upstream_to docstring. Got:\n{stdout}"
    );
    assert!(
        stdout.contains("tracking information"),
        "branch --help missing readable --set-upstream-to description. Got:\n{stdout}"
    );
}

/// Scenario: in-process happy path for branch creation:
/// 1. Two empty commits on `main` produce two distinct commit hashes.
/// 2. Creating `first_branch` at the older hash must record that hash.
/// 3. Creating `second_branch` without an explicit start point must
///    inherit the current HEAD hash.
/// Also exercises `--show-current` (output not asserted, just non-panic).
#[tokio::test]
#[serial]
async fn test_branch() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    let commit_args = CommitArgs {
        message: Some("first".to_string()),
        file: None,
        allow_empty: true,
        conventional: false,
        amend: false,
        no_edit: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    };
    commit::execute(commit_args).await;
    let first_commit_id = Branch::find_branch_result("main", None)
        .await
        .expect("failed to query main branch")
        .expect("main branch should exist")
        .commit;

    let commit_args = CommitArgs {
        message: Some("second".to_string()),
        file: None,
        allow_empty: true,
        conventional: false,
        amend: false,
        no_edit: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    };
    commit::execute(commit_args).await;
    let second_commit_id = Branch::find_branch_result("main", None)
        .await
        .expect("failed to query main branch")
        .expect("main branch should exist")
        .commit;

    {
        // create branch with first commit
        let first_branch_name = "first_branch".to_string();
        let args = BranchArgs {
            subcommand: None,
            format: None,
            no_column: false,
            new_branch: Some(first_branch_name.clone()),
            commit_hash: Some(first_commit_id.to_string()),
            list: false,
            delete: None,
            delete_safe: None,
            set_upstream_to: None,
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
        };
        execute(args).await;

        // check branch exist
        match Head::current().await {
            Head::Branch(current_branch) => {
                assert_ne!(current_branch, first_branch_name)
            }
            _ => panic!("should be branch"),
        };

        let first_branch = Branch::find_branch_result(&first_branch_name, None)
            .await
            .expect("failed to query first branch")
            .expect("first_branch should exist");
        assert_eq!(first_branch.commit, first_commit_id);
        assert_eq!(first_branch.name, first_branch_name);
    }

    {
        // create second branch with current branch
        let second_branch_name = "second_branch".to_string();
        let args = BranchArgs {
            subcommand: None,
            format: None,
            no_column: false,
            new_branch: Some(second_branch_name.clone()),
            commit_hash: None,
            list: false,
            delete: None,
            delete_safe: None,
            set_upstream_to: None,
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
        };
        execute(args).await;
        let second_branch = Branch::find_branch_result(&second_branch_name, None)
            .await
            .expect("failed to query second branch")
            .expect("second_branch should exist");
        assert_eq!(second_branch.commit, second_commit_id);
        assert_eq!(second_branch.name, second_branch_name);
    }

    // show current branch
    println!("show current branch");
    let args = BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: None,
        commit_hash: None,
        list: false,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
        unset_upstream: None,
        edit_description: None,
        show_current: true,
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
    };
    execute(args).await;

    // list branches
    println!("list branches");
    // execute(BranchArgs::parse_from([""])).await; // default list
}

/// Scenario: a local branch can be created from `origin/main` (a
/// remote-tracking ref). Verifies the resulting branch points to the
/// same hash that the remote ref recorded.
#[tokio::test]
#[serial]
async fn test_create_branch_from_remote() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    test::init_debug_logger();

    let args = CommitArgs {
        message: Some("first".to_string()),
        file: None,
        allow_empty: true,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    };
    commit::execute(args).await;
    let hash = Head::current_commit().await.unwrap();
    Branch::update_branch("main", &hash.to_string(), Some("origin"))
        .await
        .unwrap(); // create remote branch
    assert!(get_target_commit("origin/main").await.is_ok());

    let args = BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: Some("test_new".to_string()),
        commit_hash: Some("origin/main".into()),
        list: false,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
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
    };
    execute(args).await;

    let branch = Branch::find_branch_result("test_new", None)
        .await
        .expect("failed to query test_new branch")
        .expect("branch create failed found");
    assert_eq!(branch.commit, hash);
}

/// Scenario: branch creation accepts the fully-qualified
/// `refs/remotes/origin/main` form (in addition to the short `origin/main`
/// form covered by the previous test). Confirms ref resolution accepts
/// both spellings.
#[tokio::test]
#[serial]
async fn test_create_branch_from_remote_tracking_ref() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    commit::execute(CommitArgs {
        message: Some("first".to_string()),
        allow_empty: true,
        disable_pre: true,
        no_verify: false,
        ..Default::default()
    })
    .await;

    let hash = Head::current_commit().await.unwrap();
    Branch::update_branch(
        "refs/remotes/origin/main",
        &hash.to_string(),
        Some("origin"),
    )
    .await
    .unwrap();

    assert!(get_target_commit("origin/main").await.is_ok());

    execute(BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: Some("tracking-copy".to_string()),
        commit_hash: Some("origin/main".into()),
        list: false,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
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

    let branch = Branch::find_branch_result("tracking-copy", None)
        .await
        .expect("failed to query tracking-copy branch")
        .expect("branch create from tracking ref failed");
    assert_eq!(branch.commit, hash);
}

/// Scenario: corrupt HEAD storage (the `main` ref points at a
/// non-existent hash) must surface as `LBR-REPO-002` (exit 128) when
/// trying to create a branch off HEAD, with messages "failed to resolve
/// HEAD commit" and "stored branch reference 'main' is corrupt". The
/// inner block uses a guard so the corruption is applied with the test
/// CWD set to the repo before reverting.
#[tokio::test]
#[serial]
async fn test_branch_create_without_base_surfaces_corrupt_head_storage() {
    let repo = create_committed_repo_via_cli();
    {
        let _guard = ChangeDirGuard::new(repo.path());
        Branch::update_branch("main", "not-a-valid-hash", None)
            .await
            .unwrap();
    }

    let output = run_libra_command(&["branch", "feature"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert!(
        stderr.contains("failed to resolve HEAD commit"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        stderr.contains("stored branch reference 'main' is corrupt"),
        "unexpected stderr: {stderr}"
    );
}

/// Scenario: same corruption pattern as above, but exercised through
/// `branch -d`. The safe-delete path must also surface `LBR-REPO-002`
/// with the corrupt-HEAD message rather than crash or report a misleading
/// "branch not merged" error.
#[tokio::test]
#[serial]
async fn test_branch_delete_safe_surfaces_corrupt_head_storage() {
    let repo = create_committed_repo_via_cli();
    let create = run_libra_command(&["branch", "topic"], repo.path());
    assert_cli_success(&create, "branch topic");

    {
        let _guard = ChangeDirGuard::new(repo.path());
        Branch::update_branch("main", "not-a-valid-hash", None)
            .await
            .unwrap();
    }

    let output = run_libra_command(&["branch", "-d", "topic"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert!(
        stderr.contains("failed to resolve HEAD commit"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        stderr.contains("stored branch reference 'main' is corrupt"),
        "unexpected stderr: {stderr}"
    );
}

/// Scenario: same corruption pattern, exercised through
/// `branch --show-current`. The display-only path must NOT silently
/// succeed when HEAD storage is broken; it must surface `LBR-REPO-002`.
#[tokio::test]
#[serial]
async fn test_branch_show_current_surfaces_corrupt_head_storage() {
    let repo = create_committed_repo_via_cli();
    {
        let _guard = ChangeDirGuard::new(repo.path());
        Branch::update_branch("main", "not-a-valid-hash", None)
            .await
            .unwrap();
    }

    let output = run_libra_command(&["branch", "--show-current"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert!(
        stderr.contains("failed to resolve HEAD commit"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        stderr.contains("stored branch reference 'main' is corrupt"),
        "unexpected stderr: {stderr}"
    );
}

/// Scenario: a stray branch with an invalid commit hash
/// (`broken-topic`) must trip the listing path with `LBR-REPO-002` and a
/// "stored branch reference 'broken-topic' is corrupt" message. Confirms
/// listing validates every branch row, not only HEAD.
#[tokio::test]
#[serial]
async fn test_branch_list_surfaces_corrupt_reference_name() {
    let repo = create_committed_repo_via_cli();
    {
        let _guard = ChangeDirGuard::new(repo.path());
        Branch::update_branch("broken-topic", "not-a-valid-hash", None)
            .await
            .unwrap();
    }

    let output = run_libra_command(&["branch"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-002");
    assert!(
        stderr.contains("stored branch reference 'broken-topic' is corrupt"),
        "unexpected stderr: {stderr}"
    );
}

/// Scenario: branch names rejected by `is_valid_git_branch_name`
/// (e.g. `@{mega}`) must not be created. Asserts both the validator's
/// return value and the post-condition that the branch does not exist.
#[tokio::test]
#[serial]
async fn test_invalid_branch_name() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    test::init_debug_logger();

    let args = CommitArgs {
        message: Some("first".to_string()),
        file: None,
        allow_empty: true,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    };
    commit::execute(args).await;

    // Check validation logic directly
    assert!(!libra::command::branch::is_valid_git_branch_name("@{mega}"));

    // Ensure no branch was created
    let branch = Branch::find_branch_result("@{mega}", None)
        .await
        .expect("failed to query @{mega} branch");
    assert!(branch.is_none(), "invalid branch should not be created");
}

/// Scenario: `branch -m old new` renames a non-current branch. Verifies
/// the old name no longer resolves and the new name carries the same
/// commit hash.
#[tokio::test]
#[serial]
async fn test_branch_rename() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    test::init_debug_logger();

    // Create initial commit
    let args = CommitArgs {
        message: Some("first".to_string()),
        file: None,
        allow_empty: true,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    };
    commit::execute(args).await;
    let commit_id_1 = Head::current_commit().await.unwrap();

    // Create a test branch
    let args = BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: Some("old_name".to_string()),
        commit_hash: None,
        list: false,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
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
    };
    execute(args).await;

    // Verify old branch exists
    let old_branch = Branch::find_branch_result("old_name", None)
        .await
        .expect("failed to query old_name branch");
    assert!(old_branch.is_some(), "old branch should exist");
    assert_eq!(old_branch.unwrap().commit, commit_id_1);

    // Rename branch from old_name to new_name
    let args = BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: None,
        commit_hash: None,
        list: false,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
        unset_upstream: None,
        edit_description: None,
        show_current: false,
        rename: vec!["old_name".to_string(), "new_name".to_string()],
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
    };
    execute(args).await;

    // Verify old branch no longer exists
    let old_branch = Branch::find_branch_result("old_name", None)
        .await
        .expect("failed to query old_name branch");
    assert!(
        old_branch.is_none(),
        "old branch should not exist after rename"
    );

    // Verify new branch exists with same commit
    let new_branch = Branch::find_branch_result("new_name", None)
        .await
        .expect("failed to query new_name branch");
    assert!(new_branch.is_some(), "new branch should exist");
    assert_eq!(new_branch.unwrap().commit, commit_id_1);
}

/// Scenario: renaming the currently checked-out branch must update HEAD
/// to the new name. Uses the single-argument `rename: vec![new]` form
/// which renames *the current* branch. Pins the HEAD-follows-rename
/// invariant.
#[tokio::test]
#[serial]
async fn test_rename_current_branch() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    test::init_debug_logger();

    // Create initial commit
    let args = CommitArgs {
        message: Some("first".to_string()),
        file: None,
        allow_empty: true,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    };
    commit::execute(args).await;
    let commit_id = Head::current_commit().await.unwrap();

    // Verify we're on main branch
    match Head::current().await {
        Head::Branch(name) => assert_eq!(name, "main"),
        _ => panic!("should be on a branch"),
    }

    // Create and switch to a feature branch
    let feature_branch = "feature".to_string();
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: None,
        create: Some(feature_branch.clone()),
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    // Verify we're on feature branch
    match Head::current().await {
        Head::Branch(name) => assert_eq!(name, "feature"),
        _ => panic!("should be on feature branch"),
    }

    // Rename current branch (feature) to feature_new using single argument
    let feature_new = "feature_new".to_string();
    let args = BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: None,
        commit_hash: None,
        list: false,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
        unset_upstream: None,
        edit_description: None,
        show_current: false,
        rename: vec![feature_new.clone()],
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
    };
    execute(args).await;

    // Verify HEAD is now on 'feature_new'
    match Head::current().await {
        Head::Branch(name) => assert_eq!(name, feature_new),
        _ => panic!("should be on a branch"),
    }

    // Verify old branch no longer exists
    let old_branch = Branch::find_branch_result(&feature_branch, None)
        .await
        .expect("failed to query feature branch");
    assert!(
        old_branch.is_none(),
        "feature branch should not exist after rename"
    );

    // Verify new branch exists with same commit
    let new_branch = Branch::find_branch_result(&feature_new, None)
        .await
        .expect("failed to query feature_new branch");
    assert!(new_branch.is_some(), "feature_new branch should exist");
    assert_eq!(new_branch.unwrap().commit, commit_id);
}

/// Scenario: renaming `branch1` to `branch2` while `branch2` already
/// exists must fail and leave both branches intact. Pins the
/// "no overwrite without -M" guard.
#[tokio::test]
#[serial]
async fn test_rename_to_existing_branch() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    test::init_debug_logger();

    // Create initial commit
    let args = CommitArgs {
        message: Some("first".to_string()),
        file: None,
        allow_empty: true,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    };
    commit::execute(args).await;

    // Create two branches
    let args = BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: Some("branch1".to_string()),
        commit_hash: None,
        list: false,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
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
    };
    execute(args).await;

    let args = BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: Some("branch2".to_string()),
        commit_hash: None,
        list: false,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
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
    };
    execute(args).await;

    // Try to rename branch1 to branch2 (should fail)
    let args = BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: None,
        commit_hash: None,
        list: false,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
        unset_upstream: None,
        edit_description: None,
        show_current: false,
        rename: vec!["branch1".to_string(), "branch2".to_string()],
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
    };
    execute(args).await;

    // Verify both branches still exist
    assert!(
        Branch::find_branch_result("branch1", None)
            .await
            .expect("failed to query branch1")
            .is_some()
    );
    assert!(
        Branch::find_branch_result("branch2", None)
            .await
            .expect("failed to query branch2")
            .is_some()
    );
}

/// Scenario: `branch -a` must list both local and remote branches
/// without crashing. The output is not directly captured (it just goes
/// to stdout); the assertion is that both local (`feature_branch`) and
/// remote (`origin/remote_branch`) refs resolve through `Branch::find_branch`.
#[tokio::test]
#[serial]
async fn test_list_all_branches() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    test::init_debug_logger();

    // Create initial commit
    let args = CommitArgs {
        message: Some("initial commit".to_string()),
        file: None,
        allow_empty: true,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    };
    commit::execute(args).await;

    // Create local branch
    let args = BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: Some("feature_branch".to_string()),
        commit_hash: None,
        list: false,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
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
    };
    execute(args).await;

    ConfigKv::set("remote.origin.url", "https://example.com/repo.git", false)
        .await
        .unwrap();

    // Create remote branch
    let hash = Head::current_commit().await.unwrap();
    Branch::update_branch("remote_branch", &hash.to_string(), Some("origin"))
        .await
        .unwrap();

    // Test -a parameter - just call execute, don't try to capture output
    let args = BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: None,
        commit_hash: None,
        list: false,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
        unset_upstream: None,
        edit_description: None,
        show_current: false,
        rename: vec![],
        copy: vec![],
        copy_force: vec![],
        remotes: false,
        all: true,
        contains: vec![],
        no_contains: vec![],
        points_at: None,
        merged: None,
        no_merged: None,
        sort: None,
        ignore_case: false,
        column: None,
        verbose: 0,
    };
    execute(args).await; // This will print to stdout, which is fine for tests

    // Verify branches exist
    assert!(
        Branch::find_branch_result("main", None)
            .await
            .expect("failed to query main branch")
            .is_some()
    );
    assert!(
        Branch::find_branch_result("feature_branch", None)
            .await
            .expect("failed to query feature_branch")
            .is_some()
    );
    assert!(
        Branch::find_branch_result("remote_branch", Some("origin"))
            .await
            .expect("failed to query remote_branch")
            .is_some()
    );
}

/// Scenario: `branch -d <name>` must refuse to delete an unmerged branch
/// and succeed once the branch has been merged into the current head.
/// Uses a fast-forward "merge" by directly updating `main` to the
/// feature branch's commit. Pins the safe-delete merge gate.
#[tokio::test]
#[serial]
async fn test_branch_delete_safe() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create first commit on master
    let commit_args = CommitArgs {
        message: Some("initial commit".to_string()),
        file: None,
        allow_empty: true,
        conventional: false,
        amend: false,
        no_edit: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    };
    commit::execute(commit_args).await;

    // Create a feature branch
    execute(BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: Some("feature".to_string()),
        commit_hash: None,
        list: false,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
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

    // Switch to feature branch and make a commit
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    let commit_args = CommitArgs {
        message: Some("feature work".to_string()),
        file: None,
        allow_empty: true,
        conventional: false,
        amend: false,
        no_edit: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    };
    commit::execute(commit_args).await;

    // Switch back to master
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    // Try to delete feature branch with -d (should fail - not merged)
    execute(BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: None,
        commit_hash: None,
        list: false,
        delete: None,
        delete_safe: Some("feature".to_string()),
        set_upstream_to: None,
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

    // Feature branch should still exist
    assert!(
        Branch::find_branch_result("feature", None)
            .await
            .expect("failed to query feature branch")
            .is_some()
    );

    // Now merge feature into master
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("main".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    // Fast-forward merge (just update master to feature's commit)
    let feature_commit = Branch::find_branch_result("feature", None)
        .await
        .expect("failed to query feature branch")
        .expect("feature branch should exist")
        .commit;
    Branch::update_branch("main", &feature_commit.to_string(), None)
        .await
        .unwrap();

    // Now try -d again (should succeed - fully merged)
    execute(BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: None,
        commit_hash: None,
        list: false,
        delete: None,
        delete_safe: Some("feature".to_string()),
        set_upstream_to: None,
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

    // Feature branch should be deleted
    assert!(
        Branch::find_branch_result("feature", None)
            .await
            .expect("failed to query feature branch")
            .is_none()
    );
}

/// Scenario: comprehensive coverage of `--contains` and `--no-contains`
/// filter semantics over a divergent branch topology:
///
/// ```text
///   master:  base ← m1 ← m2
///             ↖
///   dev:        d1 ← d2
/// ```
///
/// Where:
/// - `base`: common ancestor, reachable from both branches
/// - `m1`, `m2`: commits unique to master
/// - `d1`, `d2`: commits unique to dev (d1 branches from base, d2 extends d1)
///
/// Tests cover:
/// 1. Single filters (`--contains` or `--no-contains` alone)
/// 2. Combined filters (`--contains` AND `--no-contains`)
/// 3. Multiple values (OR semantics for `--contains`, AND for `--no-contains`)
/// 4. Chain dependency edge cases (e.g. `--contains d1 --no-contains d2`
///    is empty because d2 contains d1).
///
/// The `libra/intent` agent branch is filtered out before assertions to
/// keep the expected sets clean.
#[tokio::test]
#[serial]
async fn test_branch_contains_commit_filter() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    test::init_debug_logger();

    let main_branch = match Head::current().await {
        Head::Branch(name) => name,
        _ => panic!("expected to start on a branch"),
    };

    let make_commit = |msg: &str| CommitArgs {
        message: Some(msg.to_string()),
        file: None,
        allow_empty: true,
        conventional: false,
        amend: false,
        no_edit: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    };

    // ================================================================
    //  Build commit graph: divergent branches with shared ancestor
    // ================================================================

    // Common ancestor
    commit::execute(make_commit("base")).await;
    let base = Head::current_commit().await.unwrap().to_string();

    // Create dev branch and add two commits
    execute(BranchArgs {
        subcommand: None,
        format: None,
        no_column: false,
        new_branch: Some("dev".to_string()),
        commit_hash: None,
        list: false,
        delete: None,
        delete_safe: None,
        set_upstream_to: None,
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

    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some("dev".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    commit::execute(make_commit("d1")).await;
    let d1 = Head::current_commit().await.unwrap().to_string();

    commit::execute(make_commit("d2")).await;
    let d2 = Head::current_commit().await.unwrap().to_string();

    // Return to main branch and add two commits
    switch::execute(SwitchArgs {
        no_progress: false,
        branch: Some(main_branch.clone()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    })
    .await;

    commit::execute(make_commit("m1")).await;
    let m1 = Head::current_commit().await.unwrap().to_string();

    commit::execute(make_commit("m2")).await;
    let m2 = Head::current_commit().await.unwrap().to_string();

    // -- Helper: resolve commits from `&[String]` to `HashSet<ObjectHash>`
    let resolve_commits = async |commits: &[String]| {
        let mut set = HashSet::new();
        for commit in commits {
            let target_commit = match get_target_commit(commit).await {
                Ok(commit) => commit,
                Err(e) => panic!("fatal: {e}"),
            };
            set.insert(target_commit);
        }
        set
    };

    // -- Helper: filter and return sorted branch names --
    let run_filter = |contains: &[&str], no_contains: &[&str]| {
        let contains: Vec<String> = contains.iter().map(|s| s.to_string()).collect();
        let no_contains: Vec<String> = no_contains.iter().map(|s| s.to_string()).collect();
        async move {
            let mut branches = Branch::list_branches_result(None)
                .await
                .expect("failed to list branches");
            branches.retain(|b| b.name != "libra/intent");
            filter_branches(
                &mut branches,
                &resolve_commits(&contains).await,
                &resolve_commits(&no_contains).await,
            )
            .unwrap();
            let mut names: Vec<String> = branches.into_iter().map(|b| b.name).collect();
            names.sort();
            names
        }
    };

    let sorted = |names: &[&str]| -> Vec<String> {
        let mut v: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        v.sort();
        v
    };

    // ================================================================
    //  Test single `--contains` filter
    // ================================================================

    // Common ancestor is in both branches
    assert_eq!(
        run_filter(&[&base], &[]).await,
        sorted(&[&main_branch, "dev"]),
        "`--contains base` should match both branches"
    );

    // Branch-specific commits
    assert_eq!(
        run_filter(&[&d1], &[]).await,
        sorted(&["dev"]),
        "`--contains d1` should match only dev"
    );

    assert_eq!(
        run_filter(&[&d2], &[]).await,
        sorted(&["dev"]),
        "`--contains d2` (tip of dev) should match only dev"
    );

    assert_eq!(
        run_filter(&[&m1], &[]).await,
        sorted(&[&main_branch]),
        "`--contains m1` should match only master"
    );

    assert_eq!(
        run_filter(&[&m2], &[]).await,
        sorted(&[&main_branch]),
        "`--contains m2` (tip of master) should match only master"
    );

    // ================================================================
    //  Test single `--no-contains` filter
    // ================================================================

    // Excluding common ancestor filters out everything
    assert_eq!(
        run_filter(&[], &[&base]).await,
        sorted(&[]),
        "`--no-contains base` should match nothing"
    );

    // Excluding branch-specific commits
    assert_eq!(
        run_filter(&[], &[&d1]).await,
        sorted(&[&main_branch]),
        "`--no-contains d1` should match only master"
    );

    assert_eq!(
        run_filter(&[], &[&m1]).await,
        sorted(&["dev"]),
        "`--no-contains m1` should match only dev"
    );

    // ================================================================
    //  Test multiple `--contains` (OR semantics)
    // ================================================================

    // Any branch containing d1 OR m1
    assert_eq!(
        run_filter(&[&d1, &m1], &[]).await,
        sorted(&[&main_branch, "dev"]),
        "`--contains d1 --contains m1` should match both (OR)"
    );

    // Any branch containing d2 OR m2 (both tips)
    assert_eq!(
        run_filter(&[&d2, &m2], &[]).await,
        sorted(&[&main_branch, "dev"]),
        "`--contains d2 --contains m2` should match both (OR)"
    );

    // ================================================================
    //  Test multiple `--no-contains` (AND semantics)
    // ================================================================

    // Branches excluding both d1 AND m1 → none (each branch has one)
    assert_eq!(
        run_filter(&[], &[&d1, &m1]).await,
        sorted(&[]),
        "`--no-contains d1 --no-contains m1` should match nothing (each branch has one)"
    );

    // ================================================================
    //  Test combined `--contains` and `--no-contains`
    // ================================================================

    // Branches with base but not m1 → dev
    assert_eq!(
        run_filter(&[&base], &[&m1]).await,
        sorted(&["dev"]),
        "`--contains base --no-contains m1` should match dev"
    );

    // Branches with base but not d1 → master
    assert_eq!(
        run_filter(&[&base], &[&d1]).await,
        sorted(&[&main_branch]),
        "`--contains base --no-contains d1` should match master"
    );

    // Branches with base but not m2 → dev
    assert_eq!(
        run_filter(&[&base], &[&m2]).await,
        sorted(&["dev"]),
        "`--contains base --no-contains m2` should match dev"
    );

    // Branches with d1 OR m1, but not d2 → only master (dev is excluded by d2)
    assert_eq!(
        run_filter(&[&d1, &m1], &[&d2]).await,
        sorted(&[&main_branch]),
        "`--contains d1 --contains m1 --no-contains d2` should match master"
    );

    // Branches with d1 OR m1, but not m2 → only dev (master is excluded by m2)
    assert_eq!(
        run_filter(&[&d1, &m1], &[&m2]).await,
        sorted(&["dev"]),
        "`--contains d1 --contains m1 --no-contains m2` should match dev"
    );

    // ================================================================
    //  Test edge cases
    // ================================================================

    // Chain dependency: d2 contains d1, so `--contains d1 --no-contains d2` → empty
    assert_eq!(
        run_filter(&[&d1], &[&d2]).await,
        sorted(&[]),
        "`--contains d1 --no-contains d2` should match nothing (d2 contains d1)"
    );

    // Similarly for master chain
    assert_eq!(
        run_filter(&[&m1], &[&m2]).await,
        sorted(&[]),
        "`--contains m1 --no-contains m2` should match nothing (m2 contains m1)"
    );

    // Branches with base but excluding both tips → none
    assert_eq!(
        run_filter(&[&base], &[&d2, &m2]).await,
        sorted(&[]),
        "`--contains base --no-contains d2 --no-contains m2` should match nothing"
    );
}

/// Scenario: `filter_branches` must propagate (not swallow) errors when
/// a branch row points at a non-existent commit hash. The BFS inside
/// `commit_contains` should fail to load the bogus commit, and the
/// outer call must surface that error with a "failed to load commit"
/// message. Regression guard for silent-skip bugs.
#[test]
#[serial]
fn test_filter_branches_propagates_error_for_corrupt_commit() {
    use std::str::FromStr;

    use git_internal::hash::ObjectHash;
    use libra::internal::branch::Branch;

    let temp_path = tempdir().unwrap();
    init_repo_via_cli(temp_path.path());
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Fabricate a branch whose commit hash does not exist in any storage.
    let bogus_hash =
        ObjectHash::from_str("0000000000000000000000000000000000000000000000000000000000000000")
            .expect("valid hex");
    let corrupt_branch = Branch {
        name: "corrupt".into(),
        commit: bogus_hash,
        remote: None,
    };

    // `contains_set` with a real-looking hash forces BFS traversal.
    let mut branches = vec![corrupt_branch];
    let mut contains = HashSet::new();
    contains.insert(
        ObjectHash::from_str("1111111111111111111111111111111111111111111111111111111111111111")
            .expect("valid hex"),
    );
    let no_contains = HashSet::new();

    let result = filter_branches(&mut branches, &contains, &no_contains);
    assert!(
        result.is_err(),
        "filter_branches should propagate error for corrupt commit, got Ok"
    );
    let err = result.unwrap_err();
    assert!(
        err.message().contains("failed to load commit"),
        "error should mention failed commit load, got: {}",
        err.message()
    );
}

#[test]
fn branch_merged_and_no_merged_filters() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // `old` stays at the base commit; once `main` advances, `old` is merged into it.
    assert_cli_success(&run_libra_command(&["branch", "old"], p), "branch old");
    std::fs::write(p.join("m.txt"), "m\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "m.txt"], p), "add m");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2 on main",
    );

    // `side` diverges with its own commit, so it is NOT merged into main.
    assert_cli_success(&run_libra_command(&["branch", "side"], p), "branch side");
    assert_cli_success(&run_libra_command(&["switch", "side"], p), "switch side");
    std::fs::write(p.join("s.txt"), "s\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "s.txt"], p), "add s");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c3", "--no-verify"], p),
        "commit c3 on side",
    );
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");

    // --merged main: main + old (reachable from main), NOT side.
    let merged = run_libra_command(&["branch", "--merged", "main"], p);
    assert_cli_success(&merged, "branch --merged main");
    let m = String::from_utf8_lossy(&merged.stdout).into_owned();
    assert!(
        m.contains("old") && m.contains("main"),
        "merged has old+main: {m:?}"
    );
    assert!(
        !m.contains("side"),
        "side must not be merged into main: {m:?}"
    );

    // --no-merged main: side, NOT old/main.
    let no_merged = run_libra_command(&["branch", "--no-merged", "main"], p);
    assert_cli_success(&no_merged, "branch --no-merged main");
    let n = String::from_utf8_lossy(&no_merged.stdout).into_owned();
    assert!(n.contains("side"), "side is not merged into main: {n:?}");
    assert!(!n.contains("old"), "old IS merged into main: {n:?}");
}

#[test]
fn branch_sort_orders_list_by_key() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    for b in ["zeta", "alpha", "v1.9", "v1.10"] {
        assert_cli_success(&run_libra_command(&["branch", b], p), b);
    }
    let names = |out: &std::process::Output| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim_start_matches("* ").trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
    };

    // refname: lexicographic ascending.
    let asc = run_libra_command(&["branch", "--sort", "refname"], p);
    assert_cli_success(&asc, "branch --sort refname");
    let a = names(&asc);
    let mut sorted_a = a.clone();
    sorted_a.sort();
    assert_eq!(a, sorted_a, "ascending refname order: {a:?}");

    // -refname reverses.
    let desc = run_libra_command(&["branch", "--sort=-refname"], p);
    assert_cli_success(&desc, "branch --sort=-refname");
    let d = names(&desc);
    let mut rd = d.clone();
    rd.sort();
    rd.reverse();
    assert_eq!(d, rd, "descending refname order: {d:?}");

    // version:refname is numeric-aware: v1.9 sorts before v1.10.
    let ver = run_libra_command(&["branch", "--sort", "version:refname"], p);
    assert_cli_success(&ver, "branch --sort version:refname");
    let v = names(&ver);
    let p9 = v.iter().position(|x| x == "v1.9");
    let p10 = v.iter().position(|x| x == "v1.10");
    assert!(
        p9 < p10,
        "v1.9 must sort before v1.10 (numeric-aware): {v:?}"
    );

    // Unknown key is a usage error.
    assert!(
        !run_libra_command(&["branch", "--sort", "bogus"], p)
            .status
            .success(),
        "unknown sort key must be rejected"
    );
}

#[test]
fn branch_copy_duplicates_branch_with_config() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["branch", "feat"], p), "branch feat");
    // Give `feat` an upstream config so the copy can carry it over.
    assert_cli_success(
        &run_libra_command(&["config", "branch.feat.remote", "origin"], p),
        "config remote",
    );
    assert_cli_success(
        &run_libra_command(&["config", "branch.feat.merge", "refs/heads/main"], p),
        "config merge",
    );

    // -c copies feat -> feat2, keeping feat.
    assert_cli_success(
        &run_libra_command(&["branch", "-c", "feat", "feat2"], p),
        "branch -c",
    );
    let list = run_libra_command(&["branch"], p);
    let names = String::from_utf8_lossy(&list.stdout).into_owned();
    assert!(names.contains("feat2"), "copy must exist: {names:?}");
    assert!(names.contains("feat"), "source must remain: {names:?}");

    // The upstream config is copied to feat2.
    let cfg = run_libra_command(&["config", "get", "branch.feat2.remote"], p);
    assert_cli_success(&cfg, "config get feat2.remote");
    assert!(
        String::from_utf8_lossy(&cfg.stdout).contains("origin"),
        "copied branch must inherit the upstream config"
    );

    // Copying onto an existing branch without -C is an error.
    let dup = run_libra_command(&["branch", "-c", "feat", "feat2"], p);
    assert!(
        !dup.status.success(),
        "-c onto an existing branch must fail"
    );

    // -C overwrites the existing destination.
    assert_cli_success(
        &run_libra_command(&["branch", "-C", "main", "feat2"], p),
        "branch -C overwrites",
    );

    // Even -C refuses to overwrite the checked-out branch (HEAD is on main).
    let onto_current = run_libra_command(&["branch", "-C", "feat", "main"], p);
    assert!(
        !onto_current.status.success(),
        "-C onto the current branch must be rejected"
    );
}

#[test]
fn branch_column_lays_out_in_columns() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    for b in ["aa", "bb", "cc", "dd", "ee"] {
        assert_cli_success(&run_libra_command(&["branch", b], p), "branch");
    }

    // --column packs multiple branches per line; the plain listing is one per line.
    let col = run_libra_command(&["branch", "--column"], p);
    assert_cli_success(&col, "branch --column");
    let col_out = String::from_utf8_lossy(&col.stdout);
    let col_lines = col_out.lines().filter(|l| !l.trim().is_empty()).count();

    let plain = run_libra_command(&["branch"], p);
    assert_cli_success(&plain, "branch");
    let plain_out = String::from_utf8_lossy(&plain.stdout);
    let plain_lines = plain_out.lines().filter(|l| !l.trim().is_empty()).count();

    assert!(
        plain_lines >= 6,
        "plain list is one branch per line: {plain_out:?}"
    );
    assert!(
        col_lines < plain_lines,
        "columns are more compact ({col_lines} < {plain_lines}): {col_out:?}"
    );
    // All branches are still present in the columnar output.
    for b in ["main", "aa", "bb", "cc", "dd", "ee"] {
        assert!(
            col_out.contains(b),
            "column output contains {b}: {col_out:?}"
        );
    }
}

#[test]
fn branch_verbose_shows_sha_and_subject() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "create feature branch",
    );

    // Plain listing is name-only.
    let plain = run_libra_command(&["branch"], p);
    assert_cli_success(&plain, "branch");
    let plain_out = String::from_utf8_lossy(&plain.stdout);

    // `-v` appends the short sha and the commit subject to each branch line.
    let verbose = run_libra_command(&["branch", "-v"], p);
    assert_cli_success(&verbose, "branch -v");
    let v_out = String::from_utf8_lossy(&verbose.stdout);

    // The current-branch line under -v carries more than just the name.
    let plain_main = plain_out
        .lines()
        .find(|l| l.contains("* "))
        .expect("current branch line");
    let v_main = v_out
        .lines()
        .find(|l| l.contains("* "))
        .expect("current branch line (-v)");
    assert!(
        v_main.len() > plain_main.len(),
        "-v line is longer than the plain line: plain={plain_main:?} verbose={v_main:?}"
    );
    // Every branch line under -v has at least 3 whitespace-separated fields
    // (marker+name, sha, subject...).
    for line in v_out.lines().filter(|l| !l.trim().is_empty()) {
        assert!(
            line.split_whitespace().count() >= 3,
            "verbose line has sha + subject: {line:?}"
        );
    }
}

#[test]
fn branch_vv_shows_upstream_segment() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "create feature",
    );

    // Configure an upstream for `feature`.
    assert_cli_success(
        &run_libra_command(&["config", "branch.feature.remote", "origin"], p),
        "set remote",
    );
    assert_cli_success(
        &run_libra_command(&["config", "branch.feature.merge", "refs/heads/feature"], p),
        "set merge",
    );

    // `-vv` shows the upstream tracking segment for `feature`; the remote-tracking
    // ref is not fetched, so the ahead/behind counts are omitted.
    let vv = run_libra_command(&["branch", "-vv"], p);
    assert_cli_success(&vv, "branch -vv");
    let vv_out = String::from_utf8_lossy(&vv.stdout);
    let feature_line = vv_out
        .lines()
        .find(|l| l.contains("feature"))
        .expect("feature line");
    assert!(
        feature_line.contains("[origin/feature]"),
        "-vv shows the upstream segment: {feature_line:?}"
    );

    // `-v` does NOT show the upstream segment (only the sha + subject).
    let v = run_libra_command(&["branch", "-v"], p);
    assert_cli_success(&v, "branch -v");
    assert!(
        !String::from_utf8_lossy(&v.stdout).contains("[origin/feature]"),
        "-v omits the upstream segment"
    );
}

#[test]
fn branch_no_column_countermands_column() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    run_libra_command(&["branch", "aaaaa"], p);
    run_libra_command(&["branch", "bbbbb"], p);

    // `--no-column` alone lists one branch per line (the default).
    let plain = run_libra_command(&["branch", "--no-column"], p);
    assert!(
        plain.status.success(),
        "branch --no-column: {}",
        String::from_utf8_lossy(&plain.stderr)
    );

    // `--column=always --no-column` (last wins) countermands `--column`, so the
    // listing is one-per-line, NOT columnar (no two names share a line).
    let out = run_libra_command(&["branch", "--column=always", "--no-column"], p);
    assert!(out.status.success(), "branch --column=always --no-column");
    let listed = String::from_utf8_lossy(&out.stdout);
    assert!(
        !listed
            .lines()
            .any(|l| l.contains("aaaaa") && l.contains("bbbbb")),
        "--no-column countermands --column (one per line): {listed}"
    );
}

/// End-to-end `branch --edit-description`: an explicitly configured (scripted)
/// editor sets `branch.<name>.description`, and a comment-only buffer unsets it.
/// Uses GIT_EDITOR so no TTY is needed (the editor runs regardless of TTY when
/// explicitly configured).
#[cfg(unix)]
#[test]
fn branch_edit_description_sets_then_unsets_via_editor() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // Resolve the current branch name to query its description config key.
    let cur = run_libra_command(&["branch", "--show-current"], p);
    assert_cli_success(&cur, "branch --show-current");
    let branch = String::from_utf8_lossy(&cur.stdout).trim().to_string();
    assert!(!branch.is_empty(), "expected a current branch name");
    let key = format!("branch.{branch}.description");

    let write_editor = |name: &str, body: &str| -> String {
        let path = p.join(name);
        fs::write(&path, format!("#!/bin/sh\nprintf '%s' '{body}' > \"$1\"\n")).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path.to_string_lossy().into_owned()
    };

    // 1) A scripted editor writes a description, which is stored under the key.
    let set_editor = write_editor("set_editor.sh", "a tidy summary\n# stripped comment\n");
    let out = run_libra_command_with_stdin_and_env(
        &["branch", "--edit-description"],
        p,
        "",
        &[("GIT_EDITOR", set_editor.as_str())],
    );
    assert_cli_success(&out, "branch --edit-description (set)");

    let got = run_libra_command(&["config", "get", &key], p);
    assert_cli_success(&got, "config get description after set");
    assert!(
        String::from_utf8_lossy(&got.stdout).contains("a tidy summary"),
        "description should be stored: {}",
        String::from_utf8_lossy(&got.stdout)
    );

    // 2) A comment-only buffer cleans to empty, which unsets the key.
    let clear_editor = write_editor("clear_editor.sh", "# only a comment line\n");
    let out = run_libra_command_with_stdin_and_env(
        &["branch", "--edit-description"],
        p,
        "",
        &[("GIT_EDITOR", clear_editor.as_str())],
    );
    assert_cli_success(&out, "branch --edit-description (unset)");

    // The previous value must be gone (whether `config get` now fails or prints
    // nothing, the old description must not survive).
    let got = run_libra_command(&["config", "get", &key], p);
    assert!(
        !String::from_utf8_lossy(&got.stdout).contains("a tidy summary"),
        "description should be unset: {}",
        String::from_utf8_lossy(&got.stdout)
    );
}

#[test]
fn test_branch_sort_by_committer_date() {
    // `--sort=committerdate` orders branches by their tip commit's committer date
    // (oldest first), distinct from `--sort=refname`. The branch names are chosen
    // so the date order is the OPPOSITE of the alphabetical order, proving the date
    // key (not the name) drives the sort. A short sleep gives the two commits
    // distinct one-second-granularity timestamps.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // First (older) commit -> branch "zzz" (alphabetically last).
    std::fs::write(p.join("f.txt"), "1\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add 1");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit 1",
    );
    assert_cli_success(&run_libra_command(&["branch", "zzz"], p), "branch zzz");

    std::thread::sleep(std::time::Duration::from_millis(1200));

    // Second (newer) commit -> branch "aaa" (alphabetically first). A long
    // message makes this commit object materially larger than zzz's, so the
    // `objectsize` assertions below are driven by size, not the refname
    // tie-break.
    std::fs::write(p.join("f.txt"), "1\n2\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add 2");
    let long_msg = format!("c2 {}", "x".repeat(400));
    assert_cli_success(
        &run_libra_command(&["commit", "-m", &long_msg, "--no-verify"], p),
        "commit 2",
    );
    assert_cli_success(&run_libra_command(&["branch", "aaa"], p), "branch aaa");

    let order = |args: &[&str]| -> Vec<String> {
        let out = run_libra_command(args, p);
        assert_cli_success(&out, "branch sort");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim_start_matches(['*', ' ']).trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    };

    // `expect` both names so a missing branch fails loudly instead of a `None`
    // position satisfying a relative-order comparison.
    let pos = |list: &[String], name: &str| -> usize {
        list.iter()
            .position(|n| n == name)
            .unwrap_or_else(|| panic!("branch '{name}' missing from {list:?}"))
    };

    // committerdate ascending: zzz (older) before aaa (newer).
    let by_date = order(&["branch", "--sort=committerdate"]);
    assert!(
        pos(&by_date, "zzz") < pos(&by_date, "aaa"),
        "committerdate: older zzz must precede newer aaa: {by_date:?}"
    );
    // refname ascending: aaa before zzz (the OPPOSITE order — proves date != name).
    let by_name = order(&["branch", "--sort=refname"]);
    assert!(
        pos(&by_name, "aaa") < pos(&by_name, "zzz"),
        "refname: aaa must precede zzz: {by_name:?}"
    );
    // -committerdate reverses: newer aaa before older zzz.
    let by_date_rev = order(&["branch", "--sort=-committerdate"]);
    assert!(
        pos(&by_date_rev, "aaa") < pos(&by_date_rev, "zzz"),
        "-committerdate: newer aaa must precede older zzz: {by_date_rev:?}"
    );

    // creatordate uses the same committer-date basis for branches: same ordering.
    let by_creator = order(&["branch", "--sort=creatordate"]);
    assert!(
        pos(&by_creator, "zzz") < pos(&by_creator, "aaa"),
        "creatordate: older zzz must precede newer aaa: {by_creator:?}"
    );
    let by_creator_rev = order(&["branch", "--sort=-creatordate"]);
    assert!(
        pos(&by_creator_rev, "aaa") < pos(&by_creator_rev, "zzz"),
        "-creatordate: newer aaa must precede older zzz: {by_creator_rev:?}"
    );

    // authordate sorts by the tip commit's author date (oldest first), like
    // committerdate here: zzz (older) before aaa (newer), reversible.
    let by_author = order(&["branch", "--sort=authordate"]);
    assert!(
        pos(&by_author, "zzz") < pos(&by_author, "aaa"),
        "authordate: older zzz must precede newer aaa: {by_author:?}"
    );
    let by_author_rev = order(&["branch", "--sort=-authordate"]);
    assert!(
        pos(&by_author_rev, "aaa") < pos(&by_author_rev, "zzz"),
        "-authordate: newer aaa must precede older zzz: {by_author_rev:?}"
    );

    // objectsize sorts by the tip object's byte size: aaa's tip carries the long
    // message and is therefore larger than zzz's, so ascending object size puts
    // the smaller zzz before aaa; reversible.
    let by_size = order(&["branch", "--sort=objectsize"]);
    assert!(
        pos(&by_size, "zzz") < pos(&by_size, "aaa"),
        "objectsize: smaller root-commit zzz must precede larger aaa: {by_size:?}"
    );
    let by_size_rev = order(&["branch", "--sort=-objectsize"]);
    assert!(
        pos(&by_size_rev, "aaa") < pos(&by_size_rev, "zzz"),
        "-objectsize: larger aaa must precede smaller zzz: {by_size_rev:?}"
    );

    // objectname sorts by the tip commit's object id (lexicographic on the hex
    // hash, matching Git's binary-oid order), with the refname tie-break for
    // branches sharing a tip. Read the hash alongside each name and assert the
    // listing matches a hash-sorted expectation derived from the same data, so
    // the check is deterministic regardless of which hashes are produced.
    let with_hash = |args: &[&str]| -> Vec<(String, String)> {
        let out = run_libra_command(args, p);
        assert_cli_success(&out, "branch sort objectname");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let l = l.trim_start_matches(['*', ' ']).trim();
                let (h, n) = l.split_once(' ')?;
                Some((h.to_string(), n.to_string()))
            })
            .collect()
    };
    let asc = with_hash(&[
        "branch",
        "--sort=objectname",
        "--format=%(objectname) %(refname:short)",
    ]);
    let mut expect = asc.clone();
    expect.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    assert_eq!(
        asc, expect,
        "objectname: branches ordered by ascending tip hash"
    );
    let desc = with_hash(&[
        "branch",
        "--sort=-objectname",
        "--format=%(objectname) %(refname:short)",
    ]);
    let mut expect_rev = desc.clone();
    // `-` reverses the primary (hash) key but the refname tie-break stays ascending.
    expect_rev.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    assert_eq!(
        desc, expect_rev,
        "-objectname: branches ordered by descending tip hash"
    );

    // An unknown sort key is a usage error (exit 129).
    let bad = run_libra_command(&["branch", "--sort=bogus"], p);
    assert_eq!(bad.status.code(), Some(129), "unknown sort key exits 129");
}

/// `--sort=authordate` orders by the tip commit's AUTHOR date (not committer
/// date), and a reversed sort keeps the refname tie-break ascending. Uses two
/// crafted commits whose author/committer dates are swapped so `authordate` and
/// `committerdate` produce OPPOSITE orders — proving the author timestamp drives
/// `authordate`.
#[test]
fn test_branch_sort_authordate_uses_author_date_and_keeps_tiebreak() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    let tree = {
        let cat = run_libra_command(&["cat-file", "-p", "HEAD"], p);
        String::from_utf8_lossy(&cat.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("tree ").map(str::to_string))
            .expect("HEAD has a tree")
    };
    // Craft a commit with explicit (author_ts, committer_ts) and return its oid.
    let craft = |name: &str, author_ts: u64, committer_ts: u64| -> String {
        let body = format!(
            "tree {tree}\nauthor a <a@b> {author_ts} +0000\ncommitter a <a@b> {committer_ts} +0000\n\nmsg {name}\n"
        );
        let file = p.join(format!("{name}.commit"));
        std::fs::write(&file, body).unwrap();
        let out = run_libra_command(
            &[
                "hash-object",
                "-t",
                "commit",
                "--literally",
                "-w",
                file.to_str().unwrap(),
            ],
            p,
        );
        assert_cli_success(&out, "hash-object crafted commit");
        let oid = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_cli_success(
            &run_libra_command(&["branch", name, &oid], p),
            "branch -> crafted",
        );
        oid
    };
    // ax: author OLD (100), committer NEW (900). ay: the reverse.
    craft("ax", 100, 900);
    craft("ay", 900, 100);
    // tie_a / tie_z share ax's dates (author 100) to exercise the tie-break.
    craft("tie_a", 100, 900);
    craft("tie_z", 100, 900);

    let order = |args: &[&str]| -> Vec<String> {
        let out = run_libra_command(args, p);
        assert_cli_success(&out, "branch sort");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim_start_matches(['*', ' ']).trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    };
    let pos = |list: &[String], name: &str| -> usize {
        list.iter()
            .position(|n| n == name)
            .unwrap_or_else(|| panic!("branch '{name}' missing from {list:?}"))
    };

    // authordate: ax (author 100) before ay (author 900).
    let by_author = order(&["branch", "--sort=authordate"]);
    assert!(
        pos(&by_author, "ax") < pos(&by_author, "ay"),
        "authordate: ax (older author date) before ay: {by_author:?}"
    );
    // committerdate: OPPOSITE — ay (committer 100) before ax (committer 900).
    let by_committer = order(&["branch", "--sort=committerdate"]);
    assert!(
        pos(&by_committer, "ay") < pos(&by_committer, "ax"),
        "committerdate must use committer date (opposite of authordate): {by_committer:?}"
    );

    // Reversed sort keeps the refname tie-break ASCENDING: among the equal-author
    // (100) branches, tie_a must still precede tie_z under -authordate.
    let by_author_rev = order(&["branch", "--sort=-authordate"]);
    assert!(
        pos(&by_author_rev, "tie_a") < pos(&by_author_rev, "tie_z"),
        "-authordate keeps the refname tie-break ascending: {by_author_rev:?}"
    );
}

/// `branch --format` renders each branch via the for-each-ref atom engine,
/// replacing the default `* name` listing. `%(refname:short)`, `%(objectname)`,
/// `%(HEAD)`, and `%(if)` blocks all resolve.
#[test]
fn branch_format_renders_for_each_ref_atoms() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "branch feature",
    );

    let head = run_libra_command(&["rev-parse", "HEAD"], p);
    let full_oid = String::from_utf8_lossy(&head.stdout).trim().to_string();

    // %(refname) is the full ref; %(objectname) the full hash.
    let out = run_libra_command(&["branch", "--format=%(refname) %(objectname)"], p);
    assert_cli_success(&out, "branch --format");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains(&format!("refs/heads/feature {full_oid}")),
        "expected full refname + objectname line: {text}"
    );
    assert!(
        text.lines().all(|l| l.starts_with("refs/heads/")),
        "every line is a formatted ref, no `* ` marker: {text}"
    );

    // %(HEAD) marks the current branch; %(if)/%(then)/%(end) works.
    let marked = run_libra_command(
        &[
            "branch",
            "--format=%(refname:short)%(if)%(HEAD)%(then) <-%(end)",
        ],
        p,
    );
    assert_cli_success(&marked, "branch --format HEAD marker");
    let marked_text = String::from_utf8_lossy(&marked.stdout);
    let current =
        String::from_utf8_lossy(&run_libra_command(&["branch", "--show-current"], p).stdout)
            .trim()
            .to_string();
    assert!(
        marked_text.lines().any(|l| l == format!("{current} <-")),
        "current branch should carry the HEAD marker: {marked_text}"
    );
    assert!(
        marked_text.lines().any(|l| l == "feature"),
        "non-current branch should have no marker: {marked_text}"
    );
}
