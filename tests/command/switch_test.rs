//! Tests switch command for branch creation, switching, and dirty-state checks.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use git_internal::internal::index::Index;
use libra::{
    internal::{
        branch::{Branch as InternalBranch, TRACES_BRANCH},
        head::Head,
    },
    utils::{client_storage::ClientStorage, path, test::ChangeDirGuard},
};

use super::*;

#[test]
fn test_switch_cli_missing_branch_returns_cli_exit_code() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["switch", "no-such"], repo.path());

    assert_eq!(output.status.code(), Some(129));
    assert!(String::from_utf8_lossy(&output.stderr).contains("branch 'no-such' not found"));
}

/// opencode.md OC-Phase 3 acceptance criterion 5 requires that
/// `switch` refuse to create a branch named `intent` or
/// `traces`. The runtime guard at
/// `src/command/switch.rs::is_locked_branch` covers both, but the
/// `switch_test` suite previously had no coverage at all for the
/// locked-name refusal — a regression that dropped the guard could
/// have shipped silently.
#[test]
fn test_switch_create_intent_branch_is_blocked() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["switch", "-c", "intent"], repo.path());

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("intent"),
        "expected the locked branch name in the message, got: {stderr}"
    );
}

/// Companion to `test_switch_create_intent_branch_is_blocked` for the
/// `traces` locked name. Without the guard, `switch -c
/// traces` could shadow the reserved capture ref locally and
/// then propagate via `push`.
#[test]
fn test_switch_create_agent_traces_branch_is_blocked() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["switch", "-c", "traces"], repo.path());

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("traces"),
        "expected the traces branch name in the message, got: {stderr}"
    );
}

#[tokio::test]
#[serial]
async fn test_switch_existing_agent_traces_branch_is_blocked() {
    let repo = create_committed_repo_via_cli();
    {
        let _guard = ChangeDirGuard::new(repo.path());
        let head = Head::current_commit()
            .await
            .expect("committed repo should have HEAD");
        InternalBranch::update_branch(TRACES_BRANCH, &head.to_string(), None)
            .await
            .expect("seed traces branch");
    }

    let output = run_libra_command(&["switch", TRACES_BRANCH], repo.path());

    assert!(!output.status.success());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert!(
        stderr.contains(TRACES_BRANCH),
        "expected the traces branch name in the message, got: {stderr}"
    );
}

#[test]
fn test_switch_json_create_output_reports_new_branch() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "switch", "-c", "feature"], repo.path());
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "switch");
    assert_eq!(json["data"]["branch"], "feature");
    assert_eq!(json["data"]["created"], true);
    assert_eq!(json["data"]["detached"], false);
}

#[test]
fn test_switch_force_create_resets_existing_branch() {
    let repo = create_committed_repo_via_cli();

    // Create an old branch from HEAD and make a new commit on main
    let output = run_libra_command(&["switch", "-c", "feature"], repo.path());
    assert_cli_success(&output, "create feature should succeed");

    let output = run_libra_command(&["switch", "main"], repo.path());
    assert_cli_success(&output, "switch back to main should succeed");

    std::fs::write(repo.path().join("second.txt"), "second\n").unwrap();
    let output = run_libra_command(&["add", "second.txt"], repo.path());
    assert_cli_success(&output, "add second.txt should succeed");
    let output = run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path());
    assert_cli_success(&output, "commit second should succeed");

    let head = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    let head_hash = String::from_utf8_lossy(&head.stdout).trim().to_string();

    // -C should reset feature to current main and switch
    let output = run_libra_command(&["switch", "-C", "feature"], repo.path());
    assert_cli_success(&output, "force-create feature should succeed");

    let output = run_libra_command(&["rev-parse", "feature"], repo.path());
    let feature_hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(
        feature_hash, head_hash,
        "feature should point to current HEAD"
    );
}

#[test]
fn test_switch_force_create_refuses_current_branch() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["switch", "-c", "feature"], repo.path());
    assert_cli_success(&output, "create feature should succeed");

    let output = run_libra_command(&["switch", "-C", "feature"], repo.path());
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot force-create the currently checked-out branch"),
        "got: {stderr}"
    );
}

