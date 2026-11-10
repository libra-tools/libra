//! Integration tests for the commit command covering staged changes, message handling, and tree/hash updates.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::path::Path;

use libra::{
    command::commit::CleanupMode,
    utils::{object_ext::TreeExt, output::OutputConfig},
};
use sea_orm::{ConnectionTrait, Database, Statement};
use serial_test::serial;
use tempfile::tempdir;

use super::*;
mod env_restore_tests;

mod unmerged;

const OBJECT_INDEX_REJECTION_TRIGGER: &str = "test_reject_object_index_write";

async fn set_object_index_write_rejection(repo: &Path, enabled: bool) {
    let db_path = repo.join(".libra/libra.db");
    let conn = Database::connect(format!("sqlite://{}", db_path.display()))
        .await
        .expect("open repository database for object-index fixture");
    let sql = if enabled {
        format!(
            "CREATE TRIGGER {OBJECT_INDEX_REJECTION_TRIGGER} \
             BEFORE INSERT ON object_index BEGIN \
             SELECT RAISE(ABORT, 'test object-index write rejection'); END"
        )
    } else {
        format!("DROP TRIGGER IF EXISTS {OBJECT_INDEX_REJECTION_TRIGGER}")
    };
    conn.execute_raw(Statement::from_string(conn.get_database_backend(), sql))
        .await
        .expect("install object-index rejection fixture");
    conn.close()
        .await
        .expect("close object-index fixture connection");
}

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
        assert_eq!(tree.tree_items.len(), 2); // a.txt and bb/
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
        assert_eq!(tree.tree_items.len(), 2); // a.txt and bb/
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
    let entries = tree.get_plain_items().expect("storage");
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
    let entries = tree.get_plain_items().expect("storage");
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
        separate_git_dir: None,
        separate_libra_dir: None,
        recurse_submodules: false,
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

