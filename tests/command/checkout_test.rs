//! Tests checkout behavior for switching branches and creating new ones via restore logic.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use colored::Colorize;
use libra::{
    command::{
        add::{self, AddArgs},
        branch,
        checkout::{check_branch, get_current_branch, switch_branch},
        commit, init,
        restore::{self, RestoreArgs},
    },
    internal::{config::ConfigKv, head::Head},
    utils::{output::OutputConfig, test, util},
};
use serial_test::serial;
use tempfile::tempdir;

use super::*;

async fn configure_identity_for_test() {
    ConfigKv::set("user.name", "Checkout Test User", false)
        .await
        .unwrap();
    ConfigKv::set("user.email", "checkout-test@example.com", false)
        .await
        .unwrap();
}
async fn test_check_branch() {
    println!("\n\x1b[1mTest check_branch function.\x1b[0m");

    // For non-existent branches, it should return Err
    assert!(check_branch("non_existent_branch").await.is_err());
    // For the current branch, it should return Ok(None)
    assert_eq!(
        check_branch(&get_current_branch().await.unwrap_or("main".to_string()))
            .await
            .unwrap(),
        None
    );
    // For other existing branches, it should return Ok(Some(false))
    assert_eq!(check_branch("new_branch_01").await.unwrap(), Some(false));
}

async fn test_switch_branch() {
    println!("\n\x1b[1mTest switch_branch function.\x1b[0m");

    let show_all_branches = async || {
        // Use the list_branches function of the branch module to list all current local branches
        let _ = branch::list_branches(branch::BranchListMode::Local, &[], &[]).await;
        println!(
            "Current branch is '{}'.",
            get_current_branch()
                .await
                .unwrap_or("Get_current_branch_failed".to_string())
                .green()
        );
    };

    // Switch to the new branch and back
    show_all_branches().await;
    switch_branch("new_branch_01").await.unwrap();
    show_all_branches().await;
    switch_branch("new_branch_02").await.unwrap();
    show_all_branches().await;
    switch_branch("main").await.unwrap();
    show_all_branches().await;
}

