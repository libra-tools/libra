//! Integration tests for `libra branch diff` (lore.md §1.12): thin sugar over
//! the diff engine — tip-to-tip, byte-identical to `diff BASE..BRANCH`.
//!
//! **Layer:** L1 — deterministic.

use super::*;

fn diverged_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("f.txt"), "a\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit",
    );
    assert_cli_success(&run_libra_command(&["branch", "feature"], p), "branch");
    assert_cli_success(&run_libra_command(&["switch", "feature"], p), "switch");
    fs::write(p.join("f.txt"), "b\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit2",
    );
    repo
}

#[test]
fn branch_diff_parity_defaults_and_exit_codes() {
    let repo = diverged_repo();
    let p = repo.path();
    // Byte parity with the underlying engine.
    let sugar = run_libra_command(&["branch", "diff", "main", "feature"], p);
    assert_cli_success(&sugar, "branch diff");
    let engine = run_libra_command(&["diff", "main..feature"], p);
    assert_eq!(sugar.stdout, engine.stdout, "byte-identical to diff A..B");
    // One-arg: base explicit, subject = current branch (tip-to-tip, no
    // worktree involvement even when dirty).
    fs::write(p.join("f.txt"), "dirty\n").unwrap();
    let one = run_libra_command(&["branch", "diff", "main"], p);
    assert_eq!(one.stdout, engine.stdout, "one-arg ignores the worktree");
    fs::write(p.join("f.txt"), "b\n").unwrap();
    // --merge-base three-dot parity.
    let mb = run_libra_command(&["branch", "diff", "--merge-base", "main", "feature"], p);
    let three = run_libra_command(&["diff", "main...feature"], p);
    assert_eq!(mb.stdout, three.stdout, "--merge-base = three-dot");
    // Exit codes: 0 with differences by default; --exit-code → 1.
    let plain = run_libra_command(&["branch", "diff", "main", "feature"], p);
    assert_eq!(plain.status.code(), Some(0));
    let ec = run_libra_command(&["branch", "diff", "main", "feature", "--exit-code"], p);
    assert_eq!(ec.status.code(), Some(1));
    let same = run_libra_command(&["branch", "diff", "feature", "feature", "--exit-code"], p);
    assert_eq!(same.status.code(), Some(0), "self-diff clean");
    // Pathspec after `--` + curated flags.
    let named = run_libra_command(
        &["branch", "diff", "main", "--name-status", "--", "f.txt"],
        p,
    );
    assert_eq!(String::from_utf8_lossy(&named.stdout).trim(), "M\tf.txt");
    // JSON delegates to the diff schema.
    let json_out = run_libra_command(&["--json", "branch", "diff", "main", "feature"], p);
    assert_cli_success(&json_out, "json");
    let json = parse_json_stdout(&json_out);
    assert_eq!(
        json["command"].as_str(),
        Some("diff"),
        "diff schema: {json}"
    );
}

#[test]
fn branch_diff_errors_and_reserved_word() {
    let repo = diverged_repo();
    let p = repo.path();
    // Unknown side → branch UX (129, LBR-CLI-003 target class).
    let bad = run_libra_command(&["branch", "diff", "mian", "feature"], p);
    assert_eq!(bad.status.code(), Some(129), "unknown base");
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("not found"),
        "{}",
        String::from_utf8_lossy(&bad.stderr)
    );
    // Zero-arg without upstream → tracking-info error with hints.
    let noup = run_libra_command(&["branch", "diff"], p);
    assert_eq!(noup.status.code(), Some(129));
    assert!(
        String::from_utf8_lossy(&noup.stderr).contains("no tracking information"),
        "{}",
        String::from_utf8_lossy(&noup.stderr)
    );
    // Reserved verb: flags + `diff` must REFUSE, never create a branch.
    for argv in [
        vec!["branch", "-v", "diff"],
        vec!["branch", "--no-column", "diff", "main"],
    ] {
        let out = run_libra_command(&argv, p);
        assert_eq!(out.status.code(), Some(129), "{argv:?} refused");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("reserved branch verb"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let list = run_libra_command(&["branch", "--list"], p);
    assert!(
        !String::from_utf8_lossy(&list.stdout).contains("diff"),
        "no branch named diff was ever created: {}",
        String::from_utf8_lossy(&list.stdout)
    );
    // The escape hatch still creates a literal `diff` branch.
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "diff"], p),
        "switch -c diff",
    );
    // Zero-arg WITH a LOCAL upstream (branch.<n>.remote = "."): resolves to
    // the local branch, matching Git's local-tracking form.
    assert_cli_success(&run_libra_command(&["switch", "feature"], p), "back");
    assert_cli_success(
        &run_libra_command(&["config", "branch.feature.remote", "."], p),
        "remote .",
    );
    assert_cli_success(
        &run_libra_command(&["config", "branch.feature.merge", "refs/heads/main"], p),
        "merge main",
    );
    let zero = run_libra_command(&["branch", "diff", "--name-status"], p);
    assert_cli_success(&zero, "zero-arg with local upstream");
    assert_eq!(String::from_utf8_lossy(&zero.stdout).trim(), "M\tf.txt");
}
