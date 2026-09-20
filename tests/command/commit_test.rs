//! Integration tests for the commit command covering staged changes, message handling, and tree/hash updates.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use libra::{
    command::commit::CleanupMode,
    utils::{object_ext::TreeExt, output::OutputConfig},
};
use serial_test::serial;
use tempfile::tempdir;

use super::*;
mod env_restore_tests;

mod unmerged;

#[tokio::test]
#[serial(cwd)]
/// A commit with no file changes should fail if `allow_empty` is false.
/// This test verifies that the commit command rejects empty changesets
/// when not explicitly permitted.
async fn test_execute_commit_with_empty_index_fail() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    let args = CommitArgs {
        message: Some("init".to_string()),
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
    };
    let result = execute_safe(args, &OutputConfig::default()).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().render().contains("nothing to commit"));
}

#[tokio::test]
#[serial(env, cwd)]
async fn test_commit_requires_configured_identity_in_strict_mode() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Isolate global/system config so that the host machine's real
    // ~/.libra/config.db (which may contain user.name / user.email) does not
    // leak into the cascade lookup and make the test pass incorrectly.
    let fake_global = temp_path.path().join("fake_global.db");
    let fake_system = temp_path.path().join("fake_system.db");
    let _global_config = test::ScopedEnvVar::set("LIBRA_CONFIG_GLOBAL_DB", &fake_global);
    let _system_config = test::ScopedEnvVar::set("LIBRA_CONFIG_SYSTEM_DB", &fake_system);

    use libra::internal::config::ConfigKv;
    ConfigKv::unset_all("user.name").await.unwrap();
    ConfigKv::unset_all("user.email").await.unwrap();
    ConfigKv::set("user.useConfigOnly", "true", false)
        .await
        .unwrap();

    test::ensure_file("identity.txt", Some("identity"));
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["identity.txt".into()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    let result = execute_safe(
        CommitArgs {
            message: Some("should fail without identity".to_string()),
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
    .await;
    assert!(result.is_err());
    let rendered = result.unwrap_err().render();
    assert!(rendered.contains("fatal: author identity unknown"));
    assert!(rendered.contains("Hint:"));
}

#[test]
fn test_commit_cli_without_identity_returns_auth_exit_code() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    std::fs::write(repo.path().join("identity.txt"), "identity\n").unwrap();

    let output = run_libra_command(&["add", "identity.txt"], repo.path());
    assert_cli_success(&output, "failed to stage identity fixture");

    let output = run_libra_command(&["commit", "-m", "missing identity"], repo.path());
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert!(stderr.contains("fatal: author identity unknown"));
    assert!(stderr.contains("Error-Code: LBR-AUTH-001"));
    assert!(stderr.contains("Hint: run 'libra config --global user.name"));
}

#[test]
fn test_commit_cli_use_config_only_returns_auth_exit_code() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["config", "user.useConfigOnly", "true"], repo.path());
    assert_cli_success(&output, "failed to enable useConfigOnly");

    std::fs::write(repo.path().join("identity.txt"), "identity\n").unwrap();

    let output = run_libra_command(&["add", "identity.txt"], repo.path());
    assert_cli_success(&output, "failed to stage identity fixture");

    let output = run_libra_command(&["commit", "-m", "missing identity"], repo.path());
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert!(stderr.contains("fatal: author identity unknown"));
    assert!(stderr.contains("Error-Code: LBR-AUTH-001"));
    assert!(stderr.contains("Hint: run 'libra config --global user.name"));
}

#[tokio::test]
#[serial(cwd)]
/// Tests normal commit functionality with both `--amend` and `--allow_empty` flags.
/// Verifies that:
/// 1. Amending works correctly when allowed
/// 2. Empty commits are permitted when explicitly enabled
async fn test_execute_commit() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    // create first empty commit
    {
        let args = CommitArgs {
            message: Some("init".to_string()),
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

        // check head branch exists
        let head = Head::current().await;
        let branch_name = match head {
            Head::Branch(name) => name,
            _ => panic!("head not in branch"),
        };
        // Migrated from lossy `Branch::find_branch` per docs/development/commands/branch.md.
        let branch = Branch::find_branch_result(&branch_name, None)
            .await
            .expect("failed to query branch")
            .expect("branch should exist");
        let commit: Commit = load_object(&branch.commit).unwrap();

        assert_eq!(commit.message.trim(), "init");
        // Migrated from lossy `Branch::find_branch` per docs/development/commands/branch.md.
        let branch = Branch::find_branch_result(&branch_name, None)
            .await
            .expect("failed to query branch")
            .expect("branch should exist");
        assert_eq!(branch.commit, commit.id);
    }

    // modify first empty commit
    {
        let args = CommitArgs {
            message: Some("init commit".to_string()),
            file: None,
            allow_empty: true,
            conventional: false,
            no_edit: false,
            amend: true,
            signoff: false,
            disable_pre: true,
            all: false,
            no_verify: false,
            author: None,
            ..Default::default()
        };
        commit::execute(args).await;

        // check head branch exists
        let head = Head::current().await;
        let branch_name = match head {
            Head::Branch(name) => name,
            _ => panic!("head not in branch"),
        };
        // Migrated from lossy `Branch::find_branch` per docs/development/commands/branch.md.
        let branch = Branch::find_branch_result(&branch_name, None)
            .await
            .expect("failed to query branch")
            .expect("branch should exist");
        let commit: Commit = load_object(&branch.commit).unwrap();

        assert_eq!(commit.message.trim(), "init commit");
        // Migrated from lossy `Branch::find_branch` per docs/development/commands/branch.md.
        let branch = Branch::find_branch_result(&branch_name, None)
            .await
            .expect("failed to query branch")
            .expect("branch should exist");
        assert_eq!(branch.commit, commit.id);
    }

    // create a new commit
    {
        // create `a.txt` `bb/b.txt` `bb/c.txt`
        test::ensure_file("a.txt", Some("a"));
        test::ensure_file("bb/b.txt", Some("b"));
        test::ensure_file("bb/c.txt", Some("c"));
        let args = AddArgs {
            intent_to_add: false,
            sparse: false,
            all: true,
            update: false,
            verbose: false,
            pathspec: vec![],
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
        };
        add::execute(args).await;
    }

    {
        let args = CommitArgs {
            message: Some("add some files".to_string()),
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
        };
        commit::execute(args).await;

        let commit_id = Head::current_commit().await.unwrap();
        let commit: Commit = load_object(&commit_id).unwrap();
        assert_eq!(
            commit.message.trim(),
            "add some files",
            "{}",
            commit.message
        );

        let pre_commit_id = commit.parent_commit_ids[0];
        let pre_commit: Commit = load_object(&pre_commit_id).unwrap();
        assert_eq!(pre_commit.message.trim(), "init commit");

        let tree_id = commit.tree_id;
        let tree: Tree = load_object(&tree_id).unwrap();
        assert_eq!(tree.tree_items.len(), 3); // .libraignore, a.txt, and bb/
    }
    //modify new commit
    {
        let args = CommitArgs {
            message: Some("add some txt files".to_string()),
            file: None,
            allow_empty: true,
            conventional: false,
            no_edit: false,
            amend: true,
            signoff: false,
            disable_pre: true,
            all: false,
            no_verify: false,
            author: None,
            ..Default::default()
        };
        commit::execute(args).await;

        let commit_id = Head::current_commit().await.unwrap();
        let commit: Commit = load_object(&commit_id).unwrap();
        assert_eq!(
            commit.message.trim(),
            "add some txt files",
            "{}",
            commit.message
        );

        let pre_commit_id = commit.parent_commit_ids[0];
        let pre_commit: Commit = load_object(&pre_commit_id).unwrap();
        assert_eq!(pre_commit.message.trim(), "init commit");

        let tree_id = commit.tree_id;
        let tree: Tree = load_object(&tree_id).unwrap();
        assert_eq!(tree.tree_items.len(), 3); // .libraignore, a.txt, and bb/
    }
}

