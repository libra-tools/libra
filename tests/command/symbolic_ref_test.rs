//! Integration tests for `symbolic-ref`.

use super::*;

#[test]
fn symbolic_ref_head_prints_full_ref() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["symbolic-ref", "HEAD"], repo.path());
    assert_cli_success(&output, "symbolic-ref HEAD");

    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "refs/heads/main"
    );
}

#[test]
fn symbolic_ref_short_head_prints_branch_name() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["symbolic-ref", "--short", "HEAD"], repo.path());
    assert_cli_success(&output, "symbolic-ref --short HEAD");

    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "main");
}

#[test]
fn symbolic_ref_json_reports_read_target() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["--json", "symbolic-ref", "HEAD"], repo.path());
    assert_cli_success(&output, "symbolic-ref --json HEAD");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "symbolic-ref");
    assert_eq!(json["data"]["action"], "read");
    assert_eq!(json["data"]["name"], "HEAD");
    assert_eq!(json["data"]["target"], "refs/heads/main");
    assert_eq!(json["data"]["short"], "main");
}

#[test]
fn symbolic_ref_set_head_to_existing_branch() {
    let repo = create_committed_repo_via_cli();

    let branch_output = run_libra_command(&["branch", "feature"], repo.path());
    assert_cli_success(&branch_output, "branch feature");

    let output = run_libra_command(&["symbolic-ref", "HEAD", "refs/heads/feature"], repo.path());
    assert_cli_success(&output, "symbolic-ref HEAD refs/heads/feature");
    assert!(
        output.stdout.is_empty(),
        "set form should be silent on success, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let current = run_libra_command(&["symbolic-ref", "--short", "HEAD"], repo.path());
    assert_cli_success(&current, "symbolic-ref --short HEAD after set");
    assert_eq!(String::from_utf8_lossy(&current.stdout).trim(), "feature");
}

#[test]
fn symbolic_ref_detached_head_returns_invalid_target() {
    let repo = create_committed_repo_via_cli();

    let detach = run_libra_command(&["switch", "--detach", "HEAD"], repo.path());
    assert_cli_success(&detach, "switch --detach HEAD");

    let output = run_libra_command(&["symbolic-ref", "HEAD"], repo.path());
    assert!(!output.status.success(), "detached HEAD should fail");

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        human.contains("HEAD is not a symbolic ref"),
        "unexpected stderr: {human}"
    );
    assert_eq!(report.error_code, "LBR-CLI-003");
}