#[cfg(feature = "test-live-agent")]
#[tokio::test]
#[ignore = "local-only Codex session association; never run in CI"]
#[serial(cwd)]
async fn agent_trailer_opt_in_links_scoped_codex_session_and_flag_omits_only_trailer() {
    use libra::internal::ai::capture_scope::CaptureScope;

    let repo = tempdir().unwrap();
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = ChangeDirGuard::new(repo.path());
    assert_cli_success(
        &run_libra_command(
            &["commit", "--allow-empty", "--no-gpg-sign", "-m", "baseline"],
            repo.path(),
        ),
        "create parent commit",
    );
    let parent = Head::current_commit().await.expect("parent commit");
    let conn = Database::connect(format!(
        "sqlite://{}",
        repo.path().join(".libra/libra.db").display()
    ))
    .await
    .expect("open repository catalog");
    let scope = CaptureScope::resolve(&conn, repo.path())
        .await
        .expect("resolve test worktree scope");
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_session (session_id, agent_kind, provider_session_id,
             state, working_dir, parent_commit, started_at, last_event_at,
             repo_id, worktree_id, workspace_id, workspace_fence, scope_state)
         VALUES (?, 'codex', ?, 'active', ?, ?, 1, 2, ?, ?, ?, ?, 'scoped')",
        [
            "codex__linked".into(),
            "provider-linked".into(),
            repo.path().to_string_lossy().to_string().into(),
            parent.to_string().into(),
            scope.repo_id.into(),
            scope.worktree_id.into(),
            scope.workspace_id.into(),
            scope.workspace_fence.into(),
        ],
    ))
    .await
    .expect("insert scoped Codex session");
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope,
             parent_commit, tree_oid, metadata_blob_oid, traces_commit, created_at)
         VALUES ('checkpoint-linked', 'codex__linked', 'committed', ?, ?, ?, ?, 2)",
        [
            parent.to_string().into(),
            parent.to_string().into(),
            parent.to_string().into(),
            parent.to_string().into(),
        ],
    ))
    .await
    .expect("insert committed checkpoint");

    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--allow-empty",
                "--no-gpg-sign",
                "-m",
                "default-off",
            ],
            repo.path(),
        ),
        "default commit",
    );
    let off_oid = Head::current_commit().await.expect("default commit ID");
    let off_commit: Commit = load_object(&off_oid).expect("default commit object");
    assert!(!off_commit.message.contains("Libra-Agent-Session:"));
    let off_links = libra::internal::ai::history::agent_sessions_for_commit(&conn, off_oid)
        .await
        .expect("read default-off links");
    assert!(off_links.is_empty());
    assert!(
        !libra::internal::ai::commit_journal::journal_path(&repo.path().join(".libra")).exists(),
        "default-off commit must not create recovery evidence"
    );
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_session (session_id, agent_kind, provider_session_id,
             state, working_dir, parent_commit, started_at, last_event_at,
             repo_id, worktree_id, workspace_id, workspace_fence, scope_state)
         SELECT 'codex__foreign', agent_kind, 'provider-foreign', state,
                working_dir, parent_commit, started_at, last_event_at,
                repo_id, 'foreign-worktree', workspace_id, workspace_fence, scope_state
         FROM agent_session WHERE session_id = 'codex__linked'"
            .to_string(),
    ))
    .await
    .expect("insert foreign-worktree session");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope,
             parent_commit, tree_oid, metadata_blob_oid, traces_commit, created_at)
         SELECT 'checkpoint-foreign', 'codex__foreign', scope,
                parent_commit, tree_oid, metadata_blob_oid, traces_commit, created_at
         FROM agent_checkpoint WHERE checkpoint_id = 'checkpoint-linked'"
            .to_string(),
    ))
    .await
    .expect("insert foreign-worktree checkpoint");
    // Model a fresh Codex turn after the default-off commit; its captured
    // parent is now the commit about to be advanced by the opt-in call.
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE agent_session SET parent_commit = ?
         WHERE session_id IN ('codex__linked', 'codex__foreign')",
        [off_oid.to_string().into()],
    ))
    .await
    .expect("anchor selected session to the current parent");
    for (
        suffix,
        repo_id,
        worktree_id,
        workspace_id,
        workspace_fence,
        scope_state,
        state,
        checkpoint,
    ) in [
        (
            "foreign_repo",
            "'foreign-repository'",
            "worktree_id",
            "workspace_id",
            "workspace_fence",
            "scope_state",
            "state",
            true,
        ),
        (
            "foreign_workspace",
            "repo_id",
            "worktree_id",
            "'foreign-workspace'",
            "1",
            "scope_state",
            "state",
            true,
        ),
        (
            "legacy_scope",
            "repo_id",
            "worktree_id",
            "workspace_id",
            "workspace_fence",
            "'legacy_unknown'",
            "state",
            true,
        ),
        (
            "no_checkpoint",
            "repo_id",
            "worktree_id",
            "workspace_id",
            "workspace_fence",
            "scope_state",
            "state",
            false,
        ),
        (
            "quarantined",
            "repo_id",
            "worktree_id",
            "workspace_id",
            "workspace_fence",
            "scope_state",
            "'quarantined'",
            true,
        ),
    ] {
        let session_id = format!("codex__{suffix}");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            format!(
                "INSERT INTO agent_session (session_id, agent_kind, provider_session_id,
                     state, working_dir, parent_commit, started_at, last_event_at,
                     repo_id, worktree_id, workspace_id, workspace_fence, scope_state)
                 SELECT ?, agent_kind, ?, {state}, working_dir, parent_commit,
                        started_at, last_event_at, {repo_id}, {worktree_id},
                        {workspace_id}, {workspace_fence}, {scope_state}
                 FROM agent_session WHERE session_id = 'codex__linked'"
            ),
            [
                session_id.as_str().into(),
                format!("provider-{suffix}").into(),
            ],
        ))
        .await
        .expect("insert mismatched session fixture");
        if checkpoint {
            conn.execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope,
                     parent_commit, tree_oid, metadata_blob_oid, traces_commit, created_at)
                 SELECT ?, ?, scope, parent_commit, tree_oid,
                        metadata_blob_oid, traces_commit, created_at
                 FROM agent_checkpoint WHERE checkpoint_id = 'checkpoint-linked'",
                [format!("checkpoint-{suffix}").into(), session_id.into()],
            ))
            .await
            .expect("insert mismatched checkpoint fixture");
        }
    }

    assert_cli_success(
        &run_libra_command(&["config", "agent.commit_trailer", "true"], repo.path()),
        "enable agent commit trailer",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "--allow-empty", "--no-gpg-sign", "-m", "opt-in"],
            repo.path(),
        ),
        "opt-in commit",
    );
    let enabled_oid = Head::current_commit().await.expect("opt-in commit ID");
    let enabled_commit: Commit = load_object(&enabled_oid).expect("opt-in commit object");
    assert_eq!(
        enabled_commit
            .message
            .matches("Libra-Agent-Session: codex__linked")
            .count(),
        1
    );
    assert_eq!(
        libra::internal::ai::history::agent_sessions_for_commit(&conn, enabled_oid)
            .await
            .expect("read opt-in links"),
        vec!["codex__linked"]
    );
    assert_cli_success(
        &run_libra_command(&["config", "generate-gpg-key"], repo.path()),
        "create a vault signing key for the signed trailer check",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "--allow-empty", "-S", "-m", "signed-agent"],
            repo.path(),
        ),
        "sign an opt-in agent commit",
    );
    let signed_oid = Head::current_commit()
        .await
        .expect("signed agent commit ID");
    let signed_commit: Commit = load_object(&signed_oid).expect("signed agent commit object");
    assert!(signed_commit.message.contains("gpgsig "));
    assert!(
        signed_commit
            .message
            .contains("Libra-Agent-Session: codex__linked")
    );
    assert_eq!(
        libra::internal::ai::history::agent_sessions_for_commit(&conn, signed_oid)
            .await
            .expect("read signed commit links"),
        vec!["codex__linked"]
    );

    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--allow-empty",
                "--no-gpg-sign",
                "--no-agent-trailer",
                "-m",
                "local-link-only",
            ],
            repo.path(),
        ),
        "commit without automatic trailer",
    );
    let suppressed_oid = Head::current_commit().await.expect("suppressed commit ID");
    let suppressed_commit: Commit = load_object(&suppressed_oid).expect("suppressed commit object");
    assert!(!suppressed_commit.message.contains("Libra-Agent-Session:"));
    assert_eq!(
        libra::internal::ai::history::agent_sessions_for_commit(&conn, suppressed_oid)
            .await
            .expect("read suppressed links"),
        vec!["codex__linked"]
    );
    let journal_path =
        libra::internal::ai::commit_journal::journal_path(&repo.path().join(".libra"));
    let (records, bad_tail) =
        libra::internal::ai::commit_journal::read(&repo.path().join(".libra"))
            .expect("read durable trailer-free journal");
    assert!(!bad_tail);
    assert!(records.iter().any(|record| {
        record.commit_oid == suppressed_oid.to_string()
            && record.stage == libra::internal::ai::commit_journal::CommitJournalStage::Prepared
            && record.session_ids.len() == 1
            && record.session_ids[0] == "codex__linked"
    }));
    assert!(records.iter().any(|record| {
        record.commit_oid == suppressed_oid.to_string()
            && record.stage == libra::internal::ai::commit_journal::CommitJournalStage::Linked
    }));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&journal_path)
                .expect("journal metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let saved = repo.path().join(".libra/agent-commit-links.saved");
        std::fs::rename(&journal_path, &saved).expect("hold journal aside for symlink refusal");
        let protected = repo.path().join("protected-journal-target");
        std::fs::write(&protected, b"unchanged").expect("write protected target");
        std::os::unix::fs::symlink(&protected, &journal_path).expect("place journal symlink");
        let refused = run_libra_command(
            &[
                "commit",
                "--allow-empty",
                "--no-gpg-sign",
                "--no-agent-trailer",
                "-m",
                "journal-symlink-refusal",
            ],
            repo.path(),
        );
        assert!(
            !refused.status.success(),
            "symlink journal must refuse commit"
        );
        assert_eq!(
            Head::current_commit()
                .await
                .expect("head after journal failure"),
            suppressed_oid
        );
        assert_eq!(
            std::fs::read(&protected).expect("protected target"),
            b"unchanged"
        );
        std::fs::remove_file(&journal_path).expect("remove symlink");
        std::fs::rename(&saved, &journal_path).expect("restore journal");
    }
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "CREATE TRIGGER test_reject_agent_journal_reflog BEFORE INSERT ON reflog BEGIN SELECT RAISE(ABORT, 'injected reflog failure'); END".to_string(),
    ))
    .await
    .expect("install ref transaction failure");
    let failed_ref = run_libra_command(
        &[
            "commit",
            "--allow-empty",
            "--no-gpg-sign",
            "--no-agent-trailer",
            "-m",
            "journal-ref-rollback",
        ],
        repo.path(),
    );
    assert!(
        !failed_ref.status.success(),
        "injected reflog failure must abort commit"
    );
    assert_eq!(
        Head::current_commit()
            .await
            .expect("head after ref rollback"),
        suppressed_oid
    );
    let (after_ref_failure, bad_tail) =
        libra::internal::ai::commit_journal::read(&repo.path().join(".libra"))
            .expect("read prepared intent after ref rollback");
    assert!(!bad_tail);
    assert!(after_ref_failure.len() > records.len());
    assert!(after_ref_failure.iter().any(|record| {
        record.stage == libra::internal::ai::commit_journal::CommitJournalStage::Prepared
            && record.commit_oid != suppressed_oid.to_string()
    }));
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "DROP TRIGGER test_reject_agent_journal_reflog".to_string(),
    ))
    .await
    .expect("remove ref transaction failure");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let hook_dir = repo.path().join(".libra/hooks");
        std::fs::create_dir_all(&hook_dir).expect("create hook directory");
        let hook = hook_dir.join("commit-msg");
        std::fs::write(&hook, "#!/bin/sh\nprintf '\n\nHook-Ack: yes\n' >> \"$1\"\n")
            .expect("write commit-msg hook");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
            .expect("make commit-msg hook executable");
    }
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--allow-empty",
                "--no-gpg-sign",
                "-m",
                "user-trailer",
                "--trailer",
                "Libra-Agent-Session: codex__linked",
            ],
            repo.path(),
        ),
        "commit with user-provided session trailer",
    );
    let user_oid = Head::current_commit()
        .await
        .expect("user-trailer commit ID");
    let user_commit: Commit = load_object(&user_oid).expect("user-trailer commit object");
    assert_eq!(
        user_commit
            .message
            .matches("Libra-Agent-Session: codex__linked")
            .count(),
        1
    );
    #[cfg(unix)]
    assert!(user_commit.message.contains("Hook-Ack: yes"));
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "UPDATE agent_session SET state = 'stopped' WHERE session_id = 'codex__linked'".to_string(),
    ))
    .await
    .expect("stop linked session before amend");
    assert_cli_success(
        &run_libra_command(
            &["commit", "--amend", "--no-edit", "--no-gpg-sign"],
            repo.path(),
        ),
        "amend a commit with an existing session trailer",
    );
    let amended_oid = Head::current_commit().await.expect("amended commit ID");
    let amended_commit: Commit = load_object(&amended_oid).expect("amended commit object");
    assert_eq!(
        amended_commit
            .message
            .matches("Libra-Agent-Session: codex__linked")
            .count(),
        1
    );
    assert_eq!(
        libra::internal::ai::history::agent_sessions_for_commit(&conn, amended_oid)
            .await
            .expect("read amended links"),
        vec!["codex__linked"]
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--amend",
                "--no-gpg-sign",
                "-m",
                "rewritten-amend",
            ],
            repo.path(),
        ),
        "rewrite amend message while preserving stopped session link",
    );
    let rewritten_oid = Head::current_commit().await.expect("rewritten amend ID");
    let rewritten_commit: Commit = load_object(&rewritten_oid).expect("rewritten amend object");
    assert_eq!(
        rewritten_commit
            .message
            .matches("Libra-Agent-Session: codex__linked")
            .count(),
        1
    );
    assert_eq!(
        libra::internal::ai::history::agent_sessions_for_commit(&conn, rewritten_oid)
            .await
            .expect("read rewritten amend links"),
        vec!["codex__linked"]
    );
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "UPDATE agent_session SET state = 'active' WHERE session_id = 'codex__linked'".to_string(),
    ))
    .await
    .expect("resume linked session before normal commit");
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--allow-empty",
                "--no-gpg-sign",
                "-m",
                "spaced-trailer",
                "--trailer",
                "Libra-Agent-Session : codex__linked",
                "--trailer",
                "Notes: first\n continued",
            ],
            repo.path(),
        ),
        "commit with Git-valid spaced and continuation trailers",
    );
    let spaced_oid = Head::current_commit()
        .await
        .expect("spaced trailer commit ID");
    let spaced_commit: Commit = load_object(&spaced_oid).expect("spaced trailer commit object");
    assert_eq!(
        spaced_commit
            .message
            .matches("Libra-Agent-Session: codex__linked")
            .count(),
        1
    );
    assert!(
        !spaced_commit
            .message
            .contains("Libra-Agent-Session : codex__linked"),
        "a hook-split user trailer must move to one final parseable location"
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--allow-empty",
                "--no-gpg-sign",
                "-m",
                "body-marker\n\nLibra-Agent-Session: codex__linked\n\nBody text",
            ],
            repo.path(),
        ),
        "commit with a trailer-like body line",
    );
    let body_oid = Head::current_commit().await.expect("body-marker commit ID");
    let body_commit: Commit = load_object(&body_oid).expect("body-marker commit object");
    assert_eq!(
        body_commit
            .message
            .matches("Libra-Agent-Session: codex__linked")
            .count(),
        2,
        "a body line must not suppress the actual final trailer"
    );
    assert!(
        libra::internal::log::trailer::parse_trailers(&body_commit.message)
            .iter()
            .any(|trailer| trailer.key_matches("Libra-Agent-Session")
                && trailer.value == "codex__linked")
    );
    assert_cli_success(
        &run_libra_command(&["config", "agent.commit_trailer", "invalid"], repo.path()),
        "set invalid config fixture",
    );
    let rejected = run_libra_command(
        &["commit", "--allow-empty", "--no-gpg-sign", "-m", "rejected"],
        repo.path(),
    );
    assert!(
        !rejected.status.success(),
        "invalid config must reject commit"
    );
    assert_eq!(rejected.status.code(), Some(129));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("agent.commit_trailer"));
    let rejected_preview = run_libra_command(
        &[
            "commit",
            "--dry-run",
            "--allow-empty",
            "-m",
            "invalid-preview",
        ],
        repo.path(),
    );
    assert_eq!(rejected_preview.status.code(), Some(129));
    assert!(
        String::from_utf8_lossy(&rejected_preview.stderr).contains("agent.commit_trailer"),
        "dry-run must reject the invalid value, not an empty index"
    );
    assert_eq!(Head::current_commit().await, Some(body_oid));
    assert_cli_success(
        &run_libra_command(&["config", "agent.commit_trailer", "true"], repo.path()),
        "restore valid agent trailer config",
    );
    let links_before_preview = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS total FROM agent_session_commit".to_string(),
        ))
        .await
        .expect("count links before valid dry run")
        .expect("link count row")
        .try_get::<i64>("", "total")
        .expect("numeric link count");
    let valid_preview = run_libra_command(
        &[
            "--json",
            "commit",
            "--dry-run",
            "--allow-empty",
            "-m",
            "valid-preview",
        ],
        repo.path(),
    );
    assert_cli_success(&valid_preview, "valid opt-in dry run");
    assert!(
        !String::from_utf8_lossy(&valid_preview.stdout).contains("Libra-Agent-Session"),
        "dry run must not preview an automatic trailer"
    );
    assert_eq!(Head::current_commit().await, Some(body_oid));
    let links_after_preview = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS total FROM agent_session_commit".to_string(),
        ))
        .await
        .expect("count links after valid dry run")
        .expect("link count row")
        .try_get::<i64>("", "total")
        .expect("numeric link count");
    assert_eq!(links_after_preview, links_before_preview);
    for number in 0..64 {
        let session_id = format!("codex__overflow_{number}");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id,
                 state, working_dir, parent_commit, started_at, last_event_at,
                 repo_id, worktree_id, workspace_id, workspace_fence, scope_state)
             SELECT ?, agent_kind, ?, state, working_dir, ?, 1, 2,
                    repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             FROM agent_session WHERE session_id = 'codex__linked'",
            [
                session_id.as_str().into(),
                format!("provider-overflow-{number}").into(),
                body_oid.to_string().into(),
            ],
        ))
        .await
        .expect("insert bounded selection fixture");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope,
                 parent_commit, tree_oid, metadata_blob_oid, traces_commit, created_at)
             SELECT ?, ?, scope, parent_commit, tree_oid,
                    metadata_blob_oid, traces_commit, created_at
             FROM agent_checkpoint WHERE checkpoint_id = 'checkpoint-linked'",
            [
                format!("checkpoint-overflow-{number}").into(),
                session_id.into(),
            ],
        ))
        .await
        .expect("insert bounded checkpoint fixture");
    }
    let over_cap = run_libra_command(
        &["commit", "--allow-empty", "--no-gpg-sign", "-m", "over-cap"],
        repo.path(),
    );
    assert!(
        !over_cap.status.success(),
        "65 sessions must reject before ref update"
    );
    assert!(
        String::from_utf8_lossy(&over_cap.stderr).contains("more than 64 eligible sessions"),
        "selection cap must explain the refusal"
    );
    assert_eq!(Head::current_commit().await, Some(body_oid));
    let current_scope =
        libra::internal::ai::capture_scope::CaptureScope::resolve(&conn, repo.path())
            .await
            .expect("resolve current link scope");
    let cross_scope = libra::internal::ai::history::link_authorized_agent_sessions_to_commit(
        &conn,
        &["codex__linked".to_string(), "codex__foreign".to_string()],
        off_oid,
        &current_scope,
    )
    .await;
    assert!(cross_scope.is_err(), "cross-worktree link must roll back");
    assert!(
        libra::internal::ai::history::agent_sessions_for_commit(&conn, off_oid)
            .await
            .expect("read rejected transaction links")
            .is_empty(),
        "a rejected mixed-scope batch must leave zero rows"
    );
    let lease_scope = CaptureScope {
        repo_id: current_scope.repo_id.clone(),
        worktree_id: current_scope.worktree_id.clone(),
        workspace_id: Some("lease-fixture".to_string()),
        workspace_fence: Some(1),
    };
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO workspace_record (workspace_id, repo_id, kind, worktree_id,
             path, owner_kind, state, lease_owner, lease_fence,
             lease_expires_at, created_at, updated_at)
         VALUES ('lease-fixture', ?, 'task_copy', ?, ?, 'agent', 'active',
                 'owner', 1, ?, 1, 1)",
        [
            lease_scope.repo_id.as_str().into(),
            lease_scope.worktree_id.as_str().into(),
            repo.path()
                .join("lease-fixture")
                .to_string_lossy()
                .to_string()
                .into(),
            (chrono::Utc::now().timestamp_millis() + 60_000).into(),
        ],
    ))
    .await
    .expect("insert live workspace lease");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_session (session_id, agent_kind, provider_session_id,
             state, working_dir, parent_commit, started_at, last_event_at,
             repo_id, worktree_id, workspace_id, workspace_fence, scope_state)
         SELECT 'codex__leased', agent_kind, 'provider-leased', state,
                working_dir, parent_commit, started_at, last_event_at,
                repo_id, worktree_id, 'lease-fixture', 1, scope_state
         FROM agent_session WHERE session_id = 'codex__linked'"
            .to_string(),
    ))
    .await
    .expect("insert leased session");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope,
             parent_commit, tree_oid, metadata_blob_oid, traces_commit, created_at)
         SELECT 'checkpoint-leased', 'codex__leased', scope,
                parent_commit, tree_oid, metadata_blob_oid, traces_commit, created_at
         FROM agent_checkpoint WHERE checkpoint_id = 'checkpoint-linked'"
            .to_string(),
    ))
    .await
    .expect("insert leased checkpoint");
    lease_scope
        .assert_workspace_fence_live(&conn)
        .await
        .expect("lease is live before link attempt");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "UPDATE workspace_record SET lease_expires_at = 1
         WHERE workspace_id = 'lease-fixture'"
            .to_string(),
    ))
    .await
    .expect("expire workspace lease after precheck");
    assert!(
        libra::internal::ai::history::link_authorized_agent_sessions_to_commit(
            &conn,
            &["codex__leased".to_string()],
            off_oid,
            &lease_scope,
        )
        .await
        .is_err(),
        "expired lease must roll back a post-ref link"
    );
    assert!(
        libra::internal::ai::history::agent_sessions_for_commit(&conn, off_oid)
            .await
            .expect("read links after expired lease")
            .is_empty()
    );
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "DELETE FROM agent_session WHERE session_id GLOB 'codex__overflow_*'".to_string(),
    ))
    .await
    .expect("remove over-cap fixtures");
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "CREATE TRIGGER test_agent_link_failure BEFORE INSERT ON agent_session_commit
         BEGIN SELECT RAISE(ABORT, 'secret-link-failure-sentinel'); END"
            .to_string(),
    ))
    .await
    .expect("inject post-ref association failure");
    let warning = run_libra_command(
        &[
            "--json",
            "--exit-code-on-warning",
            "commit",
            "--allow-empty",
            "--no-gpg-sign",
            "--no-agent-trailer",
            "-m",
            "post-ref-warning",
        ],
        repo.path(),
    );
    assert_eq!(
        warning.status.code(),
        Some(9),
        "completed commit warning exit"
    );
    let warning_stderr = String::from_utf8_lossy(&warning.stderr);
    assert!(warning_stderr.contains("local agent session association failed"));
    assert!(!warning_stderr.contains("secret-link-failure-sentinel"));
    let warning_json: serde_json::Value =
        serde_json::from_slice(&warning.stdout).expect("parse successful warning envelope");
    assert_eq!(warning_json["ok"], true);
    let warning_oid = Head::current_commit()
        .await
        .expect("commit advanced before link warning");
    assert_ne!(warning_oid, body_oid);
    assert!(
        libra::internal::ai::history::agent_sessions_for_commit(&conn, warning_oid)
            .await
            .expect("read links after injected failure")
            .is_empty()
    );
    let (warning_records, bad_tail) =
        libra::internal::ai::commit_journal::read(&repo.path().join(".libra"))
            .expect("read recovery after post-ref DAO failure");
    assert!(!bad_tail);
    assert!(warning_records.iter().any(|record| {
        record.commit_oid == warning_oid.to_string()
            && record.stage == libra::internal::ai::commit_journal::CommitJournalStage::Prepared
    }));
    assert!(!warning_records.iter().any(|record| {
        record.commit_oid == warning_oid.to_string()
            && record.stage == libra::internal::ai::commit_journal::CommitJournalStage::Linked
    }));
    assert_cli_success(
        &run_libra_command(&["config", "--unset", "agent.commit_trailer"], repo.path()),
        "remove local agent trailer setting before unreadable-global check",
    );
    let unreadable_global = repo.path().join("unreadable-global-config.db");
    std::fs::create_dir(&unreadable_global).expect("create unreadable config-store fixture");
    let unreadable_global_path = unreadable_global
        .to_str()
        .expect("temporary repository path is UTF-8");
    let unreadable = run_libra_command_with_env(
        &[
            "--json",
            "--exit-code-on-warning",
            "commit",
            "--allow-empty",
            "--no-gpg-sign",
            "-m",
            "unreadable-global",
        ],
        repo.path(),
        &[("LIBRA_CONFIG_GLOBAL_DB", unreadable_global_path)],
    );
    assert_eq!(unreadable.status.code(), Some(9));
    let unreadable_json: serde_json::Value =
        serde_json::from_slice(&unreadable.stdout).expect("parse config warning envelope");
    assert_eq!(unreadable_json["ok"], true);
    let unreadable_stderr = String::from_utf8_lossy(&unreadable.stderr);
    assert!(unreadable_stderr.contains("agent session association was disabled"));
    assert!(unreadable_stderr.contains("agent.commit_trailer"));
    let unreadable_oid = Head::current_commit()
        .await
        .expect("commit succeeded with unreadable opt-in config");
    let unreadable_commit: Commit = load_object(&unreadable_oid).expect("unreadable config commit");
    assert!(!unreadable_commit.message.contains("Libra-Agent-Session"));
    assert!(
        libra::internal::ai::history::agent_sessions_for_commit(&conn, unreadable_oid)
            .await
            .expect("read links for opt-in config fallback")
            .is_empty()
    );
    conn.close().await.expect("close repository catalog");
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