#[tokio::test]
#[serial(cwd)]
async fn test_commit_with_all_flag_stages_tracked_changes() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    test::ensure_file("tracked.txt", Some("v1"));
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["tracked.txt".into()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    commit::execute(CommitArgs {
        message: Some("initial".to_string()),
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

    test::ensure_file("tracked.txt", Some("updated"));
    test::ensure_file("new.txt", Some("untracked"));

    commit::execute(CommitArgs {
        message: Some("with -a".to_string()),
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
    })
    .await;

    let head_id = Head::current_commit().await.unwrap();
    let commit: Commit = load_object(&head_id).unwrap();
    assert_eq!(commit.message.trim(), "with -a");
    let tree: Tree = load_object(&commit.tree_id).unwrap();
    let entries = tree.get_plain_items();
    let tracked_blob_hash = calc_file_blob_hash("tracked.txt").unwrap();
    let tracked_entry = entries
        .iter()
        .find(|(path, _)| path == &std::path::PathBuf::from("tracked.txt"))
        .expect("tracked file stored in commit");
    assert_eq!(tracked_entry.1, tracked_blob_hash);
    assert!(
        entries
            .iter()
            .all(|(path, _)| path != &std::path::PathBuf::from("new.txt")),
        "untracked files should not be auto-staged by -a"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_commit_with_all_flag_records_deletions() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    test::ensure_file("keep.txt", Some("keep"));
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["keep.txt".into()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    commit::execute(CommitArgs {
        message: Some("baseline".to_string()),
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

    std::fs::remove_file("keep.txt").unwrap();
    test::ensure_file("new_untracked.txt", Some("left alone"));

    commit::execute(CommitArgs {
        message: Some("remove tracked".to_string()),
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
    })
    .await;

    let head_id = Head::current_commit().await.unwrap();
    let commit: Commit = load_object(&head_id).unwrap();
    assert_eq!(commit.message.trim(), "remove tracked");
    let tree: Tree = load_object(&commit.tree_id).unwrap();
    let entries = tree.get_plain_items();
    assert!(
        entries
            .iter()
            .all(|(path, _)| path != &std::path::PathBuf::from("keep.txt")),
        "deleted tracked files should be removed from commit"
    );
    assert!(
        entries
            .iter()
            .all(|(path, _)| path != &std::path::PathBuf::from("new_untracked.txt")),
        "new untracked files should still be absent"
    );
}

#[tokio::test]
#[serial(cwd)]
/// Verifies commit and amend operations in a SHA-256 repository.
async fn test_commit_sha256() {
    let temp_path = tempdir().unwrap();
    test::setup_clean_testing_env_in(temp_path.path());
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Initialize a repository with SHA-256 object format
    init(InitArgs {
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
    use libra::internal::config::ConfigKv;
    ConfigKv::set("user.name", "SHA256 User", false)
        .await
        .unwrap();
    ConfigKv::set("user.email", "sha256@example.com", false)
        .await
        .unwrap();

    // Create and add a file
    test::ensure_file("a.txt", Some("hello sha256"));
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["a.txt".to_string()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    // Create the first commit
    commit::execute(CommitArgs {
        message: Some("first sha256 commit".to_string()),
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

    // Verify the commit hash is SHA-256 (64 hex characters)
    let head_commit = Head::current_commit().await.expect("HEAD missing");
    assert_eq!(
        head_commit.to_string().len(),
        64,
        "Commit hash should be SHA-256"
    );

    // Amend the commit
    commit::execute(CommitArgs {
        message: Some("amended sha256 commit".to_string()),
        file: None,
        allow_empty: true, // allow_empty is needed for amend if no new changes are staged
        conventional: false,
        no_edit: false,
        amend: true,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    // Verify the amended commit hash is also SHA-256
    let amended_commit = Head::current_commit().await.expect("Amended HEAD missing");
    assert_eq!(
        amended_commit.to_string().len(),
        64,
        "Amended commit hash should be SHA-256"
    );
    assert_ne!(
        head_commit, amended_commit,
        "Amend should create a new commit"
    );
}

#[tokio::test]
#[serial(cwd)]
/// Tests that the --author parameter correctly overrides the commit author
async fn test_commit_with_custom_author() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Set default user config using libra's internal config
    use libra::internal::config::ConfigKv;
    ConfigKv::unset_all("user.name").await.unwrap();
    ConfigKv::unset_all("user.email").await.unwrap();
    ConfigKv::set("user.name", "Default User", false)
        .await
        .unwrap();
    ConfigKv::set("user.email", "default@example.com", false)
        .await
        .unwrap();

    // Create a file and add it
    test::ensure_file("test.txt", Some("test content"));
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["test.txt".into()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    // Create commit with custom author
    commit::execute(CommitArgs {
        message: Some("commit with custom author".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: Some("Custom Author <custom@example.com>".to_string()),
        ..Default::default()
    })
    .await;

    // Verify the commit was created with the custom author
    let head_commit_id = Head::current_commit().await.unwrap();
    let commit: Commit = load_object(&head_commit_id).unwrap();

    assert_eq!(commit.author.name, "Custom Author");
    assert_eq!(commit.author.email, "custom@example.com");

    // Committer should still use default user
    assert_eq!(commit.committer.name, "Default User");
    assert_eq!(commit.committer.email, "default@example.com");

    assert_eq!(commit.message.trim(), "commit with custom author");
}

#[tokio::test]
#[serial(cwd)]
/// Tests that the --author parameter works with --amend
async fn test_commit_amend_with_custom_author() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Set default user config
    use libra::internal::config::ConfigKv;
    ConfigKv::unset_all("user.name").await.unwrap();
    ConfigKv::unset_all("user.email").await.unwrap();
    ConfigKv::set("user.name", "Default User", false)
        .await
        .unwrap();
    ConfigKv::set("user.email", "default@example.com", false)
        .await
        .unwrap();

    // Create initial commit with default author
    commit::execute(CommitArgs {
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
    })
    .await;

    let initial_commit_id = Head::current_commit().await.unwrap();
    let initial_commit: Commit = load_object(&initial_commit_id).unwrap();
    assert_eq!(initial_commit.author.name, "Default User");
    assert_eq!(initial_commit.author.email, "default@example.com");

    // Amend with custom author
    commit::execute(CommitArgs {
        message: Some("amended with custom author".to_string()),
        file: None,
        allow_empty: true,
        conventional: false,
        no_edit: false,
        amend: true,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: Some("Amend Author <amend@example.com>".to_string()),
        ..Default::default()
    })
    .await;

    // Verify the amended commit has the new custom author
    let amended_commit_id = Head::current_commit().await.unwrap();
    let amended_commit: Commit = load_object(&amended_commit_id).unwrap();

    assert_eq!(amended_commit.author.name, "Amend Author");
    assert_eq!(amended_commit.author.email, "amend@example.com");
    assert_eq!(amended_commit.message.trim(), "amended with custom author");

    // Should be a different commit
    assert_ne!(initial_commit_id, amended_commit_id);
}

#[tokio::test]
#[serial(env, cwd)]
async fn test_commit_amend_preserves_author_unless_reset() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    use libra::internal::config::ConfigKv;

    // Clear identity env vars so the config-driven identity below is authoritative.
    let _identity_env = [
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "EMAIL",
        "LIBRA_COMMITTER_NAME",
        "LIBRA_COMMITTER_EMAIL",
    ]
    .map(test::ScopedEnvVar::unset);

    // Initial commit authored by the original identity.
    ConfigKv::unset_all("user.name").await.unwrap();
    ConfigKv::unset_all("user.email").await.unwrap();
    ConfigKv::set("user.name", "Original Author", false)
        .await
        .unwrap();
    ConfigKv::set("user.email", "original@example.com", false)
        .await
        .unwrap();

    commit::execute(CommitArgs {
        message: Some("initial commit".to_string()),
        allow_empty: true,
        disable_pre: true,
        ..Default::default()
    })
    .await;
    let initial_id = Head::current_commit().await.unwrap();
    let initial: Commit = load_object(&initial_id).unwrap();
    assert_eq!(initial.author.name, "Original Author");

    // Switch identity, then amend WITHOUT --reset-author: the author must be
    // preserved (Git behavior) while the committer reflects the new identity.
    ConfigKv::set("user.name", "New Committer", false)
        .await
        .unwrap();
    ConfigKv::set("user.email", "new@example.com", false)
        .await
        .unwrap();

    commit::execute(CommitArgs {
        message: Some("amended, author preserved".to_string()),
        allow_empty: true,
        amend: true,
        disable_pre: true,
        reset_author: false,
        ..Default::default()
    })
    .await;
    let preserved_id = Head::current_commit().await.unwrap();
    let preserved: Commit = load_object(&preserved_id).unwrap();
    assert_eq!(
        preserved.author.name, "Original Author",
        "amend should preserve the original author by default"
    );
    assert_eq!(preserved.author.email, "original@example.com");
    assert_eq!(
        preserved.committer.name, "New Committer",
        "committer should reflect the current identity"
    );

    // Amend again WITH --reset-author: the author now adopts the current identity.
    commit::execute(CommitArgs {
        message: Some("amended, author reset".to_string()),
        allow_empty: true,
        amend: true,
        disable_pre: true,
        reset_author: true,
        ..Default::default()
    })
    .await;
    let reset_id = Head::current_commit().await.unwrap();
    let reset: Commit = load_object(&reset_id).unwrap();
    assert_eq!(
        reset.author.name, "New Committer",
        "--reset-author should adopt the current identity"
    );
    assert_eq!(reset.author.email, "new@example.com");
}

#[tokio::test]
#[serial(cwd)]
async fn test_commit_empty_working_tree() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    let args = CommitArgs {
        message: Some("empty commit".to_string()),
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
    };

    let result = execute_safe(args, &OutputConfig::default()).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().render().contains("nothing to commit"));
}

#[tokio::test]
#[serial(cwd)]
async fn test_commit_with_actual_changes() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    let init_args = CommitArgs {
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
    commit::execute(init_args).await;

    let test_file = temp_path.path().join("test.txt");
    std::fs::write(&test_file, "test content").unwrap();

    let add_args = add::AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["test.txt".to_string()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    };
    add::execute(add_args).await;

    let args = CommitArgs {
        message: Some("add test file".to_string()),
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
    };

    commit::execute(args).await;
}

#[tokio::test]
#[serial(cwd)]
async fn test_commit_amend_without_changes() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Create an initial commit
    let init_args = CommitArgs {
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
    commit::execute(init_args).await;

    // Amend the commit without any staged changes (should work without allow_empty)
    let amend_args = CommitArgs {
        message: Some("amended commit message".to_string()),
        file: None,
        allow_empty: false, // Should not need allow_empty for amend
        conventional: false,
        no_edit: false,
        amend: true,
        signoff: false,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    };

    // This should succeed even without staged changes
    commit::execute(amend_args).await;
}

#[tokio::test]
#[serial(cwd)]
async fn test_commit_signoff_persists_trailer() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    test::ensure_file("signed.txt", Some("signed content"));
    add::execute(add::AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["signed.txt".into()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    commit::execute(CommitArgs {
        message: Some("signed commit".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: false,
        signoff: true,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    let head_id = Head::current_commit().await.unwrap();
    let commit: Commit = load_object(&head_id).unwrap();
    assert!(commit.message.trim_start().starts_with("signed commit"));
    assert!(
        commit
            .message
            .contains("Signed-off-by: Libra Test User <libra-test@example.com>"),
        "signoff trailer missing from commit message: {}",
        commit.message
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_commit_amend_signoff_persists_trailer() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    commit::execute(CommitArgs {
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
    })
    .await;

    commit::execute(CommitArgs {
        message: Some("amended signed commit".to_string()),
        file: None,
        allow_empty: false,
        conventional: false,
        no_edit: false,
        amend: true,
        signoff: true,
        disable_pre: true,
        all: false,
        no_verify: false,
        author: None,
        ..Default::default()
    })
    .await;

    let head_id = Head::current_commit().await.unwrap();
    let commit: Commit = load_object(&head_id).unwrap();
    assert!(
        commit
            .message
            .trim_start()
            .starts_with("amended signed commit")
    );
    assert!(
        commit
            .message
            .contains("Signed-off-by: Libra Test User <libra-test@example.com>"),
        "signoff trailer missing from amended commit message: {}",
        commit.message
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_commit_amend_without_existing_commit_returns_repo_state_error() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    let result = execute_safe(
        CommitArgs {
            message: Some("amend without head".to_string()),
            file: None,
            allow_empty: false,
            conventional: false,
            no_edit: false,
            amend: true,
            signoff: false,
            disable_pre: true,
            all: false,
            no_verify: false,
            author: None,
            ..Default::default()
        },
        &OutputConfig::default(),
    )
    .await;

    let err = result.expect_err("amending without an existing commit should fail");
    assert_eq!(err.stable_code().as_str(), "LBR-REPO-003");
    assert!(
        err.message().contains("there is no commit to amend"),
        "unexpected error message: {}",
        err.message()
    );
}

/// Without explicit identity (config/env), commit should fail with the same
/// "author identity unknown" style error as Git.
#[tokio::test]
#[serial(env, cwd)]
async fn test_commit_without_identity_fails_by_default() {
    use libra::internal::config::ConfigKv;

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    // Isolate from host config so no user.name/email leaks in
    let fake_global = temp_path.path().join("fake_global.db");
    let fake_system = temp_path.path().join("fake_system.db");
    let _global_config = test::ScopedEnvVar::set("LIBRA_CONFIG_GLOBAL_DB", &fake_global);
    let _system_config = test::ScopedEnvVar::set("LIBRA_CONFIG_SYSTEM_DB", &fake_system);
    let _identity_env = [
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "EMAIL",
        "LIBRA_COMMITTER_NAME",
        "LIBRA_COMMITTER_EMAIL",
    ]
    .map(test::ScopedEnvVar::unset);

    // Ensure useConfigOnly is NOT set (default)
    ConfigKv::unset_all("user.name").await.unwrap();
    ConfigKv::unset_all("user.email").await.unwrap();

    test::ensure_file("autodetect.txt", Some("content"));
    add::execute(add::AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["autodetect.txt".into()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    let result = execute_safe(
        CommitArgs {
            message: Some("auto-detect identity test".to_string()),
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
    .await;

    assert!(result.is_err(), "commit should fail without identity");
    let rendered = result.unwrap_err().render();
    assert!(rendered.contains("fatal: author identity unknown"));
    assert!(rendered.contains("Hint:"));
}

/// `libra commit --help` surfaces the EXAMPLES section so users see the
/// nine canonical invocations (`-m`, `-a -m`, `--amend`, `--amend
/// --no-edit`, `-F file`, `-s -m`, `--allow-empty`, `--conventional`,
/// `--json -m`) without having to read the design doc. Companion to the
/// global `--help` EXAMPLES rollout tracked in
/// `docs/development/commands/_general.md` (cross-cutting item B).
#[test]
fn test_commit_help_lists_examples_banner() {
    let repo = tempdir().expect("tempdir for commit --help");
    let output = run_libra_command(&["commit", "--help"], repo.path());
    assert!(
        output.status.success(),
        "commit --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "commit --help should include EXAMPLES banner, stdout: {stdout}"
    );
    for invocation in [
        "libra commit -m",
        "libra commit --amend",
        "libra commit -a -m",
        "libra commit -F message.txt",
        "libra commit -s -m",
        "libra commit --allow-empty",
        "libra commit --json -m",
    ] {
        assert!(
            stdout.contains(invocation),
            "commit --help should include `{invocation}`, stdout: {stdout}"
        );
    }
}

#[tokio::test]
#[serial(cwd)]
async fn test_commit_cleanup_strips_comments() {
    let temp_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_dir.path());

    test::ensure_file("a.txt", Some("a\n"));
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["a.txt".into()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    commit::execute(CommitArgs {
        message: Some("subject\n\n# this is a comment\nbody".into()),
        cleanup: Some(CleanupMode::Strip),
        no_verify: true,
        ..Default::default()
    })
    .await;

    let commit = Head::current_commit().await.unwrap();
    let commit_obj = load_object::<Commit>(&commit).unwrap();
    assert!(
        !commit_obj.message.contains("# this is a comment"),
        "cleanup=strip should remove comment lines: {}",
        commit_obj.message
    );
    assert!(commit_obj.message.contains("body"));
}

#[test]
fn test_commit_honors_cleanup_and_verbose_config() {
    // `commit.cleanup` supplies the default cleanup mode when `--cleanup` is unset,
    // and `commit.verbose` makes `-v` the default — both matching Git.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // commit.cleanup=verbatim keeps a `#` comment line a default Strip would remove.
    assert_cli_success(
        &run_libra_command(&["config", "commit.cleanup", "verbatim"], p),
        "set commit.cleanup",
    );
    std::fs::write(p.join("c.txt"), "one\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "c.txt"], p), "add c");
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "subject\n\n# keep me\nbody", "--no-verify"],
            p,
        ),
        "commit with verbatim cleanup",
    );
    let msg = String::from_utf8_lossy(&run_libra_command(&["cat-file", "-p", "HEAD"], p).stdout)
        .into_owned();
    assert!(
        msg.contains("# keep me"),
        "commit.cleanup=verbatim keeps the comment line:\n{msg}"
    );

    // commit.verbose=true prints the staged diff to stderr on a plain `-m` commit.
    assert_cli_success(
        &run_libra_command(&["config", "commit.verbose", "true"], p),
        "set commit.verbose",
    );
    std::fs::write(p.join("c.txt"), "one\ntwo\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "c.txt"], p), "add c2");
    let out = run_libra_command(&["commit", "-m", "verbose default", "--no-verify"], p);
    assert_cli_success(&out, "commit with verbose config");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("@@") || stderr.contains("+two"),
        "commit.verbose=true prints the staged diff to stderr:\n{stderr}"
    );
}

#[test]
fn test_commit_cleanup_default_on_non_editor_keeps_comments() {
    // Git's `default` cleanup means strip when the message is edited, otherwise
    // whitespace. On the non-editor `-m` path it must behave like whitespace, so a
    // `#` comment line is kept (it is NOT treated as strip).
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("d.txt"), "one\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "d.txt"], p), "add d");
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--cleanup=default",
                "-m",
                "subject\n\n# keep on default\nbody",
                "--no-verify",
            ],
            p,
        ),
        "commit --cleanup=default",
    );
    let msg = String::from_utf8_lossy(&run_libra_command(&["cat-file", "-p", "HEAD"], p).stdout)
        .into_owned();
    assert!(
        msg.contains("# keep on default"),
        "--cleanup=default on a non-editor -m commit keeps comment lines (whitespace semantics):\n{msg}"
    );

    // `--cleanup=scissors` likewise behaves like whitespace on the non-editor path:
    // it does NOT truncate at the scissors marker (Git only truncates when edited).
    std::fs::write(p.join("d.txt"), "two\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "d.txt"], p), "add d2");
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--cleanup=scissors",
                "-m",
                "subj2\n# ------------------------ >8 ------------------------\nbelow scissors",
                "--no-verify",
            ],
            p,
        ),
        "commit --cleanup=scissors",
    );
    let scissors_msg =
        String::from_utf8_lossy(&run_libra_command(&["cat-file", "-p", "HEAD"], p).stdout)
            .into_owned();
    assert!(
        scissors_msg.contains("below scissors"),
        "--cleanup=scissors on a non-editor -m commit does not truncate at the marker:\n{scissors_msg}"
    );
}

#[test]
fn test_commit_config_invalid_is_fatal_but_flag_overrides() {
    // An invalid `commit.cleanup` is fatal when no `--cleanup` is given, but an
    // explicit `--cleanup` flag wins and overrides the bad config; `commit.verbose`
    // accepts an integer (Git's bool-or-int).
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["config", "commit.cleanup", "bogus"], p),
        "set invalid commit.cleanup",
    );
    std::fs::write(p.join("e.txt"), "1\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "e.txt"], p), "add e");

    // No flag: the invalid configured mode is fatal.
    let bad = run_libra_command(&["commit", "-m", "x", "--no-verify"], p);
    assert_ne!(
        bad.status.code(),
        Some(0),
        "invalid commit.cleanup should be fatal: {}",
        String::from_utf8_lossy(&bad.stderr)
    );

    // Explicit `--cleanup` overrides the bad config and succeeds.
    assert_cli_success(
        &run_libra_command(&["commit", "--cleanup=strip", "-m", "x", "--no-verify"], p),
        "explicit --cleanup overrides invalid commit.cleanup",
    );

    // `commit.verbose=2` (integer) is accepted and enables the verbose diff.
    assert_cli_success(
        &run_libra_command(&["config", "--unset", "commit.cleanup"], p),
        "unset commit.cleanup",
    );
    assert_cli_success(
        &run_libra_command(&["config", "commit.verbose", "2"], p),
        "set commit.verbose=2",
    );
    std::fs::write(p.join("e.txt"), "1\n2\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "e.txt"], p), "add e2");
    let v = run_libra_command(&["commit", "-m", "v", "--no-verify"], p);
    assert_cli_success(&v, "commit.verbose=2 accepted");
    let stderr = String::from_utf8_lossy(&v.stderr);
    assert!(
        stderr.contains("@@") || stderr.contains("+2"),
        "commit.verbose=2 enables the staged-diff output:\n{stderr}"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_commit_trailer_appended() {
    let temp_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_dir.path());

    test::ensure_file("a.txt", Some("a\n"));
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["a.txt".into()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    commit::execute(CommitArgs {
        message: Some("subject".into()),
        trailers: vec!["Reviewed-by: Jane".to_string()],
        no_verify: true,
        ..Default::default()
    })
    .await;

    let commit = Head::current_commit().await.unwrap();
    let commit_obj = load_object::<Commit>(&commit).unwrap();
    assert!(commit_obj.message.contains("Reviewed-by: Jane"));
}

#[tokio::test]
#[serial(cwd)]
async fn test_commit_dry_run_does_not_create_commit() {
    let temp_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_dir.path());

    test::ensure_file("a.txt", Some("a\n"));
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["a.txt".into()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;

    let before = Head::current_commit().await;

    commit::execute(CommitArgs {
        message: Some("dry run subject".into()),
        dry_run: true,
        no_verify: true,
        ..Default::default()
    })
    .await;

    let after = Head::current_commit().await;
    assert_eq!(before, after, "--dry-run should not advance HEAD");
}

#[tokio::test]
#[serial(cwd)]
async fn test_commit_reuse_message() {
    let temp_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_dir.path());

    test::ensure_file("a.txt", Some("a\n"));
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["a.txt".into()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("original subject".into()),
        no_verify: true,
        ..Default::default()
    })
    .await;

    let first = Head::current_commit().await.unwrap();

    test::ensure_file("b.txt", Some("b\n"));
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["b.txt".into()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;
    commit::execute(CommitArgs {
        reuse_message: Some("HEAD".to_string()),
        no_verify: true,
        ..Default::default()
    })
    .await;

    let second = Head::current_commit().await.unwrap();
    let second_obj = load_object::<Commit>(&second).unwrap();
    assert_eq!(
        second_obj.message.trim_start_matches('\n'),
        "original subject"
    );
    assert_ne!(first, second);
}

#[tokio::test]
#[serial(cwd)]
async fn test_commit_fixup_sets_subject() {
    let temp_dir = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_dir.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_dir.path());

    test::ensure_file("a.txt", Some("a\n"));
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["a.txt".into()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("original subject".into()),
        no_verify: true,
        ..Default::default()
    })
    .await;

    test::ensure_file("b.txt", Some("b\n"));
    add::execute(AddArgs {
        intent_to_add: false,
        sparse: false,
        pathspec: vec!["b.txt".into()],
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
        resolved: false,
        patch: false,
        auto_advance: false,
        no_auto_advance: false,
    })
    .await;
    commit::execute(CommitArgs {
        fixup: Some("HEAD".to_string()),
        no_verify: true,
        ..Default::default()
    })
    .await;

    let commit = Head::current_commit().await.unwrap();
    let commit_obj = load_object::<Commit>(&commit).unwrap();
    assert_eq!(
        commit_obj.message.trim_start_matches('\n'),
        "fixup! original subject"
    );
}

#[cfg(unix)]
#[test]
fn test_commit_porcelain_non_utf8_dir_marker_is_not_corrupted() {
    use std::os::unix::ffi::OsStrExt;

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // A collapsed untracked directory with a non-UTF-8 name must keep its
    // bytes through the legacy collapse helper — escaped with the default
    // quotePath, never a U+FFFD replacement character.
    let dir = p.join(std::ffi::OsStr::from_bytes(b"dir-\xff"));
    if let Err(error) = std::fs::create_dir(&dir) {
        // APFS (macOS) refuses non-UTF-8 names with EILSEQ; the byte-level
        // fixture cannot exist there — same skip convention as the wave-0
        // non-UTF-8 tests.
        eprintln!("skipped (filesystem refuses non-UTF-8 file names: {error})");
        return;
    }
    std::fs::write(dir.join("file.txt"), "payload\n").unwrap();
    std::fs::write(p.join("staged.txt"), "s\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "staged.txt"], p),
        "add staged.txt",
    );

    let output = run_libra_command(&["commit", "--dry-run", "--porcelain", "-m", "preview"], p);
    assert_cli_success(&output, "commit --dry-run --porcelain");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("dir-\\377/"),
        "the marker keeps its real bytes (octal-escaped): {stdout}"
    );
    assert!(
        !stdout.contains('\u{fffd}'),
        "and never a replacement character: {stdout}"
    );
}

#[test]
fn test_commit_porcelain_outputs_status_format() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    std::fs::write(p.join("staged.txt"), "s\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "staged.txt"], p),
        "add staged.txt",
    );
    std::fs::write(p.join("untracked.txt"), "u\n").unwrap();

    let before = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();

    // --dry-run --porcelain: machine status (porcelain v1), no commit.
    let output = run_libra_command(&["commit", "--dry-run", "--porcelain", "-m", "preview"], p);
    assert_cli_success(&output, "commit --dry-run --porcelain");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("A  staged.txt"),
        "staged file should appear in porcelain output: {stdout:?}"
    );
    assert!(
        stdout.contains("?? untracked.txt"),
        "untracked file should appear in porcelain output: {stdout:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout).trim(),
        before,
        "--dry-run must not move HEAD"
    );

    // `--porcelain` implies `--dry-run` (Git semantics): it prints the porcelain
    // preview and does NOT create a commit, even without an explicit --dry-run.
    let output = run_libra_command(&["commit", "--porcelain", "-m", "add staged"], p);
    assert_cli_success(&output, "commit --porcelain");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("A  staged.txt"),
        "porcelain output expected: {stdout:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout).trim(),
        before,
        "commit --porcelain implies dry-run and must NOT create a commit"
    );
}

#[test]
fn test_commit_all_porcelain_shows_autostaged_as_staged() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // A tracked, committed file, then modified in the worktree (unstaged).
    std::fs::write(p.join("tracked.txt"), "v1\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], p),
        "add tracked.txt",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "add tracked", "--no-verify"], p),
        "commit tracked",
    );
    std::fs::write(p.join("tracked.txt"), "v2\n").unwrap();

    // `commit -a --porcelain`: `-a` auto-stages the modification for the
    // preview, so the porcelain must show it as STAGED ("M  tracked.txt"), not
    // unstaged (" M tracked.txt") — the isolated preview is read after auto-staging.
    let output = run_libra_command(&["commit", "-a", "--porcelain", "-m", "x"], p);
    assert_cli_success(&output, "commit -a --porcelain");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("M  tracked.txt"),
        "auto-staged (-a) modification must appear as staged in porcelain: {stdout:?}"
    );
    assert!(
        !stdout.contains(" M tracked.txt"),
        "the -a modification must not appear as unstaged: {stdout:?}"
    );

    // The preview must NOT mutate the index: the modification is still unstaged
    // afterwards (the dry-run `-a` auto-stage used an isolated index).
    let status = run_libra_command(&["status", "--porcelain"], p);
    assert_cli_success(&status, "status --porcelain after preview");
    let status_out = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_out.contains(" M tracked.txt"),
        "commit -a --porcelain (dry-run) must not persist the auto-stage: {status_out:?}"
    );
    assert!(
        !status_out.contains("M  tracked.txt"),
        "the modification must remain unstaged after the preview: {status_out:?}"
    );
}

#[test]
fn commit_no_status_flag_is_accepted_noop() {
    let repo = create_committed_repo_via_cli();
    std::fs::write(repo.path().join("ns.txt"), "x\n").unwrap();
    assert!(
        run_libra_command(&["add", "ns.txt"], repo.path())
            .status
            .success()
    );
    // `--no-status` is accepted and a no-op: Libra's commit editor template
    // never includes a status section, so the commit proceeds normally.
    let output = run_libra_command(
        &[
            "commit",
            "--no-status",
            "-m",
            "with no-status",
            "--no-verify",
        ],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "commit --no-status succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn commit_no_gpg_sign_produces_unsigned_commit() {
    let repo = create_committed_repo_via_cli();
    // `--no-gpg-sign` skips Libra's vault signing path (`gpg_sig = None`) even
    // with `vault.signing=true`, so the resulting commit carries no `gpgsig`
    // header. (A full signed-vs-unsigned differential would require a vault
    // unseal key, which `libra init` does not load in the test harness — a basic
    // repo's commits are unsigned regardless; this asserts the flag's contract:
    // the commit is created and is not signed.)
    assert!(
        run_libra_command(&["config", "vault.signing", "true"], repo.path())
            .status
            .success()
    );
    std::fs::write(repo.path().join("gs.txt"), "x\n").unwrap();
    assert!(
        run_libra_command(&["add", "gs.txt"], repo.path())
            .status
            .success()
    );
    let output = run_libra_command(
        &[
            "commit",
            "--no-gpg-sign",
            "-m",
            "with no-gpg-sign",
            "--no-verify",
        ],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "commit --no-gpg-sign succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let show = run_libra_command(&["cat-file", "-p", "HEAD"], repo.path());
    assert!(
        !String::from_utf8_lossy(&show.stdout).contains("gpgsig"),
        "commit --no-gpg-sign produces an unsigned commit (no gpgsig header)"
    );
}

/// Regression: `--amend --no-edit` on a *signed* parent must reuse the parent's
/// real log message, NOT leak the parent's embedded `gpgsig` signature block.
///
/// Libra stores the PGP signature *inside* the commit object's message field
/// (see `format_commit_msg`), so the amend path has to strip it with
/// `parse_commit_msg` before reusing the parent's message. Before the fix, the
/// amended commit's subject/message became "gpgsig -----BEGIN PGP SIGNATURE-----…"
/// instead of the parent's real subject.
#[test]
fn test_commit_amend_no_edit_signed_parent_does_not_leak_gpgsig() {
    let repo = tempdir().unwrap();
    let p = repo.path();

    // `init --vault false` leaves no signing key; `generate-gpg-key` then both
    // creates the key and turns on `vault.signing`, so the next commit is
    // GPG-signed and its stored body embeds a `gpgsig` block.
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], p),
        "init --vault false",
    );
    configure_identity_via_cli(p);
    assert_cli_success(
        &run_libra_command(&["config", "generate-gpg-key"], p),
        "generate-gpg-key",
    );

    // Signed base commit whose real subject is "original-subject".
    std::fs::write(p.join("f.txt"), "a\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add base file");
    let base = run_libra_command(
        &["--json", "commit", "-m", "original-subject", "--no-verify"],
        p,
    );
    assert_cli_success(&base, "create signed base commit");
    assert_eq!(
        parse_json_stdout(&base)["data"]["signed"].as_bool(),
        Some(true),
        "base commit must be signed for this regression to be meaningful"
    );

    // Amend with --no-edit: the amended commit must carry the parent's real
    // message, not its signature header.
    std::fs::write(p.join("f.txt"), "ab\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add amended file");
    assert_cli_success(
        &run_libra_command(&["commit", "--amend", "--no-edit", "--no-verify"], p),
        "amend --no-edit",
    );

    // The amended subject must equal the parent's real subject — not a leaked
    // "gpgsig -----BEGIN PGP SIGNATURE-----" header line.
    let subject_out = run_libra_command(&["log", "-1", "--format=%s"], p);
    assert_cli_success(&subject_out, "log amended subject");
    let subject = String::from_utf8_lossy(&subject_out.stdout)
        .trim()
        .to_string();
    assert_eq!(
        subject, "original-subject",
        "amended subject must be the parent's real message, not a leaked gpgsig header"
    );

    // Belt-and-suspenders: Libra's `log` strips a *legitimate* `gpgsig` header
    // from the displayed message, so any signature-block text appearing in the
    // amended commit's log means the parent's signature leaked into the body.
    let log_out = run_libra_command(&["log", "-1"], p);
    assert_cli_success(&log_out, "log amended commit");
    let log = String::from_utf8_lossy(&log_out.stdout);
    assert!(
        !log.contains("gpgsig"),
        "amended message leaked a gpgsig header:\n{log}"
    );
    assert!(
        !log.contains("BEGIN PGP SIGNATURE"),
        "amended message leaked a PGP signature block:\n{log}"
    );
}

#[test]
fn commit_with_terminal_index_failure_is_repaired_without_recommitting() {
    let repo = tempdir().expect("create temp repo");
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    std::fs::write(p.join("indexed.txt"), "content\n").expect("write fixture");
    assert_cli_success(
        &run_libra_command(&["add", "indexed.txt"], p),
        "stage fixture",
    );

    let committed = run_libra_command_with_stdin_and_env(
        &["commit", "-m", "durable index repair", "--no-verify"],
        p,
        "",
        &[("LIBRA_TEST_OBJECT_INDEX_UPDATE_FAIL", "1")],
    );
    assert_cli_success(
        &committed,
        "the local commit should complete while cloud-index repair is durable",
    );
    assert!(
        String::from_utf8_lossy(&committed.stderr).contains("durable repair queue"),
        "terminal index failure should be visible: {}",
        String::from_utf8_lossy(&committed.stderr)
    );
    assert!(
        String::from_utf8_lossy(&committed.stdout).contains("durable index repair"),
        "the ref-advancing commit should have completed"
    );

    let marker_dir = p.join(".libra/object-index-repair");
    let pending = std::fs::read_dir(&marker_dir)
        .expect("repair marker directory should exist")
        .count();
    assert!(
        pending >= 2,
        "commit and tree updates should remain repairable"
    );

    let status = run_libra_command(&["status", "--short"], p);
    assert_cli_success(&status, "the next command should repair pending index rows");
    assert_eq!(
        std::fs::read_dir(&marker_dir)
            .expect("repair marker directory should remain readable")
            .count(),
        0,
        "repair must not require creating a second commit"
    );
}

fn merge_conflict_repo_for_commit() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("shared.txt"), "top\nl1\nORIG\nl3\nbottom\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base shared", "--no-verify"], p),
        "commit base",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "co feature",
    );
    std::fs::write(p.join("shared.txt"), "top\nl1\nFEATURE\nl3\nbottom\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add feature");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "feature edit", "--no-verify"], p),
        "commit feature",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "co main");
    std::fs::write(p.join("shared.txt"), "top\nl1\nMAIN\nl3\nbottom\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "shared.txt"], p), "add main");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main edit", "--no-verify"], p),
        "commit main",
    );
    repo
}