#[test]
fn test_switch_orphan_keeps_head_unborn_until_first_commit() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["switch", "--orphan", "fresh"], repo.path());
    assert_cli_success(&output, "create orphan branch should succeed");

    let output = run_libra_command(&["rev-parse", "--symbolic-full-name", "HEAD"], repo.path());
    assert_cli_success(&output, "orphan branch should become symbolic HEAD");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "refs/heads/fresh"
    );

    let output = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    assert!(
        !output.status.success(),
        "orphan HEAD should be unborn before the first user commit"
    );

    let output = run_libra_command(&["commit", "-m", "fresh root", "--no-verify"], repo.path());
    assert_cli_success(&output, "first orphan commit should succeed");

    let output = run_libra_command(&["log", "--pretty=%P", "-1"], repo.path());
    assert_cli_success(&output, "parent list should be printable");
    assert!(
        String::from_utf8_lossy(&output.stdout).trim().is_empty(),
        "first orphan commit should have no parents"
    );
}

#[tokio::test]
#[serial]
async fn test_switch_json_track_output_stays_clean() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    let head = Head::current_commit().await.unwrap();
    let output = run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );
    assert_cli_success(&output, "add origin remote for track test");

    Branch::update_branch(
        "refs/remotes/origin/feature",
        &head.to_string(),
        Some("origin"),
    )
    .await
    .unwrap();

    let output = run_libra_command(
        &["--json", "switch", "--track", "origin/feature"],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "switch");
    assert_eq!(json["data"]["branch"], "feature");
    assert_eq!(json["data"]["tracking"]["remote"], "origin");
    assert_eq!(json["data"]["tracking"]["remote_branch"], "feature");
}

#[tokio::test]
#[serial]
async fn test_switch_track_human_output_keeps_tracking_message() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    let head = Head::current_commit().await.unwrap();
    let output = run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );
    assert_cli_success(&output, "add origin remote for track test");

    Branch::update_branch(
        "refs/remotes/origin/feature",
        &head.to_string(),
        Some("origin"),
    )
    .await
    .unwrap();

    let output = run_libra_command(&["switch", "--track", "origin/feature"], repo.path());
    assert_cli_success(&output, "switch --track");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Branch 'feature' set up to track remote branch 'origin/feature'"),
        "expected upstream tracking message in stdout, got: {stdout}"
    );
}

/// Default-on DWIM guess: `libra switch <name>` with no local branch but a
/// unique remote-tracking branch creates a local tracking branch and switches.
#[tokio::test]
#[serial]
async fn test_switch_guess_creates_tracking_branch() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    let head = Head::current_commit().await.unwrap();
    let output = run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );
    assert_cli_success(&output, "add origin remote for guess test");

    Branch::update_branch(
        "refs/remotes/origin/guessed",
        &head.to_string(),
        Some("origin"),
    )
    .await
    .unwrap();

    // No --guess flag: guessing is enabled by default (Git parity).
    let output = run_libra_command(&["switch", "guessed"], repo.path());
    assert_cli_success(&output, "switch guessed (default guess)");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Branch 'guessed' set up to track remote branch 'origin/guessed'"),
        "expected upstream tracking message in stdout, got: {stdout}"
    );

    let output = run_libra_command(&["branch", "--show-current"], repo.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("guessed"),
        "expected to be on the guessed branch, got: {stdout}"
    );
}

/// `--no-guess` disables the DWIM behaviour: a remote-only name is rejected as
/// not-found instead of being auto-created.
#[tokio::test]
#[serial]
async fn test_switch_no_guess_rejects_remote_only_branch() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    let head = Head::current_commit().await.unwrap();
    let output = run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );
    assert_cli_success(&output, "add origin remote for no-guess test");

    Branch::update_branch(
        "refs/remotes/origin/guessed",
        &head.to_string(),
        Some("origin"),
    )
    .await
    .unwrap();

    let output = run_libra_command(&["switch", "--no-guess", "guessed"], repo.path());
    assert_eq!(output.status.code(), Some(129));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("branch 'guessed' not found"),
        "expected not-found error with --no-guess, got: {stderr}"
    );

    // The guess must not have created a local branch as a side effect.
    let output = run_libra_command(&["branch", "--list"], repo.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("guessed"),
        "no local 'guessed' branch should exist after --no-guess, got: {stdout}"
    );
}