#[tokio::test]
async fn commit_with_terminal_index_failure_is_repaired_without_recommitting() {
    let repo = tempdir().expect("create temp repo");
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    std::fs::write(p.join("indexed.txt"), "content\n").expect("write fixture");
    assert_cli_success(
        &run_libra_command(&["add", "indexed.txt"], p),
        "stage fixture",
    );

    set_object_index_write_rejection(p, true).await;
    let committed = run_libra_command_with_stdin(
        &["commit", "-m", "durable index repair", "--no-verify"],
        p,
        "",
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

    set_object_index_write_rejection(p, false).await;

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

    // MC7: partial commit is classified before touching merge state.
    let partial = run_libra_command(&["commit", "shared.txt", "--no-verify"], p);
    assert_eq!(partial.status.code(), Some(128), "MC7 path");
    assert!(String::from_utf8_lossy(&partial.stderr).contains("LBR-UNSUPPORTED-001"));
    let only = run_libra_command(&["commit", "-o", "shared.txt", "--no-verify"], p);
    assert_eq!(only.status.code(), Some(128), "MC7 -o");
    assert!(String::from_utf8_lossy(&only.stderr).contains("LBR-UNSUPPORTED-001"));
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

#[tokio::test]
async fn commit_blake3_object_readback() {
    let temp = tempdir().unwrap();
    let repo = temp.path();
    let init = run_libra_command(
        &["init", "--vault", "false", "--object-format", "blake3"],
        repo,
    );
    assert_cli_success(&init, "blake3 init");
    assert_cli_success(
        &run_libra_command(&["config", "--local", "user.name", "Blake3 User"], repo),
        "user.name",
    );
    assert_cli_success(
        &run_libra_command(
            &["config", "--local", "user.email", "blake3@example.com"],
            repo,
        ),
        "user.email",
    );
    std::fs::write(repo.join("a.txt"), b"hello blake3\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], repo), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "blake3 tip", "--no-verify"], repo),
        "commit",
    );

    let head = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], repo).stdout)
        .trim()
        .to_string();
    assert_eq!(head.len(), 64, "commit OID width: {head}");
    let tree =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD^{tree}"], repo).stdout)
            .trim()
            .to_string();
    assert_eq!(tree.len(), 64, "tree OID width: {tree}");

    let blob =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD:a.txt"], repo).stdout)
            .trim()
            .to_string();
    assert_eq!(blob.len(), 64, "blob OID width: {blob}");

    // Load objects under the repository kind to assert Blake3.
    let _guard = ChangeDirGuard::new(repo);
    let _hash = git_internal::hash::set_hash_kind_for_test(git_internal::hash::HashKind::Blake3);
    let head_oid = git_internal::hash::ObjectHash::from_hex_for_kind(
        git_internal::hash::HashKind::Blake3,
        &head,
    )
    .expect("parse head");
    assert_eq!(head_oid.kind(), git_internal::hash::HashKind::Blake3);
    let tree_oid = git_internal::hash::ObjectHash::from_hex_for_kind(
        git_internal::hash::HashKind::Blake3,
        &tree,
    )
    .expect("parse tree");
    assert_eq!(tree_oid.kind(), git_internal::hash::HashKind::Blake3);
    let blob_oid = git_internal::hash::ObjectHash::from_hex_for_kind(
        git_internal::hash::HashKind::Blake3,
        &blob,
    )
    .expect("parse blob");
    assert_eq!(blob_oid.kind(), git_internal::hash::HashKind::Blake3);

    let log = run_libra_command(&["log", "--oneline"], repo);
    assert_cli_success(&log, "log --oneline");
    let log_text = String::from_utf8_lossy(&log.stdout);
    assert!(
        log_text.contains("blake3 tip"),
        "log should show commit: {log_text}"
    );
}