#[tokio::test]
#[serial]
/// Tests branch creation, switching and validation functionality in the checkout module.
/// Verifies proper branch management and HEAD reference updates when switching between branches.
async fn test_checkout_module_functions() {
    println!("\n\x1b[1mTest checkout module functions.\x1b[0m");

    let temp_path = tempdir().unwrap();
    test::setup_clean_testing_env_in(temp_path.path());
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let init_args = init::InitArgs {
        bare: false,
        initial_branch: Some("main".to_string()),
        template: None,
        repo_directory: temp_path.path().to_str().unwrap().to_string(),
        quiet: false,
        shared: None,
        object_format: None,
        ref_format: None,
        from_git_repository: None,
        vault: false,
    };

    init::init(init_args)
        .await
        .expect("Error initializing repository");
    configure_identity_for_test().await;

    // Initialize the main branch by creating an empty commit
    let commit_args = commit::CommitArgs {
        message: Some("An empty initial commit".to_string()),
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
    commit::execute_safe(commit_args, &OutputConfig::default())
        .await
        .expect("initial commit should succeed");

    // Create tow new branch
    branch::create_branch(String::from("new_branch_01"), get_current_branch().await).await;
    branch::create_branch(String::from("new_branch_02"), get_current_branch().await).await;

    // Test the checkout module funsctions
    test_check_branch().await;
    test_switch_branch().await;
}

#[tokio::test]
#[serial]
/// Same branch workflow but in a SHA-256 repository; verifies commit id length matches hash kind.
async fn test_checkout_module_functions_sha256() {
    let temp_path = tempdir().unwrap();
    test::setup_clean_testing_env_in(temp_path.path());
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let init_args = init::InitArgs {
        bare: false,
        initial_branch: Some("main".to_string()),
        template: None,
        repo_directory: temp_path.path().to_str().unwrap().to_string(),
        quiet: false,
        shared: None,
        object_format: Some("sha256".to_string()),
        ref_format: None,
        from_git_repository: None,
        vault: false,
    };

    init::init(init_args)
        .await
        .expect("Error initializing repository");
    configure_identity_for_test().await;

    let commit_args = commit::CommitArgs {
        message: Some("An empty initial commit".to_string()),
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
    commit::execute_safe(commit_args, &OutputConfig::default())
        .await
        .expect("initial commit should succeed");

    // Ensure HEAD commit uses SHA-256 (64 hex chars)
    let head_commit = Head::current_commit().await.expect("HEAD missing");
    assert_eq!(head_commit.to_string().len(), 64);

    branch::create_branch(String::from("new_branch_01"), get_current_branch().await).await;
    branch::create_branch(String::from("new_branch_02"), get_current_branch().await).await;

    // Reuse existing helpers; they should work under SHA-256 as well.
    test_check_branch().await;
    test_switch_branch().await;
}

#[tokio::test]
#[serial]
/// In a SHA-256 repo, attempting to restore with a SHA-1 length hash should not change the worktree.
async fn checkout_restore_rejects_sha1_hash_in_sha256_repo() {
    let temp_path = tempdir().unwrap();
    test::setup_clean_testing_env_in(temp_path.path());
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    init::init(init::InitArgs {
        bare: false,
        initial_branch: Some("main".to_string()),
        template: None,
        repo_directory: temp_path.path().to_str().unwrap().to_string(),
        quiet: true,
        shared: None,
        object_format: Some("sha256".to_string()),
        ref_format: None,
        from_git_repository: None,
        vault: false,
    })
    .await
    .unwrap();
    configure_identity_for_test().await;

    // create and commit a file
    test::ensure_file("foo.txt", Some("v1"));
    add::execute_safe(
        AddArgs {
            pathspec: vec!["foo.txt".into()],
            all: false,
            update: false,
            refresh: false,
            force: false,
            verbose: false,
            dry_run: false,
            ignore_errors: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
        },
        &OutputConfig::default(),
    )
    .await
    .expect("add should succeed");
    commit::execute_safe(
        commit::CommitArgs {
            message: Some("init".into()),
            file: None,
            allow_empty: false,
            conventional: false,
            no_edit: false,
            amend: false,
            signoff: false,
            disable_pre: true,
            all: true,
            no_verify: false,
            author: None,
            ..Default::default()
        },
        &OutputConfig::default(),
    )
    .await
    .expect("commit should succeed");

    // modify the file
    test::ensure_file("foo.txt", Some("v2"));

    // try to restore using a SHA-1 length hash in a SHA-256 repo; should no-op
    let _ = restore::execute_safe(
        RestoreArgs {
            overlay: false,
            no_overlay: false,
            ours: false,
            theirs: false,
            ignore_unmerged: false,
            merge: false,
            conflict: None,
            worktree: true,
            staged: true,
            source: Some("4b825dc642cb6eb9a060e54bf8d69288fbee4904".into()),
            pathspec: vec![util::working_dir_string()],
            pathspec_from_file: None,
            pathspec_file_nul: false,
            no_progress: false,
        },
        &OutputConfig::default(),
    )
    .await;

    let content = std::fs::read_to_string(util::working_dir().join("foo.txt")).unwrap();
    assert_eq!(content, "v2", "invalid hash should not restore file");
}

/// Verifies that `checkout -b` returns a [`CliError`] when the worktree has
/// uncommitted staged changes that would be overwritten.
#[tokio::test]
#[serial]
async fn test_checkout_new_branch_with_dirty_worktree_returns_error() {
    use clap::Parser;
    use libra::{
        command::{
            add::{self, AddArgs},
            checkout::{self, CheckoutArgs},
            commit,
        },
        internal::config::ConfigKv,
        utils::test::{self, ChangeDirGuard},
    };

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    ConfigKv::set("user.name", "Checkout Tester", false)
        .await
        .unwrap();
    ConfigKv::set("user.email", "checkout@test.com", false)
        .await
        .unwrap();

    // Create initial commit so HEAD exists
    test::ensure_file("base.txt", Some("base"));
    add::execute_safe(
        AddArgs {
            pathspec: vec!["base.txt".into()],
            all: false,
            update: false,
            refresh: false,
            verbose: false,
            force: false,
            dry_run: false,
            ignore_errors: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
        },
        &OutputConfig::default(),
    )
    .await
    .expect("add should succeed");
    commit::execute_safe(
        commit::CommitArgs {
            message: Some("initial".into()),
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
        },
        &OutputConfig::default(),
    )
    .await
    .expect("initial commit should succeed");

    // Stage a change without committing — working tree is "dirty"
    test::ensure_file("dirty.txt", Some("uncommitted"));
    add::execute_safe(
        AddArgs {
            pathspec: vec!["dirty.txt".into()],
            all: false,
            update: false,
            refresh: false,
            verbose: false,
            force: false,
            dry_run: false,
            ignore_errors: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
        },
        &OutputConfig::default(),
    )
    .await
    .expect("add dirty file should succeed");

    let result = checkout::execute_safe(
        CheckoutArgs::try_parse_from(["checkout", "-b", "new"]).unwrap(),
        &OutputConfig::default(),
    )
    .await;
    assert!(
        result.is_err(),
        "checkout should fail when worktree is dirty"
    );
    let err = result.unwrap_err();
    assert!(
        err.message().contains("local changes"),
        "error should mention local changes, got: {}",
        err.message()
    );
}

/// Checking out the current branch should be a no-op even when the worktree
/// is dirty (Git prints "Already on ...").
#[tokio::test]
#[serial]
async fn test_checkout_current_branch_with_dirty_worktree_succeeds() {
    use clap::Parser;
    use libra::{
        command::{
            add::{self, AddArgs},
            checkout::{self, CheckoutArgs},
            commit,
        },
        internal::config::ConfigKv,
        utils::test::{self, ChangeDirGuard},
    };

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    ConfigKv::set("user.name", "Checkout Tester", false)
        .await
        .unwrap();
    ConfigKv::set("user.email", "checkout@test.com", false)
        .await
        .unwrap();

    test::ensure_file("base.txt", Some("base"));
    add::execute_safe(
        AddArgs {
            pathspec: vec!["base.txt".into()],
            all: false,
            update: false,
            refresh: false,
            verbose: false,
            force: false,
            dry_run: false,
            ignore_errors: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
        },
        &OutputConfig::default(),
    )
    .await
    .expect("add should succeed");
    commit::execute_safe(
        commit::CommitArgs {
            message: Some("initial".into()),
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
        },
        &OutputConfig::default(),
    )
    .await
    .expect("initial commit should succeed");

    // Dirty the worktree with unstaged content.
    test::ensure_file("base.txt", Some("base\nlocal change"));

    let current = get_current_branch()
        .await
        .expect("current branch should be present");
    let args = CheckoutArgs::try_parse_from(["checkout", current.as_str()]).unwrap();
    let result = checkout::execute_safe(args, &OutputConfig::default()).await;
    assert!(
        result.is_ok(),
        "checkout current branch should not fail on dirty worktree"
    );

    let after = get_current_branch()
        .await
        .expect("current branch should still be present");
    assert_eq!(after, current, "branch should remain unchanged");
    let content = std::fs::read_to_string("base.txt").unwrap();
    assert_eq!(content, "base\nlocal change");
}

/// Switching to another branch should keep checkout-specific dirty-worktree
/// wording even when the worktree has only unstaged changes.
#[tokio::test]
#[serial]
async fn test_checkout_existing_branch_with_unstaged_dirty_worktree_returns_error() {
    use clap::Parser;
    use libra::{
        command::{
            add::{self, AddArgs},
            checkout::{self, CheckoutArgs},
            commit,
        },
        internal::config::ConfigKv,
        utils::test::{self, ChangeDirGuard},
    };

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    ConfigKv::set("user.name", "Checkout Tester", false)
        .await
        .unwrap();
    ConfigKv::set("user.email", "checkout@test.com", false)
        .await
        .unwrap();

    test::ensure_file("base.txt", Some("base"));
    add::execute_safe(
        AddArgs {
            pathspec: vec!["base.txt".into()],
            all: false,
            update: false,
            refresh: false,
            verbose: false,
            force: false,
            dry_run: false,
            ignore_errors: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
        },
        &OutputConfig::default(),
    )
    .await
    .expect("add should succeed");
    commit::execute_safe(
        commit::CommitArgs {
            message: Some("initial".into()),
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
        },
        &OutputConfig::default(),
    )
    .await
    .expect("initial commit should succeed");

    branch::create_branch(String::from("other"), get_current_branch().await).await;

    test::ensure_file("base.txt", Some("base\nlocal change"));

    let result = checkout::execute_safe(
        CheckoutArgs::try_parse_from(["checkout", "other"]).unwrap(),
        &OutputConfig::default(),
    )
    .await;
    if let Ok(()) = result {
        panic!("checkout should fail when unstaged changes would be overwritten");
    }
    let err = result.unwrap_err();
    assert!(
        err.message()
            .contains("local changes would be overwritten by checkout"),
        "error should preserve checkout dirty-worktree wording, got: {}",
        err.message()
    );
}

#[test]
fn test_checkout_existing_branch_with_conflicting_untracked_file_returns_error() {
    use super::{
        assert_cli_success, create_committed_repo_via_cli, parse_cli_error_stderr,
        run_libra_command,
    };

    let repo = create_committed_repo_via_cli();

    let create = run_libra_command(&["switch", "-c", "other"], repo.path());
    assert_cli_success(&create, "switch -c other");

    std::fs::write(repo.path().join("conflict.txt"), "tracked on other\n").unwrap();
    let add = run_libra_command(&["add", "conflict.txt"], repo.path());
    assert_cli_success(&add, "add conflict.txt on other");
    let commit = run_libra_command(
        &["commit", "-m", "other adds conflict", "--no-verify"],
        repo.path(),
    );
    assert_cli_success(&commit, "commit conflict.txt on other");

    let back = run_libra_command(&["switch", "main"], repo.path());
    assert_cli_success(&back, "switch main");

    std::fs::write(repo.path().join("conflict.txt"), "local untracked\n").unwrap();

    let output = run_libra_command(&["checkout", "other"], repo.path());
    assert_eq!(output.status.code(), Some(128));
    let (_human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        report
            .message
            .contains("local changes would be overwritten by checkout"),
        "error should preserve checkout wording, got: {}",
        report.message
    );

    let content = std::fs::read_to_string(repo.path().join("conflict.txt")).unwrap();
    assert_eq!(content, "local untracked\n");
}

#[test]
fn test_checkout_force_discards_dirty_tracked_changes() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();

    // Track f.txt on main.
    std::fs::write(repo.path().join("f.txt"), "orig\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "f.txt"], repo.path()),
        "add f.txt",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "add f", "--no-verify"], repo.path()),
        "commit f",
    );

    // On `other`, change f.txt to a different committed content.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "other"], repo.path()),
        "switch -c other",
    );
    std::fs::write(repo.path().join("f.txt"), "other-version\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "f.txt"], repo.path()),
        "add f.txt on other",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "change f", "--no-verify"], repo.path()),
        "commit f on other",
    );

    // Back on main, leave an uncommitted local modification to the tracked file.
    assert_cli_success(
        &run_libra_command(&["switch", "main"], repo.path()),
        "switch main",
    );
    std::fs::write(repo.path().join("f.txt"), "dirty local\n").unwrap();

    // Plain checkout refuses; -f discards the local change and switches.
    let blocked = run_libra_command(&["checkout", "other"], repo.path());
    assert_ne!(
        blocked.status.code(),
        Some(0),
        "plain checkout should refuse to overwrite dirty tracked file"
    );

    let forced = run_libra_command(&["checkout", "-f", "other"], repo.path());
    assert_cli_success(&forced, "checkout -f other");
    let content = std::fs::read_to_string(repo.path().join("f.txt")).unwrap();
    assert_eq!(
        content, "other-version\n",
        "-f should adopt the target branch's tracked content, discarding local edits"
    );
}