#[test]
fn symbolic_ref_quiet_detached_head_exits_silently() {
    let repo = create_committed_repo_via_cli();

    let detach = run_libra_command(&["switch", "--detach", "HEAD"], repo.path());
    assert_cli_success(&detach, "switch --detach HEAD");

    let output = run_libra_command(&["symbolic-ref", "--quiet", "HEAD"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(1),
        "quiet detached HEAD should exit with status 1"
    );
    assert!(
        output.stdout.is_empty(),
        "quiet detached HEAD should not write stdout, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stderr.is_empty(),
        "quiet detached HEAD should not write stderr, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn symbolic_ref_json_quiet_detached_head_reports_structured_error() {
    let repo = create_committed_repo_via_cli();

    let detach = run_libra_command(&["switch", "--detach", "HEAD"], repo.path());
    assert_cli_success(&detach, "switch --detach HEAD");

    let output = run_libra_command(&["--json", "symbolic-ref", "--quiet", "HEAD"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(1),
        "json quiet detached HEAD should preserve the quiet exit code"
    );
    assert!(
        output.stdout.is_empty(),
        "json quiet detached HEAD should not write stdout, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        human.is_empty(),
        "json mode should emit only the structured error envelope, got: {human}"
    );
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert_eq!(report.exit_code, 1);
    assert_eq!(report.message, "HEAD is not a symbolic ref");
    assert!(
        report.hints.is_empty(),
        "quiet mode should suppress guidance hints"
    );
}

#[test]
fn symbolic_ref_outside_repo_reports_repo_not_found() {
    let dir = tempdir().expect("failed to create non-repo directory");

    let output = run_libra_command(&["symbolic-ref", "HEAD"], dir.path());
    assert!(
        !output.status.success(),
        "symbolic-ref outside repo should fail"
    );

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        human.contains("not a libra repository"),
        "unexpected stderr: {human}"
    );
    assert_eq!(report.error_code, "LBR-REPO-001");
}

/// `libra symbolic-ref --help` surfaces the EXAMPLES banner so users
/// see the read, short-read, set, quiet, and JSON forms without
/// reading the design doc. Cross-cutting `--help` EXAMPLES rollout
/// per `docs/development/commands/_general.md` item B.
#[test]
fn test_symbolic_ref_help_lists_examples_banner() {
    let repo = tempdir().expect("tempdir for symbolic-ref --help");
    let output = run_libra_command(&["symbolic-ref", "--help"], repo.path());
    assert!(
        output.status.success(),
        "symbolic-ref --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "symbolic-ref --help should include EXAMPLES banner, stdout: {stdout}"
    );
    for invocation in [
        "libra symbolic-ref HEAD",
        "libra symbolic-ref --short HEAD",
        "libra symbolic-ref HEAD refs/heads/main",
        "libra symbolic-ref -q HEAD",
        "libra symbolic-ref --json HEAD",
    ] {
        assert!(
            stdout.contains(invocation),
            "symbolic-ref --help should include `{invocation}`, stdout: {stdout}"
        );
    }
}

/// issues/479 SR-01 M-SYMDEL: `-d`/`--delete` is accepted for the git t1401
/// contract and never deletes anything — `HEAD` is refused with git's
/// `deleting 'HEAD' is not allowed`, any other name with `Cannot delete
/// <name>, not a symbolic ref`; both exit 128 and leave state untouched.
#[test]
fn symbolic_ref_delete_matrix() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    let root = repo.path();

    let head_before = run_libra_command(&["symbolic-ref", "HEAD"], root);
    assert!(
        head_before.status.success(),
        "read HEAD before delete: {}",
        String::from_utf8_lossy(&head_before.stderr)
    );
    let head_target = String::from_utf8_lossy(&head_before.stdout)
        .trim()
        .to_string();

    for args in [
        vec!["symbolic-ref", "-d", "HEAD"],
        vec!["symbolic-ref", "--delete", "HEAD"],
    ] {
        let out = run_libra_command(&args, root);
        assert_eq!(
            out.status.code(),
            Some(128),
            "{args:?} must exit 128: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("deleting 'HEAD' is not allowed"),
            "{args:?} must print git's message: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // PM-21a: make `main` a real entity branch row before the refused delete,
    // and prove the reference table is untouched by it.
    run_libra_command(&["config", "set", "user.name", "t"], root);
    run_libra_command(&["config", "set", "user.email", "t@t"], root);
    std::fs::write(root.join("seed.txt"), "seed\n").expect("write seed file");
    assert_cli_success(&run_libra_command(&["add", "seed.txt"], root), "add seed");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "seed", "--no-verify"], root),
        "commit seed",
    );
    let snapshot = |root: &std::path::Path| -> String {
        let out = run_libra_command(&["show-ref"], root);
        assert_cli_success(&out, "show-ref snapshot");
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let refs_before = snapshot(root);

    // S3: a branch ref name gets the "not a symbolic ref" form (128).
    let branch = run_libra_command(&["symbolic-ref", "-d", "refs/heads/main"], root);
    assert_eq!(branch.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&branch.stderr)
            .contains("Cannot delete refs/heads/main, not a symbolic ref"),
        "branch-name message: {}",
        String::from_utf8_lossy(&branch.stderr)
    );
    assert_eq!(
        snapshot(root),
        refs_before,
        "refs unchanged by the refused delete"
    );

    // S2: an arbitrary name gets the same form.
    let other = run_libra_command(&["symbolic-ref", "--delete", "FOO"], root);
    assert_eq!(other.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&other.stderr).contains("Cannot delete FOO, not a symbolic ref"),
        "FOO message: {}",
        String::from_utf8_lossy(&other.stderr)
    );

    // S4: `-q -d` keeps the fatal (git prints it regardless of -q).
    let quiet = run_libra_command(&["symbolic-ref", "-q", "-d", "FOO"], root);
    assert_eq!(quiet.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&quiet.stderr).contains("Cannot delete FOO, not a symbolic ref"),
        "-q -d keeps the fatal: {}",
        String::from_utf8_lossy(&quiet.stderr)
    );

    // HEAD is unchanged after every refusal.
    let head_after = run_libra_command(&["symbolic-ref", "HEAD"], root);
    assert!(
        head_after.status.success(),
        "read HEAD after delete: {}",
        String::from_utf8_lossy(&head_after.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&head_after.stdout).trim(),
        head_target,
        "HEAD must be unchanged"
    );
}