/// `checkout.guess=false` disables guessing when neither flag is supplied; an
/// explicit `--guess` still overrides the config back on.
#[tokio::test]
#[serial]
async fn test_switch_guess_config_disables_then_flag_overrides() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    let head = Head::current_commit().await.unwrap();
    let output = run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );
    assert_cli_success(&output, "add origin remote for guess-config test");

    Branch::update_branch(
        "refs/remotes/origin/guessed",
        &head.to_string(),
        Some("origin"),
    )
    .await
    .unwrap();

    let output = run_libra_command(&["config", "set", "checkout.guess", "false"], repo.path());
    assert_cli_success(&output, "set checkout.guess=false");

    // Config off, no flag: guessing disabled -> not found.
    let output = run_libra_command(&["switch", "guessed"], repo.path());
    assert_eq!(output.status.code(), Some(129));

    // Explicit --guess overrides the config.
    let output = run_libra_command(&["switch", "--guess", "guessed"], repo.path());
    assert_cli_success(&output, "switch --guess overrides checkout.guess=false");
    let output = run_libra_command(&["branch", "--show-current"], repo.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("guessed"),
        "expected to be on guessed after explicit --guess, got: {stdout}"
    );
}

/// When several remotes carry the guessed name the switch is blocked as
/// ambiguous, but `checkout.defaultRemote` breaks the tie.
#[tokio::test]
#[serial]
async fn test_switch_guess_ambiguous_resolved_by_default_remote() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    let head = Head::current_commit().await.unwrap();
    for remote in ["origin", "upstream"] {
        let output = run_libra_command(
            &[
                "remote",
                "add",
                remote,
                &format!("https://example.com/{remote}.git"),
            ],
            repo.path(),
        );
        assert_cli_success(&output, "add remote for ambiguity test");
        Branch::update_branch(
            &format!("refs/remotes/{remote}/feature"),
            &head.to_string(),
            Some(remote),
        )
        .await
        .unwrap();
    }

    // Two remotes match -> ambiguous, exit 128, nothing created.
    let output = run_libra_command(&["switch", "feature"], repo.path());
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("matched multiple remote-tracking branches"),
        "expected ambiguity error, got: {stderr}"
    );
    let output = run_libra_command(&["branch", "--list"], repo.path());
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("feature"),
        "ambiguous guess must not create a local branch"
    );

    // checkout.defaultRemote breaks the tie.
    let output = run_libra_command(
        &["config", "set", "checkout.defaultRemote", "upstream"],
        repo.path(),
    );
    assert_cli_success(&output, "set checkout.defaultRemote=upstream");

    let output = run_libra_command(&["switch", "feature"], repo.path());
    assert_cli_success(&output, "switch feature resolved by defaultRemote");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Branch 'feature' set up to track remote branch 'upstream/feature'"),
        "expected upstream tracking via defaultRemote, got: {stdout}"
    );
}

// async fn test_check_status() {
//     println!("\n\x1b[1mTest check_status function.\x1b[0m");
//
//     // Test the check_status
//     // Expect false when no changes
//     assert!(!check_status().await);
//
//     // Create a file and add it to the index
//     // Expect true when there are unstaged changes
//     fs::File::create("foo.txt").unwrap();
//     let add_args = add::AddArgs {
//         pathspec: vec!["foo.txt".to_string()],
//         all: false,
//         update: false,
//         verbose: true,
//         dry_run: false,
//         ignore_errors: false,
//         refresh: false,
//     };
//     add::execute(add_args).await;
//     assert!(check_status().await);
//
//     // Modify a file
//     // Expect true when there are uncommitted changes
//     fs::write("foo.txt", "modified content").unwrap();
//     assert!(check_status().await);
// }