#[test]
fn test_checkout_force_still_refuses_untracked_overwrite() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();

    assert_cli_success(
        &run_libra_command(&["switch", "-c", "other"], repo.path()),
        "switch -c other",
    );
    std::fs::write(repo.path().join("conflict.txt"), "tracked on other\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "conflict.txt"], repo.path()),
        "add conflict.txt",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "other adds conflict", "--no-verify"],
            repo.path(),
        ),
        "commit conflict.txt",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "main"], repo.path()),
        "switch main",
    );

    // An untracked file on main that the target branch tracks.
    std::fs::write(repo.path().join("conflict.txt"), "local untracked\n").unwrap();

    // -f discards tracked-file changes but must NOT clobber untracked files.
    let output = run_libra_command(&["checkout", "-f", "other"], repo.path());
    assert_ne!(
        output.status.code(),
        Some(0),
        "checkout -f must still refuse to overwrite an untracked file"
    );
    let content = std::fs::read_to_string(repo.path().join("conflict.txt")).unwrap();
    assert_eq!(
        content, "local untracked\n",
        "untracked file must be preserved under -f (no silent data loss)"
    );
}

#[test]
fn test_checkout_json_show_current_branch() {
    use super::{
        assert_cli_success, create_committed_repo_via_cli, parse_json_stdout, run_libra_command,
    };

    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "checkout"], repo.path());
    assert_cli_success(&output, "json checkout show current");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "checkout");
    assert_eq!(json["data"]["action"], "show-current");
    assert_eq!(json["data"]["branch"], "main");
    assert_eq!(json["data"]["detached"], false);
    assert_eq!(json["data"]["switched"], false);
    assert!(json["data"]["commit"].as_str().unwrap_or_default().len() >= 7);
    assert!(output.stderr.is_empty());
}