/// Issue #497: deleting the LAST tracked file leaves the index empty, which
/// must still produce a deletion commit rather than being misreported as
/// "nothing to commit". This mirrors the Git upstream scenario
/// `git rm <last-file> && git commit` and covers the `commit -a` path
/// (the `libra rm` empty-index case).
#[test]
fn test_commit_delete_last_tracked_file_via_commit_all() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);

    // Track exactly one file so removing it empties the index (issue #497).
    std::fs::write(root.join("only.txt"), "only\n").expect("write only.txt");
    assert_cli_success(
        &run_libra_command(&["add", "only.txt"], root),
        "add only.txt",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "baseline", "--no-verify"], root),
        "baseline commit",
    );

    // Stage the deletion of the final tracked file; the index is now empty.
    assert_cli_success(&run_libra_command(&["rm", "only.txt"], root), "rm only.txt");

    // `commit -a` must commit the deletion instead of "nothing to commit".
    let commit_all = run_libra_command(&["commit", "-a", "-m", "delete last", "--no-verify"], root);
    assert_cli_success(&commit_all, "commit -a after deleting last tracked file");

    let log = run_libra_command(&["log", "--oneline"], root);
    assert_cli_success(&log, "log --oneline");
    let log_text = String::from_utf8_lossy(&log.stdout);
    assert!(
        log_text.contains("delete last"),
        "commit should land: {log_text}"
    );

    let ls_tree = run_libra_command(&["ls-tree", "HEAD"], root);
    assert_cli_success(&ls_tree, "ls-tree HEAD");
    assert!(
        !String::from_utf8_lossy(&ls_tree.stdout).contains("only.txt"),
        "only.txt must be removed from HEAD tree"
    );
}