#[test]
fn symbolic_ref_remote_head_read_write_delete() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    run_libra_command(&["config", "set", "user.name", "t"], repo.path());
    run_libra_command(&["config", "set", "user.email", "t@t"], repo.path());
    std::fs::write(repo.path().join("f.txt"), "a\n").expect("write fixture");
    assert_cli_success(&run_libra_command(&["add", "f.txt"], repo.path()), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], repo.path()),
        "commit",
    );

    // Seed the remote HEAD row.
    assert_cli_success(
        &run_libra_command(
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/branch1",
            ],
            repo.path(),
        ),
        "seed remote HEAD",
    );
    // AC1: the read prints the same remote's branch ref.
    let read = run_libra_command(&["symbolic-ref", "refs/remotes/origin/HEAD"], repo.path());
    assert_cli_success(&read, "read remote HEAD");
    assert_eq!(
        String::from_utf8_lossy(&read.stdout).trim(),
        "refs/remotes/origin/branch1",
        "AC1"
    );
    // AC2: writing another branch of the same remote takes effect.
    assert_cli_success(
        &run_libra_command(
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
            repo.path(),
        ),
        "rewrite remote HEAD",
    );
    let reread = run_libra_command(&["symbolic-ref", "refs/remotes/origin/HEAD"], repo.path());
    assert_cli_success(&reread, "reread remote HEAD");
    assert_eq!(
        String::from_utf8_lossy(&reread.stdout).trim(),
        "refs/remotes/origin/main",
        "AC2"
    );
    // AC3: `-d` removes the row; a later read is a target error.
    assert_cli_success(
        &run_libra_command(
            &["symbolic-ref", "-d", "refs/remotes/origin/HEAD"],
            repo.path(),
        ),
        "delete remote HEAD",
    );
    let after = run_libra_command(&["symbolic-ref", "refs/remotes/origin/HEAD"], repo.path());
    assert!(!after.status.success(), "AC3 read must fail");
    assert!(
        String::from_utf8_lossy(&after.stderr).contains("LBR-CLI-003"),
        "AC3 code"
    );
    // AC4/AC5: only the same remote's branch refs are accepted.
    for target in ["refs/remotes/upstream/x", "refs/heads/main"] {
        let bad = run_libra_command(
            &["symbolic-ref", "refs/remotes/origin/HEAD", target],
            repo.path(),
        );
        assert!(!bad.status.success(), "{target} must fail");
        assert!(
            String::from_utf8_lossy(&bad.stderr).contains("LBR-CLI-003"),
            "{target} code"
        );
    }
}