#[test]
fn test_checkout_json_switch_existing_branch() {
    use super::{
        assert_cli_success, create_committed_repo_via_cli, parse_json_stdout, run_libra_command,
    };

    let repo = create_committed_repo_via_cli();
    let branch = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&branch, "branch feature");

    let output = run_libra_command(&["--json", "checkout", "feature"], repo.path());
    assert_cli_success(&output, "json checkout feature");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "checkout");
    assert_eq!(json["data"]["action"], "switch");
    assert_eq!(json["data"]["previous_branch"], "main");
    assert_eq!(json["data"]["branch"], "feature");
    assert_eq!(json["data"]["switched"], true);
    assert_eq!(json["data"]["created"], false);
    assert_eq!(json["data"]["pulled"], false);
    assert!(output.stderr.is_empty());
}

#[test]
fn test_checkout_json_current_branch_reports_already_on() {
    use super::{
        assert_cli_success, create_committed_repo_via_cli, parse_json_stdout, run_libra_command,
    };

    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "checkout", "main"], repo.path());
    assert_cli_success(&output, "json checkout main");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "checkout");
    assert_eq!(json["data"]["action"], "already-on");
    assert_eq!(json["data"]["branch"], "main");
    assert_eq!(json["data"]["already_on"], true);
    assert_eq!(json["data"]["switched"], false);
    assert!(output.stderr.is_empty());
}