/// Issue #497, second path: `libra add -A && libra commit`. Removing the final
/// tracked file and staging everything must produce a deletion commit, not
/// "nothing to commit".
#[test]
fn test_commit_delete_last_tracked_file_via_add_all() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);

    std::fs::write(root.join("only.txt"), "only\n").expect("write only.txt");
    assert_cli_success(
        &run_libra_command(&["add", "only.txt"], root),
        "add only.txt",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "baseline", "--no-verify"], root),
        "baseline commit",
    );

    // Remove the only tracked file from the worktree and stage everything.
    std::fs::remove_file(root.join("only.txt")).expect("remove only.txt");
    assert_cli_success(&run_libra_command(&["add", "-A"], root), "add -A");

    let commit = run_libra_command(&["commit", "-m", "delete last", "--no-verify"], root);
    assert_cli_success(&commit, "commit after add -A deleting last tracked file");

    let log = run_libra_command(&["log", "--oneline"], root);
    assert_cli_success(&log, "log --oneline");
    let log_text = String::from_utf8_lossy(&log.stdout);
    assert!(
        log_text.contains("delete last"),
        "commit should land: {log_text}"
    );
}

/// WT-07 / issue #476 M-ITA-WRITE W2–W4: intent-to-add entries never enter a
/// commit. Only-ITA refuses like Git's "no changes added"; unrelated staged
/// commits leave the placeholder in the index; `commit -a` commits the real
/// content and clears the flag.
#[test]
fn test_commit_intent_to_add_matrix() {
    const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    init_repo_via_cli(root);
    configure_identity_via_cli(root);
    fs::write(root.join("tracked.txt"), "tracked\n").expect("write tracked");
    assert_cli_success(&run_libra_command(&["add", "tracked.txt"], root), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "commit init",
    );
    let init_head = libra_stdout(&["rev-parse", "HEAD"], root)
        .trim()
        .to_string();

    // W2: only an intent-to-add entry — refuse, keep HEAD and the entry.
    fs::write(root.join("new.txt"), "real content\n").expect("write new");
    assert_cli_success(
        &run_libra_command(&["add", "-N", "new.txt"], root),
        "add -N",
    );
    let refused = run_libra_command(&["commit", "-m", "nope", "--no-verify"], root);
    assert_eq!(
        refused.status.code(),
        Some(128),
        "W2 exit: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("no changes added to commit"),
        "W2 message: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    let head_after = libra_stdout(&["rev-parse", "HEAD"], root)
        .trim()
        .to_string();
    assert_eq!(head_after, init_head, "W2 HEAD must not move");
    let files = libra_stdout(&["ls-files", "-s"], root);
    assert!(
        files.contains(EMPTY_BLOB) && files.contains("new.txt"),
        "W2 placeholder must remain: {files}"
    );

    // W3: an unrelated staged path commits, but the placeholder does not.
    fs::write(root.join("other.txt"), "other\n").expect("write other");
    assert_cli_success(&run_libra_command(&["add", "other.txt"], root), "add other");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "second", "--no-verify"], root),
        "commit second",
    );
    let tree = libra_stdout(&["ls-tree", "-r", "--name-only", "HEAD"], root);
    assert!(tree.contains("other.txt"), "W3 commit content: {tree}");
    assert!(
        !tree.contains("new.txt"),
        "W3 must not commit the placeholder: {tree}"
    );
    let files = libra_stdout(&["ls-files", "-s"], root);
    assert!(
        files.contains(EMPTY_BLOB) && files.contains("new.txt"),
        "W3 placeholder must survive the unrelated commit: {files}"
    );

    // W4: `commit -a` stages the real content and commits it.
    fs::write(root.join("new.txt"), "updated content\n").expect("rewrite new");
    assert_cli_success(
        &run_libra_command(&["commit", "-a", "-m", "auto", "--no-verify"], root),
        "commit -a",
    );
    let tree = libra_stdout(&["ls-tree", "-r", "--name-only", "HEAD"], root);
    assert!(tree.contains("new.txt"), "W4 tree: {tree}");
    let blob = run_libra_command(&["show", "HEAD:new.txt"], root);
    assert_cli_success(&blob, "show HEAD:new.txt");
    assert_eq!(
        String::from_utf8_lossy(&blob.stdout),
        "updated content\n",
        "W4 must commit the real worktree content"
    );
    let files = libra_stdout(&["ls-files", "-s"], root);
    assert!(
        !files.contains(EMPTY_BLOB),
        "W4 must clear the intent-to-add flag: {files}"
    );
}