fn resolve_merge_conflict(p: &std::path::Path) {
    std::fs::write(p.join("shared.txt"), "top\nl1\nRESOLVED\nl3\nbottom\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "shared.txt"], p),
        "stage resolution",
    );
}

fn head_parent_count(p: &std::path::Path) -> usize {
    let out = run_libra_command(&["cat-file", "-p", "HEAD"], p);
    assert_cli_success(&out, "cat-file HEAD");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| line.starts_with("parent "))
        .count()
}

fn head_message_body(p: &std::path::Path) -> String {
    let out = run_libra_command(&["cat-file", "-p", "HEAD"], p);
    assert_cli_success(&out, "cat-file HEAD message");
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_once("\n\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default()
}

/// M-MCOMMIT MC1–MC9, MC11 (#477 HF-27): commit finishes an in-progress merge.
#[test]
fn test_commit_concludes_in_progress_merge_matrix() {
    // MC5: unresolved conflicts still refuse.
    let repo = merge_conflict_repo_for_commit();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["merge", "feature"], p).status.code(),
        Some(128)
    );
    let refused = run_libra_command(&["commit", "-m", "nope", "--no-verify"], p);
    assert_eq!(refused.status.code(), Some(128), "MC5");
    assert!(
        p.join(".libra/merge-state.json").exists(),
        "MC5 keeps state"
    );

    // MC6: --amend during merge refuses and does not move HEAD.
    let before = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    let amend = run_libra_command(&["commit", "--amend", "--no-edit", "--no-verify"], p);
    assert_eq!(amend.status.code(), Some(128), "MC6");
    assert!(
        String::from_utf8_lossy(&amend.stderr).contains("cannot amend"),
        "MC6: {}",
        String::from_utf8_lossy(&amend.stderr)
    );
    let after = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    assert_eq!(before, after, "MC6 HEAD unchanged");
    assert!(
        p.join(".libra/merge-state.json").exists(),
        "MC6 keeps state"
    );

    // MC9: --dry-run writes nothing and does not print a fake hash.
    resolve_merge_conflict(p);
    let dry = run_libra_command(&["commit", "--dry-run", "-m", "preview", "--no-verify"], p);
    assert_cli_success(&dry, "MC9 dry-run");
    let dry_out = format!(
        "{}{}",
        String::from_utf8_lossy(&dry.stdout),
        String::from_utf8_lossy(&dry.stderr)
    );
    assert!(
        dry_out.contains("Would finish the in-progress merge"),
        "MC9 preview: {dry_out}"
    );
    assert!(
        !dry_out.contains("[main "),
        "MC9 must not print a fake commit: {dry_out}"
    );
    assert!(
        p.join(".libra/merge-state.json").exists(),
        "MC9 keeps state"
    );

    // MC7: partial commit stays a 129 usage error.
    let partial = run_libra_command(&["commit", "shared.txt", "--no-verify"], p);
    assert_eq!(partial.status.code(), Some(129), "MC7 path");
    let only = run_libra_command(&["commit", "-o", "shared.txt", "--no-verify"], p);
    assert_eq!(only.status.code(), Some(129), "MC7 -o");
    assert!(
        p.join(".libra/merge-state.json").exists(),
        "MC7 keeps state"
    );

    // MC2 / MC2b / MC2c: -m creates a two-parent commit and clears merge state.
    let committed = run_libra_command(&["commit", "-m", "finish merge", "--no-verify"], p);
    assert_cli_success(&committed, "MC2");
    assert_eq!(head_parent_count(p), 2, "MC2 two parents");
    assert!(
        head_message_body(p).contains("finish merge"),
        "MC2 message: {}",
        head_message_body(p)
    );
    assert!(!p.join(".libra/merge-state.json").exists(), "MC2 cleared");
    let abort = run_libra_command(&["merge", "--abort"], p);
    assert!(
        String::from_utf8_lossy(&abort.stderr).contains("no merge in progress"),
        "MC2b: {}",
        String::from_utf8_lossy(&abort.stderr)
    );
    let cont = run_libra_command(&["merge", "--continue"], p);
    assert!(
        String::from_utf8_lossy(&cont.stderr).contains("no merge in progress"),
        "MC2c: {}",
        String::from_utf8_lossy(&cont.stderr)
    );
    let after_abort = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    let after_commit = String::from_utf8_lossy(&committed.stdout);
    assert!(!after_abort.is_empty(), "MC2b HEAD still exists");
    let _ = after_commit;

    // MC1: editor does not change the seeded merge message (comments stripped).
    let repo = merge_conflict_repo_for_commit();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["merge", "feature"], p).status.code(),
        Some(128)
    );
    resolve_merge_conflict(p);
    let mc1 = run_libra_command_with_env(&["commit", "--no-verify"], p, &[("GIT_EDITOR", "true")]);
    assert_cli_success(&mc1, "MC1");
    assert_eq!(head_parent_count(p), 2, "MC1 two parents");
    let body = head_message_body(p);
    assert!(
        !body.contains("# Conflicts:"),
        "MC1 editor cleanup strips comments: {body}"
    );
    assert!(!p.join(".libra/merge-state.json").exists(), "MC1 cleared");

    // MC3: --no-edit keeps # Conflicts:.
    let repo = merge_conflict_repo_for_commit();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["merge", "feature"], p).status.code(),
        Some(128)
    );
    resolve_merge_conflict(p);
    let mc3 = run_libra_command(&["commit", "--no-edit", "--no-verify"], p);
    assert_cli_success(&mc3, "MC3");
    assert_eq!(head_parent_count(p), 2, "MC3 two parents");
    let body = head_message_body(p);
    assert!(body.contains("# Conflicts:"), "MC3 keeps comments: {body}");

    // MC4: -F overrides the message.
    let repo = merge_conflict_repo_for_commit();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["merge", "feature"], p).status.code(),
        Some(128)
    );
    resolve_merge_conflict(p);
    std::fs::write(p.join("msg.txt"), "from file\n").unwrap();
    let mc4 = run_libra_command(&["commit", "-F", "msg.txt", "--no-verify"], p);
    assert_cli_success(&mc4, "MC4");
    assert_eq!(head_parent_count(p), 2, "MC4 two parents");
    assert!(
        head_message_body(p).contains("from file"),
        "{}",
        head_message_body(p)
    );

    // MC8: commit -a after a resolved worktree edit.
    let repo = merge_conflict_repo_for_commit();
    let p = repo.path();
    assert_eq!(
        run_libra_command(&["merge", "feature"], p).status.code(),
        Some(128)
    );
    resolve_merge_conflict(p);
    std::fs::write(p.join("shared.txt"), "top\nl1\nRESOLVED-A\nl3\nbottom\n").unwrap();
    let mc8 = run_libra_command(&["commit", "-a", "-m", "via -a", "--no-verify"], p);
    assert_cli_success(&mc8, "MC8");
    assert_eq!(head_parent_count(p), 2, "MC8 two parents");
    assert!(!p.join(".libra/merge-state.json").exists(), "MC8 cleared");

    // MC11: clean --no-commit then commit.
    let repo = merge_conflict_repo_for_commit();
    let p = repo.path();
    // Make a non-conflicting extra file merge via --no-commit on a clean-able pair.
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], p),
        "mc11 feature",
    );
    std::fs::write(p.join("extra.txt"), "only feature\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "extra.txt"], p), "mc11 add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "feature extra", "--no-verify"], p),
        "mc11 commit extra",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], p), "mc11 main");
    // Use a second repo that's clean: merge --no-commit of a non-conflicting branch.
    let clean = create_committed_repo_via_cli();
    let cp = clean.path();
    std::fs::write(cp.join("base.txt"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "base.txt"], cp), "base add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], cp),
        "base commit",
    );
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], cp),
        "clean branch",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], cp),
        "clean feature",
    );
    std::fs::write(cp.join("feat.txt"), "feat\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "feat.txt"], cp), "feat add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "feat", "--no-verify"], cp),
        "feat commit",
    );
    assert_cli_success(&run_libra_command(&["checkout", "main"], cp), "clean main");
    assert_cli_success(
        &run_libra_command(&["merge", "--no-commit", "feature"], cp),
        "MC11 no-commit",
    );
    assert!(cp.join(".libra/merge-state.json").exists());
    let mc11 = run_libra_command(&["commit", "--no-edit", "--no-verify"], cp);
    assert_cli_success(&mc11, "MC11 commit");
    assert_eq!(head_parent_count(cp), 2, "MC11 two parents");
    assert!(
        head_message_body(cp).to_ascii_lowercase().contains("merge"),
        "MC11 prefilled merge message: {}",
        head_message_body(cp)
    );
    assert!(!cp.join(".libra/merge-state.json").exists(), "MC11 cleared");
}