async fn test_switch_function() {
    println!("\n\x1b[1mTest switch function.\x1b[0m");

    // create first empty commit
    {
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
    }

    // create a new branch and switch to it
    {
        let args = SwitchArgs {
            no_progress: false,
            branch: None,
            create: Some("test_branch".to_string()),
            force_create: None,
            orphan: None,
            detach: false,
            track: false,
            force: false,
            guess: false,
            no_guess: false,
        };
        switch::execute(args).await;
        let head = Head::current().await;
        let ref_name = match head {
            Head::Branch(name) => name,
            _ => panic!("head not in branch,unreachable"),
            // Head::Detached(name) => name.to_string(),
        };
        assert_eq!(
            ref_name, "test_branch",
            "create a new branch and switch to it failed!"
        );
    }

    //detach the head to a commit
    {
        let head = Head::current().await;
        let ref_name = match head {
            Head::Branch(name) => name,
            _ => panic!("head not in branch,unreachable"),
            // Head::Detached(name) => name.to_string(),
        };
        // Migrated from lossy `Branch::find_branch` per docs/development/commands/branch.md.
        let branch = Branch::find_branch_result(&ref_name, None)
            .await
            .expect("failed to query current branch")
            .expect("current branch should exist");
        let commit: Commit = load_object(&branch.commit).unwrap();
        let commit_id_str = commit.id.to_string();

        let args = CommitArgs {
            message: Some("second".to_string()),
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

        let args = SwitchArgs {
            no_progress: false,
            branch: Some(commit_id_str.clone()),
            create: None,
            force_create: None,
            orphan: None,
            detach: true,
            track: false,
            force: false,
            guess: false,
            no_guess: false,
        };
        switch::execute(args).await;
        let head = Head::current().await;
        let ref_name = match head {
            Head::Detached(name) => name.to_string(),
            _ => panic!("head not detached,unreachable"),
            // Head::Detached(name) => name.to_string(),
        };
        println!("detach {ref_name:?}");
        assert_eq!(
            ref_name, commit_id_str,
            "detach the head to a commit failed!"
        );
    }

    //switch branch back to the master
    {
        let args = SwitchArgs {
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
        };
        switch::execute(args).await;
        let head = Head::current().await;
        let ref_name = match head {
            Head::Branch(name) => name,
            _ => panic!("head not in branch,unreachable"),
            // Head::Detached(name) => name.to_string(),
        };
        assert_eq!(ref_name, "main", "switch back to the master failed!");
    }
}
#[tokio::test]
#[serial]
/// Tests the core functionality of the switch command module.
/// Validates branch switching operations and working directory status checks.
async fn test_parts_of_switch_module_function() {
    println!("\n\x1b[1mTest some functions of the switch module.\x1b[0m");
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    println!("temp_path {temp_path:?}");

    //Test check the branch
    test_switch_function().await;

    // Test the switch module funsctions
    // test_check_status().await;
}

#[test]
fn test_switch_current_branch_with_dirty_worktree_is_noop() {
    let repo = create_committed_repo_via_cli();
    std::fs::write(repo.path().join("tracked.txt"), "modified content\n").unwrap();

    let output = run_libra_command(&["switch", "main"], repo.path());
    assert_cli_success(&output, "switch current branch");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Already on 'main'"),
        "switch current branch should remain a no-op, got: {stdout}"
    );
    assert!(
        !stdout.contains("Changes not staged") && !stdout.contains("On branch"),
        "switch current branch should not print a status summary, got: {stdout}"
    );
    let content = std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap();
    assert_eq!(content, "modified content\n");
}

#[test]
fn test_switch_create_branch_from_valid_commit() {
    let repo = create_committed_repo_via_cli();

    std::fs::write(repo.path().join("tracked.txt"), "tracked second\n").unwrap();
    let add = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert_cli_success(&add, "add tracked.txt");
    let commit = run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit second");

    let output = run_libra_command(&["switch", "-c", "feature-from-base", "HEAD^"], repo.path());
    assert_cli_success(&output, "switch -c feature-from-base HEAD^");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Switched to a new branch 'feature-from-base'"),
        "expected branch creation message, got: {stdout}"
    );

    let log_output = run_libra_command(&["log", "--oneline", "-1"], repo.path());
    assert_cli_success(&log_output, "log -1 after switch");
    let log_stdout = String::from_utf8_lossy(&log_output.stdout);
    assert!(
        log_stdout.contains("base"),
        "expected new branch to point at the requested base commit, got: {log_stdout}"
    );

    let content = std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap();
    assert_eq!(content, "tracked\n");
}