#[test]
fn test_checkout_json_create_branch() {
    use super::{
        assert_cli_success, create_committed_repo_via_cli, parse_json_stdout, run_libra_command,
    };

    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "checkout", "-b", "feature"], repo.path());
    assert_cli_success(&output, "json checkout -b feature");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "checkout");
    assert_eq!(json["data"]["action"], "create");
    assert_eq!(json["data"]["previous_branch"], "main");
    assert_eq!(json["data"]["branch"], "feature");
    assert_eq!(json["data"]["switched"], true);
    assert_eq!(json["data"]["created"], true);
    assert!(output.stderr.is_empty());
}

#[test]
fn test_checkout_separator_path_restores_worktree_from_index() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let branch = run_libra_command(&["branch", "tracked.txt"], repo.path());
    assert_cli_success(&branch, "branch tracked.txt");

    std::fs::write(repo.path().join("tracked.txt"), "worktree edit\n").unwrap();

    let output = run_libra_command(&["checkout", "--", "tracked.txt"], repo.path());
    assert_cli_success(&output, "checkout -- tracked.txt");

    let content = std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap();
    assert_eq!(content, "tracked\n");

    let branch = run_libra_command(&["branch", "--show-current"], repo.path());
    assert_cli_success(&branch, "branch --show-current");
    assert_eq!(String::from_utf8_lossy(&branch.stdout).trim(), "main");
}