fn stage_one(repo: &std::path::Path, name: &str, body: &str) {
    std::fs::write(repo.join(name), body).unwrap();
    assert_cli_success(&run_libra_command(&["add", name], repo), name);
}

/// M-EMPTY E1–E3, E5, E6, E8–E10 (#477 HF-06).
#[test]
fn test_commit_allow_empty_message_matrix() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    stage_one(p, "e1.txt", "e1\n");
    let e1 = run_libra_command(&["commit", "-m", "", "--no-verify"], p);
    assert_eq!(e1.status.code(), Some(128), "E1 still aborts");
    assert!(
        String::from_utf8_lossy(&e1.stderr).contains("empty commit message"),
        "E1: {}",
        String::from_utf8_lossy(&e1.stderr)
    );

    let e2 = run_libra_command(
        &["commit", "--allow-empty-message", "-m", "", "--no-verify"],
        p,
    );
    assert_cli_success(&e2, "E2 empty -m");

    stage_one(p, "e3.txt", "e3\n");
    let empty_file = p.join("empty-msg.txt");
    std::fs::write(&empty_file, "").unwrap();
    let e3 = run_libra_command(
        &[
            "commit",
            "--allow-empty-message",
            "-F",
            empty_file.to_str().unwrap(),
            "--no-verify",
        ],
        p,
    );
    assert_cli_success(&e3, "E3 empty -F");

    stage_one(p, "e5.txt", "e5\n");
    let e5 = run_libra_command(
        &[
            "commit",
            "--allow-empty-message",
            "-m",
            "   ",
            "--no-verify",
        ],
        p,
    );
    assert_cli_success(&e5, "E5 whitespace -m");

    let e6 = run_libra_command(
        &[
            "commit",
            "--amend",
            "--allow-empty-message",
            "-m",
            "",
            "--no-verify",
        ],
        p,
    );
    assert_cli_success(&e6, "E6 amend empty");

    // E8: commit-msg hook still rejects an empty message file.
    stage_one(p, "e8.txt", "e8\n");
    let hook_dir = p.join(".libra/hooks");
    std::fs::create_dir_all(&hook_dir).unwrap();
    let hook = hook_dir.join("commit-msg");
    std::fs::write(
        &hook,
        "#!/bin/sh\nif ! grep -q '[^[:space:]]' \"$1\"; then echo hook-empty; exit 1; fi\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&hook).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook, perms).unwrap();
    }
    let e8 = run_libra_command(&["commit", "--allow-empty-message", "-m", ""], p);
    assert_ne!(e8.status.code(), Some(0), "E8 hook still refuses");
    assert!(
        String::from_utf8_lossy(&e8.stderr).contains("hook")
            || String::from_utf8_lossy(&e8.stderr).contains("commit-msg"),
        "E8: {}",
        String::from_utf8_lossy(&e8.stderr)
    );
    std::fs::remove_file(&hook).unwrap();

    // E9: empty message still signs when vault signing is already on
    // (`create_committed_repo_via_cli` uses default `init`, which creates
    // the PGP key — a second `generate-gpg-key` would LBR-CONFLICT-002).
    stage_one(p, "e9.txt", "e9\n");
    let e9 = run_libra_command(
        &[
            "--json",
            "commit",
            "--allow-empty-message",
            "-m",
            "",
            "--no-verify",
        ],
        p,
    );
    assert_cli_success(&e9, "E9 signed empty");
    assert_eq!(
        parse_json_stdout(&e9)["data"]["signed"].as_bool(),
        Some(true),
        "E9 signed: {}",
        String::from_utf8_lossy(&e9.stdout)
    );

    // E10: log / show / oneline render an empty subject.
    let log = run_libra_command(&["log", "-1"], p);
    assert_cli_success(&log, "E10 log");
    let oneline = run_libra_command(&["log", "-1", "--oneline"], p);
    assert_cli_success(&oneline, "E10 oneline");
    let show = run_libra_command(&["show", "-s", "--oneline"], p);
    assert_cli_success(&show, "E10 show");
}

