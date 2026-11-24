//! Integration tests for `libra replace`.
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network). These run
//! the CLI as subprocesses, so each `libra` invocation rebuilds the
//! `refs/replace` peel cache from scratch.

use std::fs;

use tempfile::{TempDir, tempdir};

use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

/// A repo with a root commit "base" (HEAD~1) and a child commit "second" (HEAD).
fn two_commit_repo() -> TempDir {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "second\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], repo.path()),
        "stage second",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path()),
        "second commit",
    );
    repo
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// The core peel: replacing HEAD (the "second" commit) with HEAD~1 ("base")
/// makes `log` read the replacement through `load_object`.
#[test]
fn replace_makes_log_read_the_replacement() {
    let repo = two_commit_repo();

    // Sanity: before replacing, log shows "second".
    let before = run_libra_command(&["log", "-1"], repo.path());
    assert!(
        stdout(&before).contains("second"),
        "pre-state: {}",
        stdout(&before)
    );

    let created = run_libra_command(&["replace", "HEAD", "HEAD~1"], repo.path());
    assert_eq!(
        created.status.code(),
        Some(0),
        "replace failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );

    // HEAD's commit now reads back as "base" (its replacement, HEAD~1).
    let after = run_libra_command(&["log", "-1"], repo.path());
    let text = stdout(&after);
    assert!(text.contains("base"), "peel did not apply: {text}");
    assert!(!text.contains("second"), "replacement was not used: {text}");
}

#[test]
fn replace_list_shows_the_replaced_object() {
    let repo = two_commit_repo();
    let head = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    let head_oid = stdout(&head).trim().to_string();

    assert_eq!(
        run_libra_command(&["replace", "HEAD", "HEAD~1"], repo.path())
            .status
            .code(),
        Some(0)
    );
    let listed = run_libra_command(&["replace", "-l"], repo.path());
    assert_eq!(listed.status.code(), Some(0));
    assert!(
        stdout(&listed).contains(&head_oid),
        "list should contain {head_oid}: {}",
        stdout(&listed)
    );
}

#[test]
fn replace_delete_restores_the_original() {
    let repo = two_commit_repo();
    assert_eq!(
        run_libra_command(&["replace", "HEAD", "HEAD~1"], repo.path())
            .status
            .code(),
        Some(0)
    );
    let deleted = run_libra_command(&["replace", "-d", "HEAD"], repo.path());
    assert_eq!(deleted.status.code(), Some(0));

    // With the replacement gone, log reads "second" again.
    let after = run_libra_command(&["log", "-1"], repo.path());
    assert!(
        stdout(&after).contains("second"),
        "delete did not restore: {}",
        stdout(&after)
    );
}

#[test]
fn replace_existing_without_force_is_rejected() {
    let repo = two_commit_repo();
    assert_eq!(
        run_libra_command(&["replace", "HEAD", "HEAD~1"], repo.path())
            .status
            .code(),
        Some(0)
    );
    let again = run_libra_command(&["replace", "HEAD", "HEAD~1"], repo.path());
    assert_eq!(again.status.code(), Some(128), "overwrite should need -f");
    let forced = run_libra_command(&["replace", "-f", "HEAD", "HEAD~1"], repo.path());
    assert_eq!(forced.status.code(), Some(0), "-f should overwrite");
}

#[test]
fn replace_delete_missing_is_an_error() {
    let repo = two_commit_repo();
    let result = run_libra_command(&["replace", "-d", "HEAD"], repo.path());
    assert_eq!(result.status.code(), Some(128));
}

#[test]
fn replace_bad_object_is_an_error() {
    let repo = two_commit_repo();
    let result = run_libra_command(&["replace", "no-such-rev", "HEAD"], repo.path());
    assert_eq!(result.status.code(), Some(128));
}

#[test]
fn replace_outside_repository_is_an_error() {
    let dir = tempdir().unwrap();
    let result = run_libra_command(&["replace", "-l"], dir.path());
    assert_eq!(result.status.code(), Some(128));
}

/// MX-14/MX-15 (issues/481): `-l --format=short|medium|long`, `--graft`,
/// `--convert-graft-file`, and the `-d` "Deleted replace ref" line.
#[test]
fn replace_formats_graft_and_convert_graft_file() {
    let repo = super::create_committed_repo_via_cli();
    let root = repo.path();
    fs::write(root.join("second.txt"), "second\n").expect("write second file");
    super::assert_cli_success(&run_libra_command(&["add", "second.txt"], root), "stage");
    super::assert_cli_success(
        &run_libra_command(&["commit", "-m", "second", "--no-verify"], root),
        "commit",
    );
    let head = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], root).stdout)
        .trim()
        .to_string();
    let parent = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD~1"], root).stdout)
        .trim()
        .to_string();

    super::assert_cli_success(
        &run_libra_command(&["replace", &parent, &head], root),
        "create replacement",
    );
    let short = String::from_utf8_lossy(&run_libra_command(&["replace", "-l"], root).stdout)
        .trim()
        .to_string();
    assert_eq!(short, parent, "default -l prints the replaced oid");
    let medium = String::from_utf8_lossy(
        &run_libra_command(&["replace", "-l", "--format=medium"], root).stdout,
    )
    .trim()
    .to_string();
    assert_eq!(medium, format!("{parent} -> {head}"), "--format=medium");
    let long = String::from_utf8_lossy(
        &run_libra_command(&["replace", "-l", "--format=long"], root).stdout,
    )
    .trim()
    .to_string();
    assert_eq!(
        long,
        format!("{parent} (commit) -> {head} (commit)"),
        "--format=long"
    );

    // `-d` prints Git's "Deleted replace ref" line.
    let deleted = run_libra_command(&["replace", "-d", &parent], root);
    assert!(deleted.status.success(), "replace -d failed");
    assert_eq!(
        String::from_utf8_lossy(&deleted.stdout).trim(),
        format!("Deleted replace ref '{parent}'"),
        "-d output line"
    );

    // `--convert-graft-file` turns `.libra/info/grafts` into replace refs.
    fs::create_dir_all(root.join(".libra").join("info")).expect("mkdir info");
    fs::write(
        root.join(".libra").join("info").join("grafts"),
        format!("{head}\n"),
    )
    .expect("write grafts");
    let converted = run_libra_command(&["replace", "--convert-graft-file"], root);
    assert!(
        converted.status.success(),
        "--convert-graft-file failed: {}",
        String::from_utf8_lossy(&converted.stderr)
    );
    assert!(
        !root.join(".libra").join("info").join("grafts").exists(),
        "the graft file is removed after conversion"
    );
    let listed = String::from_utf8_lossy(&run_libra_command(&["replace", "-l"], root).stdout)
        .trim()
        .to_string();
    assert_eq!(listed, head, "the graft became a replace ref");

    // `--graft <commit>` (no parents) makes the commit parentless.
    let grafted = run_libra_command(&["replace", "--graft", &head, "-f"], root);
    assert!(
        grafted.status.success(),
        "--graft failed: {}",
        String::from_utf8_lossy(&grafted.stderr)
    );
    let medium = String::from_utf8_lossy(
        &run_libra_command(&["replace", "-l", "--format=medium"], root).stdout,
    )
    .trim()
    .to_string();
    assert!(
        medium.starts_with(&head) && medium.contains(" -> "),
        "--graft registered a replacement: {medium}"
    );
}