#[test]
fn test_checkout_plain_name_stays_branch_mode_when_file_matches_branch() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let branch = run_libra_command(&["branch", "tracked.txt"], repo.path());
    assert_cli_success(&branch, "branch tracked.txt");

    let output = run_libra_command(&["checkout", "tracked.txt"], repo.path());
    assert_cli_success(&output, "checkout tracked.txt");

    let branch = run_libra_command(&["branch", "--show-current"], repo.path());
    assert_cli_success(&branch, "branch --show-current");
    assert_eq!(
        String::from_utf8_lossy(&branch.stdout).trim(),
        "tracked.txt"
    );
}

#[test]
fn test_checkout_json_treeish_separator_path_restores_index_and_worktree() {
    use super::{
        assert_cli_success, create_committed_repo_via_cli, parse_json_stdout, run_libra_command,
    };

    let repo = create_committed_repo_via_cli();
    std::fs::write(repo.path().join("tracked.txt"), "staged edit\n").unwrap();
    let add = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert_cli_success(&add, "add staged edit");
    std::fs::write(repo.path().join("tracked.txt"), "worktree edit\n").unwrap();

    let output = run_libra_command(
        &["--json", "checkout", "HEAD", "--", "tracked.txt"],
        repo.path(),
    );
    assert_cli_success(&output, "json checkout HEAD -- tracked.txt");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "checkout");
    assert_eq!(json["data"]["action"], "restore-paths");
    assert_eq!(json["data"]["switched"], false);
    assert_eq!(json["data"]["restore"]["source"], "HEAD");
    assert_eq!(json["data"]["restore"]["worktree"], true);
    assert_eq!(json["data"]["restore"]["staged"], true);
    assert_eq!(json["data"]["restore"]["restored_files"][0], "tracked.txt");
    assert!(output.stderr.is_empty());

    let content = std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap();
    assert_eq!(content, "tracked\n");

    let status = run_libra_command(&["status", "--porcelain"], repo.path());
    assert_cli_success(&status, "status --porcelain");
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "checkout HEAD -- path should leave index and worktree clean, got: {}",
        String::from_utf8_lossy(&status.stdout)
    );
}

#[test]
fn test_checkout_machine_separator_path_outputs_single_json_line() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    std::fs::write(repo.path().join("tracked.txt"), "worktree edit\n").unwrap();

    let output = run_libra_command(&["--machine", "checkout", "--", "tracked.txt"], repo.path());
    assert_cli_success(&output, "machine checkout -- tracked.txt");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().count(),
        1,
        "expected one JSON line, got: {stdout}"
    );
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("expected JSON");
    assert_eq!(json["command"], "checkout");
    assert_eq!(json["data"]["action"], "restore-paths");
    assert_eq!(json["data"]["restore"]["source"], serde_json::Value::Null);
    assert_eq!(json["data"]["restore"]["worktree"], true);
    assert_eq!(json["data"]["restore"]["staged"], false);
    assert!(output.stderr.is_empty());
}