// ---------------------------------------------------------------------------
// issue #525: stdin message, -S/--gpg-sign, --short, -u, and the classified
// partial-commit options.
// ---------------------------------------------------------------------------

fn commit_fixture() -> tempfile::TempDir {
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    super::init_repo_via_cli(root);
    super::configure_identity_via_cli(root);
    fs::write(root.join("a.txt"), "a\n").expect("write a");
    super::assert_cli_success(&run_libra_command(&["add", "a.txt"], root), "stage a");
    repo
}

/// `-F -` reads the whole commit message from standard input.
#[test]
fn commit_message_from_stdin() {
    let repo = commit_fixture();
    let root = repo.path();
    let out = run_libra_command_with_stdin(
        &["commit", "-F", "-"],
        root,
        "stdin subject line\n\nbody paragraph\n",
    );
    super::assert_cli_success(&out, "commit -F -");
    let log = run_libra_command(&["log", "-1", "--format=%B"], root);
    super::assert_cli_success(&log, "log after stdin commit");
    let text = String::from_utf8_lossy(&log.stdout);
    assert!(
        text.contains("stdin subject line") && text.contains("body paragraph"),
        "the whole stdin message must be used: {text}"
    );
}

/// An explicit `-S` forces signing even when commit.gpgSign disables the default.
#[test]
fn commit_gpg_sign_flag_is_accepted() {
    let repo = commit_fixture();
    let root = repo.path();
    super::assert_cli_success(
        &run_libra_command(&["config", "commit.gpgSign", "false"], root),
        "disable default signing",
    );
    for (index, flag) in ["-S", "--gpg-sign"].iter().enumerate() {
        // Restage before each iteration: the previous commit consumed the
        // staged change.
        let file = format!("signed{index}.txt");
        fs::write(root.join(&file), "s\n").expect("write staged file");
        super::assert_cli_success(
            &run_libra_command(&["add", &file], root),
            "stage the fixture",
        );
        let out = run_libra_command(&["commit", flag, "-m", "signed"], root);
        super::assert_cli_success(&out, &format!("commit {flag} -m signed"));
        let log = run_libra_command(&["log", "-1", "--format=%s"], root);
        assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "signed");
        let raw = run_libra_command(&["cat-file", "-p", "HEAD"], root);
        assert!(
            String::from_utf8_lossy(&raw.stdout).contains("gpgsig"),
            "{flag} must force signing despite commit.gpgSign=false"
        );
    }
    // The key-id form is accepted (and ignored: one vault key per repository).
    fs::write(root.join("keyed.txt"), "k\n").expect("write keyed file");
    super::assert_cli_success(
        &run_libra_command(&["add", "keyed.txt"], root),
        "stage keyed",
    );
    let keyed = run_libra_command(&["commit", "-S=DEADBEEF", "-m", "keyed"], root);
    super::assert_cli_success(&keyed, "commit -S=<keyid>");
    let raw = run_libra_command(&["cat-file", "-p", "HEAD"], root);
    assert!(String::from_utf8_lossy(&raw.stdout).contains("gpgsig"));

    for (index, (first, last, signed)) in [
        ("-S", "--no-gpg-sign", false),
        ("--no-gpg-sign", "-S", true),
    ]
    .into_iter()
    .enumerate()
    {
        let file = format!("toggle{index}.txt");
        fs::write(root.join(&file), "toggle\n").expect("write toggle file");
        super::assert_cli_success(&run_libra_command(&["add", &file], root), "stage toggle");
        super::assert_cli_success(
            &run_libra_command(&["commit", first, last, "-m", "toggle"], root),
            "commit with signing toggles",
        );
        let raw = run_libra_command(&["cat-file", "-p", "HEAD"], root);
        assert_eq!(
            String::from_utf8_lossy(&raw.stdout).contains("gpgsig"),
            signed,
            "last signing toggle must win: {first} {last}"
        );
    }
}