/// SW-05 (M-HONOR H4, plan issues/490): `commit -a` keeps a skip-worktree
/// entry whose worktree file is absent and preserves its extended bit.
#[test]
fn test_commit_all_keeps_skip_worktree_entry() {
    use git_internal::{
        hash::HashKind,
        internal::index::{Index, IndexEntry},
    };

    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    fs::write(root.join("s"), "s\n").expect("write s");
    fs::write(root.join("other"), "other\n").expect("write other");
    assert_cli_success(&run_libra_command(&["add", "s", "other"], root), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "commit",
    );

    let index_path = root.join(".libra/index");
    {
        let mut index = Index::load_with_hash_kind(HashKind::Sha1, &index_path).expect("load");
        let (hash, mode, size) = {
            let entry = index.get("s", 0).expect("tracked");
            (entry.hash, entry.mode, entry.size)
        };
        let mut entry = IndexEntry::new_from_blob("s".to_string(), hash, size);
        entry.mode = mode;
        entry.flags.skip_worktree = true;
        index.update(entry);
        index
            .save_with_hash_kind(HashKind::Sha1, &index_path)
            .expect("save");
    }
    fs::remove_file(root.join("s")).expect("remove s");
    // Give `commit -a` a real change to commit; the skip-worktree deletion
    // alone is correctly "nothing to commit".
    fs::write(root.join("other"), "changed\n").expect("modify other");

    assert_cli_success(
        &run_libra_command(&["commit", "-a", "-m", "h4", "--no-verify"], root),
        "commit -a",
    );
    let index = Index::load_with_hash_kind(HashKind::Sha1, &index_path).expect("reload");
    let entry = index.get("s", 0).expect("s must stay tracked");
    assert!(entry.flags.skip_worktree, "the bit must survive commit -a");
}