#[test]
fn test_checkout_machine_outputs_single_json_line() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let branch = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&branch, "branch feature");

    let output = run_libra_command(&["--machine", "checkout", "feature"], repo.path());
    assert_cli_success(&output, "machine checkout feature");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().count(),
        1,
        "expected one JSON line, got: {stdout}"
    );
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("expected JSON");
    assert_eq!(json["command"], "checkout");
    assert_eq!(json["data"]["action"], "switch");
    assert_eq!(json["data"]["branch"], "feature");
    assert!(output.stderr.is_empty());
}

#[test]
fn test_checkout_detached_head_by_commit() {
    let repo = create_committed_repo_via_cli();

    let head_output = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    assert_cli_success(&head_output, "rev-parse HEAD");
    let head_hash = String::from_utf8_lossy(&head_output.stdout)
        .trim()
        .to_string();

    let output = run_libra_command(&["--json", "checkout", &head_hash], repo.path());
    assert_cli_success(&output, "checkout commit should detach HEAD");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["action"], "detach");
    assert_eq!(json["data"]["detached"], true);

    let head = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    let current_head = String::from_utf8_lossy(&head.stdout).trim().to_string();
    assert_eq!(current_head, head_hash);
}

#[test]
fn test_checkout_force_new_branch_resets_existing() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["checkout", "-b", "feature"], repo.path());
    assert_cli_success(&output, "checkout -b feature should succeed");

    let output = run_libra_command(&["checkout", "main"], repo.path());
    assert_cli_success(&output, "checkout main should succeed");

    std::fs::write(repo.path().join("second.txt"), "second\n").unwrap();
    let output = run_libra_command(&["add", "second.txt"], repo.path());
    assert_cli_success(&output, "add second.txt");
    let output = run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path());
    assert_cli_success(&output, "commit second");

    let head_output = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    let head_hash = String::from_utf8_lossy(&head_output.stdout)
        .trim()
        .to_string();

    let output = run_libra_command(&["checkout", "-B", "feature"], repo.path());
    assert_cli_success(&output, "checkout -B feature should succeed");

    let feature_output = run_libra_command(&["rev-parse", "feature"], repo.path());
    let feature_hash = String::from_utf8_lossy(&feature_output.stdout)
        .trim()
        .to_string();
    assert_eq!(feature_hash, head_hash);
}

#[test]
fn test_checkout_json_missing_branch_reports_invalid_target() {
    use super::{create_committed_repo_via_cli, parse_cli_error_stderr, run_libra_command};

    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "checkout", "missing"], repo.path());

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let (_human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
}

#[test]
fn test_checkout_json_reserved_branch_reports_invalid_target() {
    use super::{create_committed_repo_via_cli, parse_cli_error_stderr, run_libra_command};

    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "checkout", "intent"], repo.path());

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let (_human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(report.message.contains("checking out 'intent' branch"));
}

/// opencode.md OC-Phase 3 acceptance criterion 5 requires that
/// `checkout` refuse to route user work onto `traces`, the same
/// way it already refuses `intent`. The branch is reserved for the
/// external-agent capture subsystem (CEX-EntireIO) and any user-driven
/// checkout that lands on it would let `restore` / `reset` rewind
/// working state to AI-managed commits.
#[test]
fn test_checkout_traces_branch_reports_invalid_target() {
    use super::{create_committed_repo_via_cli, parse_cli_error_stderr, run_libra_command};

    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "checkout", "traces"], repo.path());

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let (_human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        report.message.contains("'traces'"),
        "error message must name the traces branch verbatim, got: {}",
        report.message,
    );
}

/// Counterpart that exercises the create-new-branch path: `checkout -b
/// traces` must fail, otherwise a user (or stray AI agent) could
/// clobber the reserved capture ref by creating a same-named local
/// branch and pushing it.
#[test]
fn test_checkout_create_traces_branch_is_blocked() {
    use super::{create_committed_repo_via_cli, parse_cli_error_stderr, run_libra_command};

    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "checkout", "-b", "traces"], repo.path());

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let (_human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        report.message.contains("'traces'"),
        "error message must name the traces branch verbatim, got: {}",
        report.message,
    );
}

