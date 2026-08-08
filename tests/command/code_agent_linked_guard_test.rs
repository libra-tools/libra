//! W0 §C.4.1.1 (plan-20260714): linked-worktree guards for the Code/Agent
//! runtime surfaces, in force until the unified config resolver lands
//! (plan-20260715 W4-06..W4-08).
//!
//! In a linked worktree the runtimes would read their configuration from the
//! LOCAL gitdir (where no repository configuration lives) while sandbox policy
//! reads COMMON storage — a security brain-split. `libra code` and
//! `libra automation` fail closed; automation VCS dispatch is disabled with
//! exactly one user-visible warning per command (silent non-execution is
//! expressly forbidden by §C.4.1.1).
//!
//! Layer: L1 (deterministic; tempdir, no network).

use std::fs;

use super::{assert_cli_success, run_libra_command};

/// A committed repo plus one linked worktree. Returns (repo_dir, wt_path).
fn repo_with_linked_worktree() -> (tempfile::TempDir, tempfile::TempDir) {
    let repo = tempfile::tempdir().expect("repo");
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["init", "--vault=false"], p), "init");
    assert_cli_success(&run_libra_command(&["config", "user.name", "t"], p), "name");
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "t@t"], p),
        "email",
    );
    fs::write(p.join("a.txt"), "a\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );
    let parent = tempfile::tempdir().expect("wt parent");
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], p),
        "worktree add",
    );
    (repo, parent)
}

/// `libra code` refuses to launch from a linked worktree BEFORE mode
/// validation — the same invalid flag combination that yields a usage error
/// in main yields the W0 guard error in a linked worktree, proving the
/// preflight runs first (and keeping this test hang-free in both arms: no
/// server ever starts).
#[test]
fn code_refuses_linked_worktree_before_mode_validation() {
    let (repo, parent) = repo_with_linked_worktree();
    let wt = parent.path().join("wt");
    let invalid_modes = ["code", "--port", "5000", "--mcp-port", "5000"];

    let linked = run_libra_command(&invalid_modes, &wt);
    assert!(
        !linked.status.success(),
        "libra code must refuse in a linked worktree"
    );
    let linked_stderr = String::from_utf8_lossy(&linked.stderr);
    assert!(
        linked_stderr.contains("cannot run in a linked worktree"),
        "linked refusal must be the W0 preflight, got: {linked_stderr}"
    );
    assert!(
        linked_stderr.contains("MAIN worktree"),
        "the refusal must tell the user where to run instead: {linked_stderr}"
    );

    let main = run_libra_command(&invalid_modes, repo.path());
    assert!(
        !main.status.success(),
        "invalid modes still refused in main"
    );
    let main_stderr = String::from_utf8_lossy(&main.stderr);
    assert!(
        main_stderr.contains("must be different"),
        "main must reach mode validation (NOT the linked guard): {main_stderr}"
    );
    assert!(
        !main_stderr.contains("cannot run in a linked worktree"),
        "the guard must not fire in the main worktree: {main_stderr}"
    );
}

/// The guard gates the SESSION's working directory, not the process cwd:
/// `libra code --cwd <linked-wt>` invoked FROM MAIN must still refuse (the
/// original guard checked only the ambient cwd, so this exact invocation
/// started the split-brained session it was meant to prevent), while
/// `--cwd <main>` invoked from inside the linked worktree must pass the
/// guard and reach mode validation.
#[test]
fn code_guard_follows_cwd_flag_not_process_cwd() {
    let (repo, parent) = repo_with_linked_worktree();
    let wt = parent.path().join("wt");
    let wt_str = wt.to_str().unwrap();
    let main_str = repo.path().to_str().unwrap();

    let bypass_attempt = run_libra_command(
        &[
            "code",
            "--cwd",
            wt_str,
            "--port",
            "5000",
            "--mcp-port",
            "5000",
        ],
        repo.path(),
    );
    assert!(
        !bypass_attempt.status.success(),
        "libra code --cwd <linked-wt> must refuse even from main"
    );
    let stderr = String::from_utf8_lossy(&bypass_attempt.stderr);
    assert!(
        stderr.contains("cannot run in a linked worktree"),
        "--cwd into a linked worktree must hit the W0 preflight: {stderr}"
    );

    let inverse = run_libra_command(
        &[
            "code",
            "--cwd",
            main_str,
            "--port",
            "5000",
            "--mcp-port",
            "5000",
        ],
        &wt,
    );
    assert!(!inverse.status.success(), "invalid modes still refused");
    let inverse_stderr = String::from_utf8_lossy(&inverse.stderr);
    assert!(
        inverse_stderr.contains("must be different"),
        "--cwd back to main from a linked cwd passes the guard and reaches \
         mode validation: {inverse_stderr}"
    );
}

/// Every `libra automation` subcommand fails closed in a linked worktree —
/// running there would silently see an EMPTY rule set instead of the
/// repository's automations. Main keeps working.
#[test]
fn automation_cli_fails_closed_in_linked_worktree() {
    let (repo, parent) = repo_with_linked_worktree();
    let wt = parent.path().join("wt");

    for subcommand in [["automation", "list"], ["automation", "history"]] {
        let linked = run_libra_command(&subcommand, &wt);
        assert!(
            !linked.status.success(),
            "libra {} must refuse in a linked worktree",
            subcommand.join(" ")
        );
        let stderr = String::from_utf8_lossy(&linked.stderr);
        assert!(
            stderr.contains("cannot run in a linked worktree"),
            "refusal must be the W0 preflight for {}: {stderr}",
            subcommand.join(" ")
        );
    }

    // Main worktree: same subcommand succeeds (no rules configured is fine).
    let main = run_libra_command(&["automation", "list"], repo.path());
    assert_cli_success(&main, "automation list in main");
}

/// Automation VCS dispatch in a linked worktree is DISABLED with exactly one
/// warning per command — not silently skipped (§C.4.1.1 forbids silent
/// non-execution), and not repeated per event.
#[test]
fn linked_commit_warns_automation_dispatch_disabled_once() {
    let (_repo, parent) = repo_with_linked_worktree();
    let wt = parent.path().join("wt");

    fs::write(wt.join("b.txt"), "b\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.txt"], &wt), "add in wt");
    let commit = run_libra_command(&["commit", "-m", "wt commit", "--no-verify"], &wt);
    assert_cli_success(&commit, "commit in linked worktree");

    let stderr = String::from_utf8_lossy(&commit.stderr);
    let warnings = stderr
        .matches("automation dispatch is disabled in linked worktrees")
        .count();
    assert_eq!(
        warnings, 1,
        "the linked commit must warn EXACTLY once that automation dispatch \
         is disabled (0 = forbidden silent skip, >1 = per-event spam): {stderr}"
    );
}

/// The warning is a linked-scope contract only: a main-worktree commit stays
/// quiet (automations simply run — or no-op without configuration).
#[test]
fn main_commit_does_not_warn_about_automation_dispatch() {
    let (repo, _parent) = repo_with_linked_worktree();
    fs::write(repo.path().join("c.txt"), "c\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "c.txt"], repo.path()),
        "add in main",
    );
    let commit = run_libra_command(&["commit", "-m", "main commit", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit in main");
    let stderr = String::from_utf8_lossy(&commit.stderr);
    assert!(
        !stderr.contains("automation dispatch is disabled"),
        "main-worktree commits must not carry the linked-scope warning: {stderr}"
    );
}