#[test]
fn symbolic_ref_heads_symref_read_write_delete() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    run_libra_command(&["config", "set", "user.name", "t"], repo.path());
    run_libra_command(&["config", "set", "user.email", "t@t"], repo.path());
    std::fs::write(repo.path().join("f.txt"), "a\n").expect("write fixture");
    assert_cli_success(&run_libra_command(&["add", "f.txt"], repo.path()), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], repo.path()),
        "commit",
    );

    // Set a `refs/heads/*` symbolic ref to a local entity branch.
    assert_cli_success(
        &run_libra_command(
            &["symbolic-ref", "refs/heads/a-branch", "refs/heads/main"],
            repo.path(),
        ),
        "set symref",
    );
    let read = run_libra_command(&["symbolic-ref", "refs/heads/a-branch"], repo.path());
    assert_cli_success(&read, "read symref");
    assert_eq!(String::from_utf8_lossy(&read.stdout), "refs/heads/main\n");
    let short = run_libra_command(
        &["symbolic-ref", "--short", "refs/heads/a-branch"],
        repo.path(),
    );
    assert_cli_success(&short, "short symref");
    assert_eq!(String::from_utf8_lossy(&short.stdout), "a-branch\n");

    // Updating retargets the row.
    assert_cli_success(
        &run_libra_command(
            &[
                "symbolic-ref",
                "refs/heads/a-branch",
                "refs/remotes/origin/branch1",
            ],
            repo.path(),
        ),
        "retarget symref",
    );
    let reread = run_libra_command(&["symbolic-ref", "refs/heads/a-branch"], repo.path());
    assert_cli_success(&reread, "reread symref");
    assert_eq!(
        String::from_utf8_lossy(&reread.stdout),
        "refs/remotes/origin/branch1\n"
    );

    // An existing entity branch cannot become a symbolic ref.
    let collision = run_libra_command(
        &["symbolic-ref", "refs/heads/main", "refs/heads/main"],
        repo.path(),
    );
    assert!(!collision.status.success(), "entity collision must fail");
    assert!(
        String::from_utf8_lossy(&collision.stderr).contains("LBR-CLI-003"),
        "collision code"
    );

    // Only refs/heads and refs/remotes targets are accepted.
    for target in ["refs/tags/v1", "refs/heads/a..b"] {
        let bad = run_libra_command(&["symbolic-ref", "refs/heads/zz", target], repo.path());
        assert!(!bad.status.success(), "{target} must fail");
        assert!(
            String::from_utf8_lossy(&bad.stderr).contains("LBR-CLI-003"),
            "{target} code"
        );
    }

    // Delete removes the row; a later read fails.
    assert_cli_success(
        &run_libra_command(&["symbolic-ref", "-d", "refs/heads/a-branch"], repo.path()),
        "delete symref",
    );
    let after = run_libra_command(&["symbolic-ref", "refs/heads/a-branch"], repo.path());
    assert!(!after.status.success(), "read after delete must fail");
    assert!(
        String::from_utf8_lossy(&after.stderr).contains("LBR-CLI-003"),
        "read-after-delete code"
    );
}

#[test]
fn checkout_resolves_a_refs_heads_symref_twice() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    run_libra_command(&["config", "set", "user.name", "t"], repo.path());
    run_libra_command(&["config", "set", "user.email", "t@t"], repo.path());
    std::fs::write(repo.path().join("f.txt"), "a\n").expect("write fixture");
    assert_cli_success(&run_libra_command(&["add", "f.txt"], repo.path()), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], repo.path()),
        "commit",
    );
    let head =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], repo.path()).stdout)
            .trim()
            .to_string();
    assert_cli_success(
        &run_libra_command(
            &["symbolic-ref", "refs/heads/a-branch", "refs/heads/main"],
            repo.path(),
        ),
        "set symref",
    );

    for attempt in 1..=2 {
        let out = run_libra_command(&["checkout", "-f", "a-branch"], repo.path());
        assert_cli_success(&out, &format!("checkout #{attempt}"));
        let status = run_libra_command(&["status", "--branch", "--short"], repo.path());
        let text = String::from_utf8_lossy(&status.stdout).to_string();
        assert!(text.contains("HEAD (detached"), "attempt {attempt}: {text}");
    }
    let resolved = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    assert_eq!(String::from_utf8_lossy(&resolved.stdout).trim(), head);
}