#[test]
fn replace_list_uses_wildmatch() {
    let repo = two_commit_repo();
    let head = stdout(&run_libra_command(&["rev-parse", "HEAD"], repo.path()))
        .trim()
        .to_string();
    let base = stdout(&run_libra_command(&["rev-parse", "HEAD~1"], repo.path()))
        .trim()
        .to_string();
    assert_cli_success(
        &run_libra_command(&["replace", &head, &base], repo.path()),
        "create replacement",
    );

    // AC1: `?` matches one character; the trailing `*` covers the rest — the
    // wildmatch is applied to the whole object name.
    let pattern = format!("{}?*", &head[..4]);
    let listed = run_libra_command(&["replace", "-l", &pattern], repo.path());
    assert_cli_success(&listed, "AC1");
    assert_eq!(stdout(&listed), format!("{head}\n"), "AC1");

    // AC2: a pattern that only matches a substring of the name lists nothing.
    let listed = run_libra_command(&["replace", "-l", "zzzz"], repo.path());
    assert_cli_success(&listed, "AC2");
    assert!(stdout(&listed).is_empty(), "AC2");
}

#[test]
fn replace_rejects_invalid_mode_combinations() {
    let repo = two_commit_repo();
    let head = stdout(&run_libra_command(&["rev-parse", "HEAD"], repo.path()))
        .trim()
        .to_string();
    let base = stdout(&run_libra_command(&["rev-parse", "HEAD~1"], repo.path()))
        .trim()
        .to_string();
    assert_cli_success(
        &run_libra_command(&["replace", &head, &base], repo.path()),
        "create replacement",
    );
    let snapshot = stdout(&run_libra_command(&["replace", "-l"], repo.path()));
    assert!(snapshot.contains(&head), "pre-state lists the replacement");

    let cases: Vec<Vec<&str>> = vec![
        // AC3: at most one `-l` pattern.
        vec!["replace", "-l", "a", "b"],
        // AC4: a create needs exactly two positional arguments.
        vec!["replace", &head],
        // AC5: `-d` and `-l` are incompatible.
        vec!["replace", "-l", "-d", &base],
        // AC6: `--format` is a list-only option.
        vec!["replace", "--format=medium", "-d", &head],
        // AC7: `-f` only makes sense when writing.
        vec!["replace", "-f"],
        // AC8: `-f` is invalid for delete too.
        vec!["replace", "-f", "-d", &head],
    ];
    for args in cases {
        let out = run_libra_command(&args, repo.path());
        assert!(!out.status.success(), "{args:?} must fail");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-002"),
            "{args:?} code: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // Failure-path invariant: the replace namespace is untouched.
    let after = stdout(&run_libra_command(&["replace", "-l"], repo.path()));
    assert_eq!(after, snapshot, "replace refs unchanged");
}