#[test]
fn commit_gpg_sign_without_vault_key_fails_closed() {
    let repo = tempdir().expect("tempdir");
    let root = repo.path();
    super::assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], root),
        "init without vault",
    );
    super::configure_identity_via_cli(root);
    fs::write(root.join("unsigned.txt"), "x\n").expect("write file");
    super::assert_cli_success(&run_libra_command(&["add", "unsigned.txt"], root), "stage");
    let index_before = fs::read(root.join(".libra/index")).expect("read index");
    let out = run_libra_command(&["commit", "-S", "-m", "must sign"], root);
    assert_eq!(out.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no unseal key found"),
        "missing key must be reported: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let head = run_libra_command(&["rev-parse", "--verify", "HEAD"], root);
    assert!(
        !head.status.success(),
        "failed signing must not create HEAD"
    );
    assert_eq!(index_before, fs::read(root.join(".libra/index")).unwrap());
}

/// `--short` renders the status preview and never creates a commit.
#[test]
fn commit_short_summary_flag() {
    let repo = commit_fixture();
    let root = repo.path();
    fs::write(root.join("b.txt"), "b\n").expect("write b");
    super::assert_cli_success(&run_libra_command(&["add", "b.txt"], root), "stage b");
    let head_before = run_libra_command(&["rev-parse", "--verify", "HEAD"], root);
    let index_before = fs::read(root.join(".libra/index")).expect("read index");

    let dry = run_libra_command(&["commit", "--short", "--dry-run"], root);
    super::assert_cli_success(&dry, "commit --short --dry-run");
    assert!(
        String::from_utf8_lossy(&dry.stdout).contains("A  b.txt"),
        "the short summary lists staged paths: {}",
        String::from_utf8_lossy(&dry.stdout)
    );

    let preview = run_libra_command(&["commit", "--short", "-m", "short"], root);
    super::assert_cli_success(&preview, "commit --short -m");
    assert!(
        String::from_utf8_lossy(&preview.stdout).contains("A  b.txt"),
        "--short prints the preview: {}",
        String::from_utf8_lossy(&preview.stdout)
    );
    let head_after = run_libra_command(&["rev-parse", "--verify", "HEAD"], root);
    assert_eq!(
        head_before.stdout, head_after.stdout,
        "HEAD must not change"
    );
    assert_eq!(index_before, fs::read(root.join(".libra/index")).unwrap());
}