#[test]
fn test_checkout_detach_at_branch_commit() {
    use super::{create_committed_repo_via_cli, parse_json_stdout, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // `checkout --detach main` detaches HEAD at main's commit instead of
    // switching to the branch (HEAD is already on main here).
    let out = run_libra_command(&["--json", "checkout", "--detach", "main"], p);
    assert!(
        out.status.success(),
        "checkout --detach: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["action"], "detach", "action is detach: {json}");
    assert_eq!(json["data"]["detached"], true, "HEAD is detached: {json}");

    // HEAD is now detached, not on a branch.
    let cur = run_libra_command(&["branch", "--show-current"], p);
    let cur_out = String::from_utf8_lossy(&cur.stdout);
    assert!(
        cur_out.contains("detached"),
        "HEAD should be detached, got: {cur_out:?}"
    );
}

#[test]
fn checkout_track_flag_is_accepted() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "create feature",
    );

    // `-t`/`--track` is accepted; for a local branch it is a no-op (Libra always
    // configures tracking for remote-tracking checkouts via DWIM).
    let short = run_libra_command(&["checkout", "-t", "feature"], p);
    assert_cli_success(&short, "checkout -t feature");
    let cur = run_libra_command(&["branch", "--show-current"], p);
    assert!(
        String::from_utf8_lossy(&cur.stdout).contains("feature"),
        "checkout -t switched to feature"
    );

    let long = run_libra_command(&["checkout", "--track", "main"], p);
    assert_cli_success(&long, "checkout --track main");
    let cur = run_libra_command(&["branch", "--show-current"], p);
    assert!(
        String::from_utf8_lossy(&cur.stdout).contains("main"),
        "checkout --track switched to main"
    );
}

#[test]
fn test_checkout_ignore_other_worktrees_is_accepted_noop() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "create feature",
    );

    // `--ignore-other-worktrees` is accepted and, in a SINGLE-worktree repo, a
    // no-op: there is no other worktree that could have `feature` checked out,
    // so the same-branch guard never fires and the checkout proceeds. (Part C
    // W0: in a MULTI-worktree repo the flag does NOT bypass the guard — Libra
    // never allows the same branch checked out twice; see the isolation tests.)
    let output = run_libra_command(&["checkout", "--ignore-other-worktrees", "feature"], p);
    assert_cli_success(&output, "checkout --ignore-other-worktrees feature");
    let current = run_libra_command(&["branch", "--show-current"], p);
    assert!(
        String::from_utf8_lossy(&current.stdout).contains("feature"),
        "checkout switched to feature"
    );
}

#[test]
fn test_checkout_no_progress_is_accepted_noop() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "create feature",
    );

    // `--no-progress` is accepted and a no-op: Libra's checkout never renders a
    // progress meter, so the checkout proceeds normally.
    let output = run_libra_command(&["checkout", "--no-progress", "feature"], p);
    assert_cli_success(&output, "checkout --no-progress feature");
    let current = run_libra_command(&["branch", "--show-current"], p);
    assert!(
        String::from_utf8_lossy(&current.stdout).contains("feature"),
        "checkout switched to feature"
    );
}

#[test]
fn test_checkout_no_overlay_is_accepted_noop() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "create feature",
    );

    // `--no-overlay` is accepted and a no-op: Libra's checkout is never in
    // overlay mode (it already matches `--no-overlay`, the Git default), so the
    // checkout proceeds normally.
    let output = run_libra_command(&["checkout", "--no-overlay", "feature"], p);
    assert_cli_success(&output, "checkout --no-overlay feature");
    let current = run_libra_command(&["branch", "--show-current"], p);
    assert!(
        String::from_utf8_lossy(&current.stdout).contains("feature"),
        "checkout --no-overlay switched to feature"
    );
}