#[tokio::test]
#[serial]
async fn test_switch_track_sets_upstream() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());

    let args = CommitArgs {
        message: Some("base".to_string()),
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

    let output = run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        temp_path.path(),
    );
    assert_cli_success(&output, "add origin remote for track test");

    let master_commit = Head::current_commit().await.unwrap();
    Branch::update_branch(
        "refs/remotes/origin/feature",
        &master_commit.to_string(),
        Some("origin"),
    )
    .await
    .unwrap();

    let args = SwitchArgs {
        no_progress: false,
        branch: Some("origin/feature".to_string()),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: true,
        force: false,
        guess: false,
        no_guess: false,
    };
    switch::execute(args).await;

    let head = Head::current().await;
    let branch_name = match head {
        Head::Branch(name) => name,
        _ => panic!("head not in branch, unreachable"),
    };
    assert_eq!(branch_name, "feature");

    let branch_config = libra::internal::config::ConfigKv::branch_config("feature")
        .await
        .ok()
        .flatten()
        .unwrap();
    assert_eq!(branch_config.remote, "origin");
    assert_eq!(branch_config.merge, "feature");
}

#[tokio::test]
#[serial]
/// Tests basic HEAD detachment capabilities with simple reference paths.
/// Validates relative references (HEAD^, HEAD~), numeric references (HEAD^1, HEAD~1),
/// and complex reference combinations (HEAD^^^, HEAD~~~, HEAD^~^~).
async fn test_detach_head_basic() {
    println!("\n\x1b[1mTest detach use the head's ref basically.\x1b[0m");
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    println!("temp_path {temp_path:?}");

    for i in 0..6 {
        let args = CommitArgs {
            message: Some(format!("commit_{i}")),
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
    }
    //detach to head
    {
        switch_to_branch("main".to_string()).await;

        let commit_message = switch_to_detach("HEAD".to_string()).await;
        assert_eq!(&commit_message, "commit_5");
    }

    //detach to the before commit
    {
        let commit_message = switch_to_detach("HEAD^".to_string()).await;
        assert_eq!(&commit_message, "commit_4");
    }

    {
        let commit_message = switch_to_detach("HEAD~".to_string()).await;
        assert_eq!(&commit_message, "commit_3");
    }
    {
        let commit_message = switch_to_detach("HEAD^1".to_string()).await;
        assert_eq!(&commit_message, "commit_2");
    }

    {
        let commit_message = switch_to_detach("HEAD~1".to_string()).await;
        assert_eq!(&commit_message, "commit_1");
    }
    switch_to_branch("main".to_string()).await;

    for i in 6..12 {
        let args = CommitArgs {
            message: Some(format!("commit_{i}")),
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
    }

    //detach use head's ref
    {
        switch_to_branch("main".to_string()).await;
        let commit_message = switch_to_detach("HEAD~11".to_string()).await;
        assert_eq!(&commit_message, "commit_0");
    }
    {
        switch_to_branch("main".to_string()).await;
        let commit_message = switch_to_detach("HEAD~~~~~~~~~~~".to_string()).await;
        assert_eq!(&commit_message, "commit_0");
    }
    {
        switch_to_branch("main".to_string()).await;
        let commit_message = switch_to_detach("HEAD^^^^^^^^^^^".to_string()).await;
        assert_eq!(&commit_message, "commit_0");
    }

    {
        switch_to_branch("main".to_string()).await;
        let commit_message = switch_to_detach("HEAD^~^~^~^~^~^".to_string()).await;
        assert_eq!(&commit_message, "commit_0");
    }
    //detach use branch's ref
    {
        switch_to_branch("main".to_string()).await;
        let commit_message = switch_to_detach("main~11".to_string()).await;
        assert_eq!(&commit_message, "commit_0");
    }
    {
        switch_to_branch("main".to_string()).await;
        let commit_message = switch_to_detach("main~~~~~~~~~~~".to_string()).await;
        assert_eq!(&commit_message, "commit_0");
    }
    {
        switch_to_branch("main".to_string()).await;
        let commit_message = switch_to_detach("main^^^^^^^^^^^".to_string()).await;
        assert_eq!(&commit_message, "commit_0");
    }

    {
        switch_to_branch("main".to_string()).await;
        let commit_message = switch_to_detach("main^~^~^~^~^~^".to_string()).await;
        assert_eq!(&commit_message, "commit_0");
    }
    // Migrated from lossy `Branch::find_branch` per docs/development/commands/branch.md.
    let master_commit_id = Branch::find_branch_result("main", None)
        .await
        .expect("failed to query main branch")
        .expect("main branch should exist")
        .commit;
    //detach use commit's ref
    {
        switch_to_branch("main".to_string()).await;
        let commit_message = switch_to_detach(format!("{master_commit_id}~11")).await;
        assert_eq!(&commit_message, "commit_0");
    }
    {
        switch_to_branch("main".to_string()).await;
        let commit_message = switch_to_detach(format!("{master_commit_id}~11")).await;
        assert_eq!(&commit_message, "commit_0");
    }
    {
        switch_to_branch("main".to_string()).await;
        let commit_message = switch_to_detach(format!("{master_commit_id}~~~~~~~~~~~")).await;
        assert_eq!(&commit_message, "commit_0");
    }
    {
        switch_to_branch("main".to_string()).await;
        let commit_message = switch_to_detach(format!("{master_commit_id}^^^^^^^^^^^")).await;
        assert_eq!(&commit_message, "commit_0");
    }

    {
        switch_to_branch("main".to_string()).await;
        let commit_message = switch_to_detach(format!("{master_commit_id}^~^~^~^~^~^")).await;
        assert_eq!(&commit_message, "commit_0");
    }
}

// a tree with many parents.
async fn create_commit_tree() {
    let index = Index::load(path::index()).unwrap();
    let storage = ClientStorage::init(path::objects());

    let tree = commit::create_tree(&index, &storage, "".into())
        .await
        .unwrap();

    let mut commit_1 = Commit::from_tree_id(tree.id, vec![], &format_commit_msg("commit_0", None));
    commit_1.committer.timestamp = 1;
    save_object(&commit_1, &commit_1.id).unwrap();

    let mut parents_ids = vec![];
    for i in 1..12 {
        let tree = commit::create_tree(&index, &storage, "".into())
            .await
            .unwrap();

        let mut commit = Commit::from_tree_id(
            tree.id,
            vec![commit_1.id],
            &format_commit_msg(&format!("commit_{i}"), None),
        );
        commit.committer.timestamp = (i + 1) as usize;
        save_object(&commit, &commit.id).unwrap();
        parents_ids.push(commit.id);
    }
    {
        let tree = commit::create_tree(&index, &storage, "".into())
            .await
            .unwrap();

        let mut commit_last = Commit::from_tree_id(
            tree.id,
            parents_ids,
            &format_commit_msg("commit_last", None),
        );
        commit_last.committer.timestamp = 100;
        save_object(&commit_last, &commit_last.id).unwrap();
        Branch::update_branch("main", &commit_last.id.to_string(), None)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[serial]
// Comprehensive tests for HEAD reference navigation using Git-style paths
// Validates support for ^ (parent selection), ~ (ancestry traversal), and their combinations
async fn test_detach_head_extra() {
    println!("\n\x1b[1mTest detach use the head's ref extra.\x1b[0m");
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = ChangeDirGuard::new(temp_path.path());
    println!("temp_path {temp_path:?}");

    create_commit_tree().await;
    //detach to head
    {
        let commit_message = switch_to_detach("HEAD".to_string()).await;
        assert_eq!(commit_message, "commit_last".to_string());
    }

    for i in 1..12 {
        let commit_message = switch_to_detach(format!("HEAD^{i}")).await;
        assert_eq!(commit_message, format!("commit_{i}"));

        //back to the last commit
        switch_to_branch("main".to_string()).await;
    }
    //detach use the branch's ref
    for i in 1..12 {
        let commit_message = switch_to_detach(format!("main^{i}")).await;
        assert_eq!(commit_message, format!("commit_{i}"));

        //back to the last commit
        switch_to_branch("main".to_string()).await;
    }
    //detach use head's ref
    {
        let commit_message = switch_to_detach("HEAD^11~".to_string()).await;
        assert_eq!(commit_message, "commit_0".to_string());
        switch_to_branch("main".to_string()).await;
    }
    //detach use branch's ref
    {
        let commit_message = switch_to_detach("main^11~".to_string()).await;
        assert_eq!(commit_message, "commit_0".to_string());
        switch_to_branch("main".to_string()).await;
    }
    // Migrated from lossy `Branch::find_branch` per docs/development/commands/branch.md.
    let master_commit_id = Branch::find_branch_result("main", None)
        .await
        .expect("failed to query main branch")
        .expect("main branch should exist")
        .commit;
    //detach use commit's ref
    {
        let commit_message = switch_to_detach(format!("{master_commit_id}^11~")).await;
        assert_eq!(commit_message, "commit_0".to_string());
        switch_to_branch("main".to_string()).await;
    }
}

async fn switch_to_detach(branch_test: String) -> String {
    let args = SwitchArgs {
        no_progress: false,
        branch: Some(branch_test),
        create: None,
        force_create: None,
        orphan: None,
        detach: true,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    };
    switch::execute(args).await;
    let head = Head::current().await;
    let commit_id = match head {
        Head::Detached(commit) => commit,
        _ => panic!("head not detached,unreachable"),
    };
    let commit = load_object::<Commit>(&commit_id).unwrap();
    commit.message.trim().to_string()
}

async fn switch_to_branch(branch_test: String) {
    let args = SwitchArgs {
        no_progress: false,
        branch: Some(branch_test),
        create: None,
        force_create: None,
        orphan: None,
        detach: false,
        track: false,
        force: false,
        guess: false,
        no_guess: false,
    };
    switch::execute(args).await;
}

#[test]
#[serial]
fn test_switch_force_discards_local_changes() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    std::fs::write(p.join("tracked.txt"), "v1\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "tracked.txt"], p), "add v1");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "v1", "--no-verify"], p),
        "commit v1",
    );

    // `other` advances tracked.txt to v2; main stays at v1.
    assert_cli_success(&run_libra_command(&["branch", "other"], p), "branch other");
    assert_cli_success(&run_libra_command(&["switch", "other"], p), "switch other");
    std::fs::write(p.join("tracked.txt"), "v2\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "tracked.txt"], p), "add v2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "v2", "--no-verify"], p),
        "commit v2",
    );
    assert_cli_success(&run_libra_command(&["switch", "main"], p), "switch main");

    // Dirty the tracked file on main.
    std::fs::write(p.join("tracked.txt"), "dirty\n").unwrap();

    // A plain switch is refused while dirty (would clobber local changes)...
    let blocked = run_libra_command(&["switch", "other"], p);
    assert!(
        !blocked.status.success(),
        "dirty switch should be refused without -f"
    );

    // ...but -f discards the local change and switches to the target.
    let forced = run_libra_command(&["switch", "-f", "other"], p);
    assert_cli_success(&forced, "switch -f other");
    assert_eq!(
        std::fs::read_to_string(p.join("tracked.txt")).unwrap(),
        "v2\n",
        "-f should restore the target branch's content"
    );
}

#[test]
fn switch_no_progress_flag_is_accepted_noop() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert!(
        run_libra_command(&["branch", "feature"], p)
            .status
            .success(),
        "create feature"
    );
    // `--no-progress` is accepted and a no-op: Libra's switch renders no progress
    // meter, so the switch proceeds normally.
    let out = run_libra_command(&["switch", "--no-progress", "feature"], p);
    assert!(
        out.status.success(),
        "switch --no-progress feature: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let current = run_libra_command(&["branch", "--show-current"], p);
    assert!(
        String::from_utf8_lossy(&current.stdout).contains("feature"),
        "switched to feature"
    );
}