/// `-u[=<mode>]` validates the mode and controls untracked preview paths.
#[test]
fn commit_untracked_files_flag_is_accepted() {
    let repo = commit_fixture();
    let root = repo.path();
    fs::create_dir(root.join("untracked")).expect("create untracked dir");
    fs::write(root.join("untracked/one.txt"), "1\n").expect("write first untracked");
    fs::write(root.join("untracked/two.txt"), "2\n").expect("write second untracked");
    for (flag, contains, excludes) in [
        ("-uno", "A  a.txt", "??"),
        (
            "--untracked-files=normal",
            "?? untracked/",
            "?? untracked/one.txt",
        ),
        ("-uall", "?? untracked/one.txt", "?? untracked/"),
        ("-u", "?? untracked/two.txt", "?? untracked/"),
    ] {
        let out = run_libra_command(&["commit", "--porcelain", flag], root);
        super::assert_cli_success(&out, &format!("commit --porcelain {flag}"));
        let text = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines.contains(&contains),
            "{flag}: missing {contains}: {text}"
        );
        if excludes == "??" {
            assert!(
                !lines.iter().any(|line| line.starts_with("?? ")),
                "{flag}: unexpected untracked path: {text}"
            );
        } else {
            assert!(
                !lines.contains(&excludes),
                "{flag}: unexpected {excludes}: {text}"
            );
        }
    }
    let invalid = run_libra_command(&["commit", "--porcelain", "-ubogus"], root);
    assert!(!invalid.status.success(), "invalid mode must be rejected");
}

/// Partial-commit options are classified with `LBR-UNSUPPORTED-001` instead of
/// an unknown-option error, and nothing is committed.
#[test]
fn commit_partial_commit_options_are_classified() {
    let repo = commit_fixture();
    let root = repo.path();
    let before = run_libra_command(&["rev-parse", "--verify", "HEAD"], root);
    let index_before = fs::read(root.join(".libra/index")).expect("read staged index");

    for args in [
        vec!["commit", "--only", "-m", "x"],
        vec!["commit", "-o", "a.txt", "-m", "x"],
        vec!["commit", "--pathspec-from-file", "list", "-m", "x"],
        vec!["commit", "a.txt", "-m", "x"],
    ] {
        let out = run_libra_command(&args, root);
        assert_eq!(
            out.status.code(),
            Some(128),
            "{args:?} must be classified: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("LBR-UNSUPPORTED-001") && stderr.contains("partial commit"),
            "{args:?} must name the missing feature: {stderr}"
        );
    }
    let after = run_libra_command(&["rev-parse", "--verify", "HEAD"], root);
    assert_eq!(
        before.stdout, after.stdout,
        "a refused partial commit must not create a commit"
    );
    assert_eq!(index_before, fs::read(root.join(".libra/index")).unwrap());
}

#[test]
fn i1062_consumer_commit_all_does_not_create_representation_only_commit() {
    use super::{
        add_test::{i1062_ok, i1062_run},
        status_test::i1062_committed_scripts,
    };
    let temp = i1062_committed_scripts("sha1");
    let repo = temp.path().join("repo");
    let before = i1062_ok(&repo, &["rev-parse", "HEAD"]);
    let output = i1062_run(
        &repo,
        &["commit", "-a", "--no-verify", "-m", "no content change"],
    );
    assert!(
        !output.status.success(),
        "empty commit must still be rejected: {output:?}"
    );
    assert_eq!(i1062_ok(&repo, &["rev-parse", "HEAD"]), before);
    assert_eq!(
        std::fs::read(repo.join("probe.ps1")).unwrap(),
        b"one\r\ntwo\r\n"
    );
    std::fs::write(repo.join("probe.ps1"), b"real change\r\n").unwrap();
    i1062_ok(
        &repo,
        &["commit", "-a", "--no-verify", "-m", "real content change"],
    );
    assert_ne!(i1062_ok(&repo, &["rev-parse", "HEAD"]), before);
}