/// FM-04 (M-DET D4/D6, plan-20260918): `commit -a` records a mode-only change
/// when `core.fileMode` is enabled and reports nothing to commit when false.
#[cfg(unix)]
#[test]
fn test_commit_all_updates_mode_only_change() {
    use std::os::unix::fs::PermissionsExt;

    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    let run = root.join("run.sh");
    fs::write(&run, "#!/bin/sh\n").expect("write");
    fs::set_permissions(&run, fs::Permissions::from_mode(0o755)).expect("chmod 755");
    assert_cli_success(&run_libra_command(&["add", "run.sh"], root), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "commit",
    );

    // D4: mode-only change is committed.
    fs::set_permissions(&run, fs::Permissions::from_mode(0o644)).expect("chmod 644");
    assert_cli_success(
        &run_libra_command(&["commit", "-a", "-m", "mode", "--no-verify"], root),
        "commit -a",
    );
    let tree = run_libra_command(&["ls-tree", "HEAD", "run.sh"], root);
    assert!(
        String::from_utf8_lossy(&tree.stdout).starts_with("100644"),
        "D4 tree mode: {}",
        String::from_utf8_lossy(&tree.stdout)
    );

    // D6: with fileMode=false a further chmod is not a change to commit.
    assert_cli_success(
        &run_libra_command(&["config", "set", "core.fileMode", "false"], root),
        "disable fileMode",
    );
    fs::set_permissions(&run, fs::Permissions::from_mode(0o755)).expect("chmod 755");
    let out = run_libra_command(&["commit", "-a", "-m", "ignored", "--no-verify"], root);
    assert_ne!(out.status.code(), Some(0), "D6 must have nothing to commit");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("nothing to commit")
            || String::from_utf8_lossy(&out.stderr).contains("nothing added")
            || String::from_utf8_lossy(&out.stderr).contains("no changes added"),
        "D6 message: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
