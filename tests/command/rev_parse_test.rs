//! Integration tests for `rev-parse` command.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use super::*;

/// Assert that `rev-parse --show-toplevel` prints the expected repository root.
///
/// Test coverage: the three direct worktree/storage-dir tests below pass temp
/// paths through both `/var` and canonical `/private/var` spellings on macOS,
/// while the symlink case verifies that an entered storage symlink still maps to
/// the canonical worktree root.
fn assert_show_toplevel_stdout_eq(output: &std::process::Output, expected: &std::path::Path) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let actual = std::path::PathBuf::from(stdout.trim());
    assert_eq!(
        actual
            .canonicalize()
            .expect("failed to canonicalize rev-parse output path"),
        expected
            .canonicalize()
            .expect("failed to canonicalize expected repo path")
    );
}

#[test]
fn test_rev_parse_head_resolves_commit() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    assert_cli_success(&output, "rev-parse HEAD");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let value = stdout.trim();
    assert_eq!(value.len(), 40, "expected full hash, got: {value}");
    assert!(value.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn test_rev_parse_short_head_returns_non_ambiguous_hash() {
    let repo = create_committed_repo_via_cli();

    let full = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    assert_cli_success(&full, "rev-parse HEAD (full)");
    let full_hash = String::from_utf8_lossy(&full.stdout).trim().to_string();

    let output = run_libra_command(&["rev-parse", "--short", "HEAD"], repo.path());
    assert_cli_success(&output, "rev-parse --short HEAD");

    let short_hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert!(
        short_hash.len() >= 7,
        "expected abbreviated hash, got: {short_hash}"
    );
    assert!(short_hash.len() <= full_hash.len());
    assert!(full_hash.starts_with(&short_hash));

    let resolved = run_libra_command(&["rev-parse", short_hash.as_str()], repo.path());
    assert_cli_success(&resolved, "rev-parse <short-hash>");
    assert_eq!(String::from_utf8_lossy(&resolved.stdout).trim(), full_hash);
}

/// Create a repo with two commits so the object database has several objects.
fn repo_with_two_commits() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("second.txt"), "second\n").expect("write second file");
    let add = run_libra_command(&["add", "second.txt"], repo.path());
    assert_cli_success(&add, "add second file");
    let commit = run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit second file");
    repo
}

/// M-OID O1/O2: after `repack` the same object exists both as a loose object
/// and in a pack (or, after two `repack -a -d`, in two packs). A unique short
/// prefix must still resolve instead of being reported as ambiguous.
#[test]
fn test_short_oid_after_repack_matrix() {
    let repo = repo_with_two_commits();
    let head = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    assert_cli_success(&head, "rev-parse HEAD");
    let full_hash = String::from_utf8_lossy(&head.stdout).trim().to_string();

    // O1: loose + pack coexistence (`repack` without `-d` keeps loose objects).
    let repack = run_libra_command(&["repack"], repo.path());
    assert_cli_success(&repack, "repack (loose + pack)");

    for prefix in [&full_hash[..10], &full_hash[..4]] {
        let out = run_libra_command(&["rev-parse", prefix], repo.path());
        assert_cli_success(&out, &format!("rev-parse {prefix} after loose+pack repack"));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            full_hash,
            "short prefix {prefix} must resolve to {full_hash}"
        );
    }

    // O2: two `repack -a -d` runs leave the same objects in two packs.
    let first = run_libra_command(&["repack", "-a", "-d"], repo.path());
    assert_cli_success(&first, "repack -a -d (first)");
    let second = run_libra_command(&["repack", "-a", "-d"], repo.path());
    assert_cli_success(&second, "repack -a -d (second)");

    for prefix in [&full_hash[..10], &full_hash[..4]] {
        let out = run_libra_command(&["rev-parse", prefix], repo.path());
        assert_cli_success(&out, &format!("rev-parse {prefix} after multi-pack repack"));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            full_hash,
            "short prefix {prefix} must resolve to {full_hash} after multi-pack repack"
        );
    }
}

/// M-OID O5: the same loose+pack dedup holds for a SHA-256 repository.
#[test]
fn test_short_oid_after_repack_sha256() {
    let repo = create_committed_repo_with_format("sha256");
    fs::write(repo.path().join("second.txt"), "second\n").expect("write second file");
    let add = run_libra_command(&["add", "second.txt"], repo.path());
    assert_cli_success(&add, "add second file (sha256)");
    let commit = run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path());
    assert_cli_success(&commit, "commit second file (sha256)");

    let head = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    assert_cli_success(&head, "rev-parse HEAD (sha256)");
    let full_hash = String::from_utf8_lossy(&head.stdout).trim().to_string();
    assert_eq!(full_hash.len(), 64, "sha256 full hash must be 64 hex chars");

    let repack = run_libra_command(&["repack"], repo.path());
    assert_cli_success(&repack, "repack (sha256 loose + pack)");

    for prefix in [&full_hash[..10], &full_hash[..4]] {
        let out = run_libra_command(&["rev-parse", prefix], repo.path());
        assert_cli_success(&out, &format!("rev-parse {prefix} (sha256 after repack)"));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            full_hash,
            "sha256 short prefix {prefix} must resolve to {full_hash}"
        );
    }
}

/// M-AMB A1/A2/A4/A5: two DISTINCT blobs sharing a 4-hex prefix produce the
/// shared Git-style ambiguity diagnostic (`error: short object ID <p> is
/// ambiguous` + `The candidates are:` hint with `<abbrev> <type>` lines);
/// `--json` exposes full OID/type/abbrev details; after `repack -a -d` the
/// candidate list is unchanged (no duplicates); `cat-file` and `show` emit the
/// same diagnostic family. Exit code 129 + LBR-CLI-003 per ADR-MX-02.
#[test]
fn test_ambiguous_short_oid_hint_matrix() {
    let repo = create_committed_repo_via_cli();
    let fixture = create_ambiguous_prefix_fixture(repo.path());
    let prefix = fixture.prefix.clone();

    // A1: human diagnostic with git-style message and candidate list.
    let output = run_libra_command(&["rev-parse", &prefix], repo.path());
    assert_eq!(
        output.status.code(),
        Some(129),
        "ambiguous rev-parse must exit 129, got {:?} / {} / {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert_eq!(report.exit_code, 129);
    assert!(
        human.contains(&format!("error: short object ID {prefix} is ambiguous")),
        "unexpected human stderr: {human}"
    );
    assert!(
        human.contains("Hint: The candidates are:"),
        "stderr: {human}"
    );

    let candidates = report.details["candidates"]
        .as_array()
        .expect("details.candidates must be an array");
    assert!(
        candidates.len() >= 2,
        "expected at least the two colliding blobs, got {candidates:?}"
    );
    // Storage-order (ascending OID) candidate lines, one per distinct object.
    let mut candidate_oids = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let oid = candidate["oid"]
            .as_str()
            .expect("candidate oid")
            .to_string();
        let object_type = candidate["type"].as_str().expect("candidate type");
        let abbrev = candidate["abbrev"].as_str().expect("candidate abbrev");
        assert_eq!(oid.len(), 40, "candidate oid must be a full hash");
        assert!(oid.starts_with(&prefix), "{oid} must share prefix {prefix}");
        assert!(
            oid.starts_with(abbrev) && abbrev.len() >= 7,
            "abbrev {abbrev} must be a >= 7-char prefix of {oid}"
        );
        assert!(
            ["blob", "tree", "commit", "tag"].contains(&object_type),
            "unexpected candidate type {object_type}"
        );
        // The hint lists `<abbrev> <type>` lines exactly like Git's
        // show_ambiguous_object (Libra renders the `Hint:` prefix house-style).
        assert!(
            human.contains(&format!("  {abbrev} {object_type}")),
            "hint must list {abbrev} {object_type}, stderr: {human}"
        );
        candidate_oids.push(oid);
    }
    assert!(
        candidate_oids.windows(2).all(|pair| pair[0] <= pair[1]),
        "candidates must be sorted by OID: {candidate_oids:?}"
    );
    // The two colliding fixture blobs must both appear.
    for oid in &fixture.oids {
        assert!(
            candidate_oids.iter().any(|candidate| candidate == oid),
            "fixture blob {oid} missing from candidates {candidate_oids:?}"
        );
    }

    // A2: after `repack -a -d` the same candidates appear with no duplicates.
    let repack = run_libra_command(&["repack", "-a", "-d"], repo.path());
    assert_cli_success(&repack, "repack -a -d for M-AMB A2");
    let output = run_libra_command(&["rev-parse", &prefix], repo.path());
    assert_eq!(output.status.code(), Some(129));
    let (_, report_after) = parse_cli_error_stderr(&output.stderr);
    let candidates_after = report_after.details["candidates"]
        .as_array()
        .expect("details.candidates must be an array after repack");
    assert_eq!(
        candidates_after.len(),
        candidates.len(),
        "repack must not duplicate or drop candidates"
    );

    // A4: `--json` puts the same candidates in the error envelope details.
    let output = run_libra_command(&["rev-parse", "--json", &prefix], repo.path());
    assert_eq!(output.status.code(), Some(129));
    let (_, json_report) = parse_cli_error_stderr(&output.stderr);
    let json_candidates = json_report.details["candidates"]
        .as_array()
        .expect("json details.candidates must be an array");
    assert_eq!(json_candidates.len(), candidates.len());
    for candidate in json_candidates {
        let oid = candidate["oid"].as_str().expect("candidate oid");
        assert!(
            ["blob", "tree", "commit", "tag"]
                .contains(&candidate["type"].as_str().expect("candidate type")),
            "unexpected json candidate: {candidate:?}"
        );
        assert!(candidate_oids.iter().any(|candidate| candidate == oid));
    }

    // A5: cat-file and show emit the same diagnostic family.
    for args in [vec!["cat-file", "-t", &prefix], vec!["show", &prefix]] {
        let output = run_libra_command(&args, repo.path());
        assert_eq!(
            output.status.code(),
            Some(129),
            "{} must exit 129 on ambiguity",
            args.join(" ")
        );
        let (human, _) = parse_cli_error_stderr(&output.stderr);
        assert!(
            human.contains(&format!("error: short object ID {prefix} is ambiguous")),
            "{} stderr: {human}",
            args.join(" ")
        );
    }
}

#[test]
fn test_rev_parse_abbrev_ref_head_returns_branch_name() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["rev-parse", "--abbrev-ref", "HEAD"], repo.path());
    assert_cli_success(&output, "rev-parse --abbrev-ref HEAD");

    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "main");
}

#[tokio::test]
#[serial(cwd)]
async fn test_rev_parse_abbrev_ref_remote_tracking_ref_returns_short_name() {
    let repo = tempdir().expect("failed to create repository root");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = ChangeDirGuard::new(repo.path());

    commit::execute(CommitArgs {
        message: Some("base".to_string()),
        allow_empty: true,
        disable_pre: true,
        no_verify: false,
        ..Default::default()
    })
    .await;

    let head = Head::current_commit().await.expect("expected HEAD commit");
    Branch::update_branch(
        "refs/remotes/origin/main",
        &head.to_string(),
        Some("origin"),
    )
    .await
    .expect("failed to create remote-tracking ref");

    let output = run_libra_command(&["rev-parse", "--abbrev-ref", "origin/main"], repo.path());
    assert_cli_success(&output, "rev-parse --abbrev-ref origin/main");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "origin/main"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_rev_parse_abbrev_ref_multi_segment_remote_tracking_ref_returns_short_name() {
    let repo = tempdir().expect("failed to create repository root");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = ChangeDirGuard::new(repo.path());

    commit::execute(CommitArgs {
        message: Some("base".to_string()),
        allow_empty: true,
        disable_pre: true,
        no_verify: false,
        ..Default::default()
    })
    .await;

    let head = Head::current_commit().await.expect("expected HEAD commit");
    Branch::update_branch(
        "refs/remotes/upstream/origin/main",
        &head.to_string(),
        Some("upstream/origin"),
    )
    .await
    .expect("failed to create multi-segment remote-tracking ref");

    let output = run_libra_command(
        &["rev-parse", "--abbrev-ref", "upstream/origin/main"],
        repo.path(),
    );
    assert_cli_success(&output, "rev-parse --abbrev-ref upstream/origin/main");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "upstream/origin/main"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_rev_parse_abbrev_ref_lowercase_head_resolves_branch_name() {
    let repo = tempdir().expect("failed to create repository root");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = ChangeDirGuard::new(repo.path());

    commit::execute(CommitArgs {
        message: Some("base".to_string()),
        allow_empty: true,
        disable_pre: true,
        no_verify: false,
        ..Default::default()
    })
    .await;

    let head = Head::current_commit().await.expect("expected HEAD commit");
    Branch::update_branch("head", &head.to_string(), None)
        .await
        .expect("failed to create lowercase head branch");

    let output = run_libra_command(&["rev-parse", "--abbrev-ref", "head"], repo.path());
    assert_cli_success(&output, "rev-parse --abbrev-ref head");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "head");
}

#[tokio::test]
#[serial(cwd)]
async fn test_rev_parse_abbrev_ref_refs_heads_returns_short_name() {
    let repo = tempdir().expect("failed to create repository root");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = ChangeDirGuard::new(repo.path());

    commit::execute(CommitArgs {
        message: Some("base".to_string()),
        allow_empty: true,
        disable_pre: true,
        no_verify: false,
        ..Default::default()
    })
    .await;

    let output = run_libra_command(
        &["rev-parse", "--abbrev-ref", "refs/heads/main"],
        repo.path(),
    );
    assert_cli_success(&output, "rev-parse --abbrev-ref refs/heads/main");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "main");
}

#[tokio::test]
#[serial(cwd)]
async fn test_rev_parse_abbrev_ref_refs_remotes_returns_short_name() {
    let repo = tempdir().expect("failed to create repository root");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = ChangeDirGuard::new(repo.path());

    commit::execute(CommitArgs {
        message: Some("base".to_string()),
        allow_empty: true,
        disable_pre: true,
        no_verify: false,
        ..Default::default()
    })
    .await;

    let head = Head::current_commit().await.expect("expected HEAD commit");
    Branch::update_branch(
        "refs/remotes/origin/main",
        &head.to_string(),
        Some("origin"),
    )
    .await
    .expect("failed to create remote-tracking ref");

    let output = run_libra_command(
        &["rev-parse", "--abbrev-ref", "refs/remotes/origin/main"],
        repo.path(),
    );
    assert_cli_success(&output, "rev-parse --abbrev-ref refs/remotes/origin/main");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "origin/main"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_rev_parse_abbrev_ref_prefers_exact_local_refs_remotes_name() {
    let repo = tempdir().expect("failed to create repository root");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = ChangeDirGuard::new(repo.path());

    commit::execute(CommitArgs {
        message: Some("base".to_string()),
        allow_empty: true,
        disable_pre: true,
        no_verify: false,
        ..Default::default()
    })
    .await;

    let head = Head::current_commit().await.expect("expected HEAD commit");
    Branch::update_branch("refs/remotes/origin/main", &head.to_string(), None)
        .await
        .expect("failed to create local branch named like remote-tracking ref");

    let output = run_libra_command(
        &["rev-parse", "--abbrev-ref", "refs/remotes/origin/main"],
        repo.path(),
    );
    assert_cli_success(
        &output,
        "rev-parse --abbrev-ref exact local refs/remotes/origin/main",
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "refs/remotes/origin/main"
    );
}

#[test]
fn test_rev_parse_show_toplevel_repo_named_storage_dir_returns_repo_root() {
    let parent = tempdir().expect("failed to create parent directory");
    let repo_path = parent.path().join(libra::utils::util::ROOT_DIR);
    init_repo_via_cli(&repo_path);

    let output = run_libra_command(&["rev-parse", "--show-toplevel"], &repo_path);
    assert_cli_success(
        &output,
        "rev-parse --show-toplevel from repo root named .libra",
    );

    // Scenario: a repository whose worktree itself is named `.libra` must not
    // be mistaken for the internal storage directory.
    assert_show_toplevel_stdout_eq(&output, &repo_path);
}

#[test]
fn test_rev_parse_show_toplevel_returns_repo_root() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["rev-parse", "--show-toplevel"], repo.path());
    assert_cli_success(&output, "rev-parse --show-toplevel from repo root");

    // Scenario: the normal worktree-root invocation returns the root path,
    // allowing platform-specific tempdir symlinks to differ only in spelling.
    assert_show_toplevel_stdout_eq(&output, repo.path());
}

#[test]
fn test_rev_parse_show_toplevel_from_storage_dir_returns_repo_root() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    let storage = repo.path().join(libra::utils::util::ROOT_DIR);

    let output = run_libra_command(&["rev-parse", "--show-toplevel"], &storage);
    assert_cli_success(&output, "rev-parse --show-toplevel from .libra");

    // Scenario: entering the physical `.libra` storage directory reports the
    // enclosing worktree root rather than the storage path itself.
    assert_show_toplevel_stdout_eq(&output, repo.path());
}

#[cfg(unix)]
#[test]
fn test_rev_parse_show_toplevel_from_symlinked_storage_dir_returns_repo_root() {
    use std::os::unix::fs::symlink;

    let temp_root = tempdir().expect("failed to create temp root");
    let repo = temp_root.path().join("repo");
    init_repo_via_cli(&repo);

    let storage = repo.join(libra::utils::util::ROOT_DIR);
    let storage_link = temp_root.path().join("storage-link");
    symlink(&storage, &storage_link).expect("failed to create storage symlink");

    let output = run_libra_command(&["rev-parse", "--show-toplevel"], &storage_link);
    assert_cli_success(&output, "rev-parse --show-toplevel from symlinked .libra");

    // Scenario: a symlink pointing at `.libra` is resolved back to the real
    // worktree root, matching Git's behavior for storage-directory traversal.
    assert_show_toplevel_stdout_eq(&output, &repo);
}

#[test]
fn test_rev_parse_show_toplevel_in_bare_repo_returns_work_tree_error() {
    let repo = tempdir().expect("failed to create repository root");
    let bare_repo = repo.path().join("repo.git");

    let init_output = run_libra_command(
        &["init", "--bare", "repo.git", "--vault", "false"],
        repo.path(),
    );
    assert_cli_success(&init_output, "init bare repo for rev-parse test");

    let output = run_libra_command(&["rev-parse", "--show-toplevel"], &bare_repo);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert!(stderr.contains("this operation must be run in a work tree"));
    assert_eq!(report.error_code, "LBR-REPO-003");
}

#[test]
fn test_rev_parse_show_toplevel_rejects_spec() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["rev-parse", "--show-toplevel", "HEAD"], repo.path());

    assert!(!output.status.success(), "command unexpectedly succeeded");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be used with") || stderr.contains("unexpected argument"),
        "stderr: {stderr}"
    );
}

#[test]
fn test_rev_parse_invalid_target_returns_cli_error_code() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["rev-parse", "badref"], repo.path());
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert!(stderr.contains("not a valid object name: 'badref'"));
    assert_eq!(report.error_code, "LBR-CLI-003");
}

#[test]
fn test_rev_parse_verify_resolves_single_object() {
    let repo = create_committed_repo_via_cli();
    let verify = run_libra_command(&["rev-parse", "--verify", "HEAD"], repo.path());
    assert_cli_success(&verify, "rev-parse --verify HEAD");
    let plain = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    assert_eq!(
        String::from_utf8_lossy(&verify.stdout).trim(),
        String::from_utf8_lossy(&plain.stdout).trim(),
        "--verify should print the same hash as a plain resolve"
    );
}

#[test]
fn test_rev_parse_verify_unresolvable_exits_128() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(
        &["rev-parse", "--verify", "definitely-not-a-ref"],
        repo.path(),
    );
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Needed a single revision"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn test_rev_parse_verify_quiet_unresolvable_exits_1_silently() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["--quiet", "rev-parse", "--verify", "nope"], repo.path());
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "quiet --verify must print nothing"
    );
    assert!(
        output.stderr.is_empty(),
        "quiet --verify must not print a diagnostic, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_rev_parse_default_used_when_no_spec() {
    let repo = create_committed_repo_via_cli();
    let with_default = run_libra_command(&["rev-parse", "--default", "HEAD"], repo.path());
    assert_cli_success(&with_default, "rev-parse --default HEAD");
    let head = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    assert_eq!(
        String::from_utf8_lossy(&with_default.stdout).trim(),
        String::from_utf8_lossy(&head.stdout).trim(),
        "--default should resolve to HEAD when no SPEC is given"
    );
}

#[test]
fn test_rev_parse_is_inside_work_tree_true() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["rev-parse", "--is-inside-work-tree"], repo.path());
    assert_cli_success(&output, "rev-parse --is-inside-work-tree");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "true");
}

#[test]
fn test_rev_parse_git_dir_points_at_libra_dir() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["rev-parse", "--git-dir"], repo.path());
    assert_cli_success(&output, "rev-parse --git-dir");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().contains(".libra"),
        "git-dir should point at the .libra dir, got {stdout}"
    );
}

#[test]
fn test_rev_parse_keeps_tag_object_and_supports_typed_tree_peel() {
    let repo = create_committed_repo_via_cli();
    let tag_id = create_non_commit_tag_object(repo.path());

    let output = run_libra_command(&["rev-parse", tag_id.as_str()], repo.path());
    assert_cli_success(&output, "rev-parse raw tag object");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), tag_id);

    let peeled = run_libra_command(&["rev-parse", &format!("{tag_id}^{{tree}}")], repo.path());
    assert_cli_success(&peeled, "rev-parse tag^{tree}");
    let tree_id = String::from_utf8_lossy(&peeled.stdout).trim().to_string();
    let object_type = run_libra_command(&["cat-file", "-t", &tree_id], repo.path());
    assert_cli_success(&object_type, "cat-file -t peeled tree");
    assert_eq!(String::from_utf8_lossy(&object_type.stdout).trim(), "tree");
}

#[test]
fn test_rev_parse_json_returns_envelope() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "rev-parse", "HEAD"], repo.path());
    assert_cli_success(&output, "json rev-parse HEAD");

    let json = parse_json_stdout(&output);
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "rev-parse");
    assert_eq!(json["data"]["mode"], "resolve");
    assert_eq!(json["data"]["input"], "HEAD");
    assert!(json["data"]["value"].as_str().is_some());
}

#[test]
fn test_rev_parse_machine_returns_single_json_line() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--machine", "rev-parse", "HEAD"], repo.path());
    assert_cli_success(&output, "machine rev-parse HEAD");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().count(),
        1,
        "expected one JSON line, got: {stdout}"
    );

    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("expected JSON");
    assert_eq!(parsed["command"], "rev-parse");
    assert_eq!(parsed["data"]["mode"], "resolve");
}

/// `libra rev-parse --help` surfaces the EXAMPLES banner so users see
/// the four mutually-exclusive modes (resolve / --short / --abbrev-ref
/// / --show-toplevel) plus the JSON variant for agents. Cross-cutting
/// `--help` EXAMPLES rollout per `docs/development/commands/_general.md` item B.
#[test]
fn test_rev_parse_help_lists_examples_banner() {
    let repo = tempdir().expect("tempdir for rev-parse --help");
    let output = run_libra_command(&["rev-parse", "--help"], repo.path());
    assert!(
        output.status.success(),
        "rev-parse --help should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EXAMPLES:"),
        "rev-parse --help should include EXAMPLES banner, stdout: {stdout}"
    );
    for invocation in [
        "libra rev-parse HEAD",
        "libra rev-parse main~3",
        "libra rev-parse --short HEAD",
        "libra rev-parse --abbrev-ref HEAD",
        "libra rev-parse --show-toplevel",
        "libra rev-parse --json HEAD",
    ] {
        assert!(
            stdout.contains(invocation),
            "rev-parse --help should include `{invocation}`, stdout: {stdout}"
        );
    }
}

#[test]
fn test_rev_parse_show_prefix_at_root() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["rev-parse", "--show-prefix"], repo.path());
    assert_cli_success(&output, "rev-parse --show-prefix");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "",
        "show-prefix at repo root should be empty"
    );
}

#[test]
fn test_rev_parse_show_prefix_in_subdir() {
    let repo = create_committed_repo_via_cli();
    let subdir = repo.path().join("src");
    std::fs::create_dir_all(&subdir).expect("create subdir");
    let output = run_libra_command(&["rev-parse", "--show-prefix"], &subdir);
    assert_cli_success(&output, "rev-parse --show-prefix in subdir");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "src/",
        "show-prefix in subdir should be 'src/'"
    );
}

#[test]
fn test_rev_parse_show_cdup_at_root() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["rev-parse", "--show-cdup"], repo.path());
    assert_cli_success(&output, "rev-parse --show-cdup");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "", "show-cdup at repo root should be empty");
}

#[test]
fn test_rev_parse_show_cdup_in_subdir() {
    let repo = create_committed_repo_via_cli();
    let subdir = repo.path().join("a").join("b");
    std::fs::create_dir_all(&subdir).expect("create subdir");
    let output = run_libra_command(&["rev-parse", "--show-cdup"], &subdir);
    assert_cli_success(&output, "rev-parse --show-cdup in subdir");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "../../",
        "show-cdup in a/b should be '../../'"
    );
}

#[test]
fn test_rev_parse_short_with_length() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["rev-parse", "--short=8", "HEAD"], repo.path());
    assert_cli_success(&output, "rev-parse --short=8 HEAD");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim().len(), 8, "short=8 should produce 8-char hash");
}

#[test]
fn test_rev_parse_is_inside_git_dir() {
    let repo = create_committed_repo_via_cli();

    // From the worktree root: not inside the .libra directory.
    let out = run_libra_command(&["rev-parse", "--is-inside-git-dir"], repo.path());
    assert_cli_success(&out, "rev-parse --is-inside-git-dir from worktree");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "false");

    // From inside the .libra directory: true (Libra's GIT_DIR equivalent).
    let libra_dir = repo.path().join(".libra");
    let out = run_libra_command(&["rev-parse", "--is-inside-git-dir"], &libra_dir);
    assert_cli_success(&out, "rev-parse --is-inside-git-dir from .libra");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "true");
}

#[test]
fn test_rev_parse_absolute_git_dir_is_canonical_absolute() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    let out = run_libra_command(&["rev-parse", "--absolute-git-dir"], p);
    assert_cli_success(&out, "rev-parse --absolute-git-dir");
    let abs = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        std::path::Path::new(&abs).is_absolute(),
        "absolute path: {abs:?}"
    );
    assert!(abs.ends_with(".libra"), "points at .libra: {abs:?}");

    // In Libra `--git-dir` is already absolute, so the two coincide.
    let gd = run_libra_command(&["rev-parse", "--git-dir"], p);
    assert_cli_success(&gd, "rev-parse --git-dir");
    assert_eq!(
        abs,
        String::from_utf8_lossy(&gd.stdout).trim(),
        "--absolute-git-dir matches --git-dir"
    );

    // Mutually exclusive with --git-dir.
    let both = run_libra_command(&["rev-parse", "--absolute-git-dir", "--git-dir"], p);
    assert!(!both.status.success(), "conflicting flags rejected");
}

#[test]
fn test_rev_parse_sq_single_quotes_resolved_object() {
    let repo = create_committed_repo_via_cli();

    let plain = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    assert_cli_success(&plain, "rev-parse HEAD");
    let hash = String::from_utf8_lossy(&plain.stdout).trim().to_string();

    // `--sq` single-quotes the resolved object name.
    let sq = run_libra_command(&["rev-parse", "--sq", "HEAD"], repo.path());
    assert_cli_success(&sq, "rev-parse --sq HEAD");
    let quoted = String::from_utf8_lossy(&sq.stdout).trim().to_string();
    assert_eq!(quoted, format!("'{hash}'"), "expected single-quoted hash");

    // `--sq` does not quote the repository-query modes (matches Git).
    let toplevel = run_libra_command(&["rev-parse", "--sq", "--show-toplevel"], repo.path());
    assert_cli_success(&toplevel, "rev-parse --sq --show-toplevel");
    let path = String::from_utf8_lossy(&toplevel.stdout).trim().to_string();
    assert!(
        !path.starts_with('\'') && !path.ends_with('\''),
        "query modes must not be shell-quoted: {path:?}"
    );
}

#[test]
fn test_rev_parse_symbolic_full_name() {
    // `--symbolic-full-name` resolves a spec to its full ref name (refs/heads,
    // refs/tags, or HEAD's branch), prints nothing for a valid non-ref object, and
    // fails with exit 128 for an unresolvable name — matching git rev-parse.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let head_branch = String::from_utf8_lossy(
        &run_libra_command(&["rev-parse", "--abbrev-ref", "HEAD"], p).stdout,
    )
    .trim()
    .to_string();
    let full = format!("refs/heads/{head_branch}");

    let out = |args: &[&str]| {
        let o = run_libra_command(args, p);
        (
            String::from_utf8_lossy(&o.stdout).trim().to_string(),
            o.status.code(),
        )
    };

    // HEAD -> its branch's full name.
    let (v, code) = out(&["rev-parse", "--symbolic-full-name", "HEAD"]);
    assert_eq!(code, Some(0));
    assert_eq!(v, full, "HEAD resolves to its full branch ref");

    let (v, code) = out(&["rev-parse", "--symbolic-full-name", "@"]);
    assert_eq!(code, Some(0));
    assert_eq!(v, full, "@ resolves like HEAD in symbolic-full-name mode");

    let (v, code) = out(&["rev-parse", "--abbrev-ref", "@"]);
    assert_eq!(code, Some(0));
    assert_eq!(v, head_branch, "@ resolves like HEAD in abbrev-ref mode");

    // A bare branch name -> refs/heads/<name>.
    let (v, _) = out(&["rev-parse", "--symbolic-full-name", &head_branch]);
    assert_eq!(v, full);

    // refs/heads/<name> is returned verbatim.
    let (v, _) = out(&["rev-parse", "--symbolic-full-name", &full]);
    assert_eq!(v, full);

    // A tag -> refs/tags/<name>.
    assert_cli_success(&run_libra_command(&["tag", "v9.9"], p), "create tag v9.9");
    let (v, code) = out(&["rev-parse", "--symbolic-full-name", "v9.9"]);
    assert_eq!(code, Some(0));
    assert_eq!(v, "refs/tags/v9.9");

    // A valid object that is not a ref (the commit SHA) prints nothing, exit 0 —
    // byte-exact empty stdout (not even a trailing newline).
    let sha = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    let commit_out = run_libra_command(&["rev-parse", "--symbolic-full-name", &sha], p);
    assert_eq!(commit_out.status.code(), Some(0));
    assert!(
        commit_out.stdout.is_empty(),
        "a non-ref commit object emits no bytes: {:?}",
        String::from_utf8_lossy(&commit_out.stdout)
    );

    // A raw tree object id (not a ref) also prints nothing, exit 0.
    let tree_sha =
        String::from_utf8_lossy(&run_libra_command(&["cat-file", "-p", "HEAD"], p).stdout)
            .lines()
            .find_map(|l| l.strip_prefix("tree ").map(|s| s.trim().to_string()))
            .expect("HEAD commit lists a tree");
    let tree_out = run_libra_command(&["rev-parse", "--symbolic-full-name", &tree_sha], p);
    assert_eq!(tree_out.status.code(), Some(0));
    assert!(
        tree_out.stdout.is_empty(),
        "a raw tree object id emits no bytes: {:?}",
        String::from_utf8_lossy(&tree_out.stdout)
    );

    // An unresolvable spec fails with exit 128 (git's "ambiguous argument").
    let (_, code) = out(&["rev-parse", "--symbolic-full-name", "definitely-not-a-ref"]);
    assert_eq!(code, Some(128), "unresolvable spec exits 128");

    // A malformed revision expression the strict parser rejects is unresolvable
    // (exit 128) — it must NOT be permissively re-resolved to empty/exit 0.
    let (_, code) = out(&["rev-parse", "--symbolic-full-name", "HEAD^garbage"]);
    assert_eq!(code, Some(128), "malformed peel/navigation spec exits 128");

    // Detached HEAD -> "HEAD".
    assert_cli_success(
        &run_libra_command(&["checkout", &sha], p),
        "detach HEAD at the commit",
    );
    let (v, code) = out(&["rev-parse", "--symbolic-full-name", "HEAD"]);
    assert_eq!(code, Some(0));
    assert_eq!(v, "HEAD", "detached HEAD resolves to literal HEAD");
}

#[test]
fn test_rev_parse_symbolic_echoes_resolvable_specs_verbatim() {
    // `--symbolic` prints a resolvable spec "as close to the original input as
    // possible" — i.e. verbatim — rather than expanding a ref to its full name
    // (the way `--symbolic-full-name` does). An unresolvable name fails (exit 128).
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let head_branch = String::from_utf8_lossy(
        &run_libra_command(&["rev-parse", "--abbrev-ref", "HEAD"], p).stdout,
    )
    .trim()
    .to_string();
    let full = format!("refs/heads/{head_branch}");

    let out = |args: &[&str]| {
        let o = run_libra_command(args, p);
        (
            String::from_utf8_lossy(&o.stdout).trim().to_string(),
            o.status.code(),
        )
    };

    // HEAD -> "HEAD" verbatim (NOT expanded to refs/heads/<branch>).
    let (v, code) = out(&["rev-parse", "--symbolic", "HEAD"]);
    assert_eq!(code, Some(0));
    assert_eq!(v, "HEAD", "--symbolic echoes HEAD verbatim");

    // A bare branch name stays the bare name (the key contrast with
    // --symbolic-full-name, which would print {full}).
    let (v, code) = out(&["rev-parse", "--symbolic", &head_branch]);
    assert_eq!(code, Some(0));
    assert_eq!(v, head_branch, "--symbolic keeps the short branch name");
    assert_ne!(
        v, full,
        "--symbolic must NOT expand to the full ref name like --symbolic-full-name"
    );

    // A fully-qualified ref is echoed verbatim too.
    let (v, _) = out(&["rev-parse", "--symbolic", &full]);
    assert_eq!(v, full);

    // A tag stays the bare tag name (not refs/tags/<name>).
    assert_cli_success(&run_libra_command(&["tag", "v9.9"], p), "create tag v9.9");
    let (v, code) = out(&["rev-parse", "--symbolic", "v9.9"]);
    assert_eq!(code, Some(0));
    assert_eq!(v, "v9.9");

    // A revision expression is echoed verbatim (git keeps `~`/`^` forms as-is).
    let (v, code) = out(&["rev-parse", "--symbolic", "HEAD~0"]);
    assert_eq!(code, Some(0));
    assert_eq!(v, "HEAD~0");

    // A bare commit SHA (a valid object that is not a ref) is echoed verbatim —
    // unlike --symbolic-full-name, which prints nothing for a non-ref object.
    let sha = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    let (v, code) = out(&["rev-parse", "--symbolic", &sha]);
    assert_eq!(code, Some(0));
    assert_eq!(v, sha, "a bare object id is echoed verbatim");

    // A raw tree object id is also a valid object -> echoed verbatim.
    let tree_sha =
        String::from_utf8_lossy(&run_libra_command(&["cat-file", "-p", "HEAD"], p).stdout)
            .lines()
            .find_map(|l| l.strip_prefix("tree ").map(|s| s.trim().to_string()))
            .expect("HEAD commit lists a tree");
    let (v, code) = out(&["rev-parse", "--symbolic", &tree_sha]);
    assert_eq!(code, Some(0));
    assert_eq!(v, tree_sha);

    // An unresolvable spec fails with exit 128 (Libra's documented divergence:
    // it errors on stderr rather than echoing the spec like git does).
    let bad = run_libra_command(&["rev-parse", "--symbolic", "definitely-not-a-ref"], p);
    assert_eq!(bad.status.code(), Some(128), "unresolvable spec exits 128");
    assert!(
        bad.stdout.is_empty(),
        "an unresolvable spec emits no stdout: {:?}",
        String::from_utf8_lossy(&bad.stdout)
    );

    // A malformed revision expression is unresolvable (exit 128).
    let (_, code) = out(&["rev-parse", "--symbolic", "HEAD^garbage"]);
    assert_eq!(code, Some(128));

    // Typed peel and fully-qualified ref navigation are valid revision
    // expressions. `--symbolic` preserves the spelling while plain rev-parse
    // emits the resolved object id.
    for spec in ["HEAD^{commit}", "HEAD^{tree}", &format!("{full}~0")] {
        let sym = run_libra_command(&["rev-parse", "--symbolic", spec], p);
        let plain = run_libra_command(&["rev-parse", spec], p);
        assert_cli_success(&sym, &format!("rev-parse --symbolic {spec}"));
        assert_eq!(String::from_utf8_lossy(&sym.stdout).trim(), spec);
        assert_cli_success(&plain, &format!("rev-parse {spec}"));
    }

    // --symbolic and --symbolic-full-name are mutually exclusive (clap usage error).
    let conflict = run_libra_command(
        &["rev-parse", "--symbolic", "--symbolic-full-name", "HEAD"],
        p,
    );
    assert_eq!(
        conflict.status.code(),
        Some(129),
        "conflicting output modes are a usage error (Libra maps clap conflicts to 129)"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_rev_parse_symbolic_full_name_remote_tracking_ref() {
    // A remote-tracking spec resolves to its full `refs/remotes/<remote>/<branch>`.
    let repo = tempdir().expect("failed to create repository root");
    test::setup_with_new_libra_in(repo.path()).await;
    let _guard = ChangeDirGuard::new(repo.path());

    commit::execute(CommitArgs {
        message: Some("base".to_string()),
        allow_empty: true,
        disable_pre: true,
        no_verify: false,
        ..Default::default()
    })
    .await;

    let head = Head::current_commit().await.expect("expected HEAD commit");
    Branch::update_branch(
        "refs/remotes/origin/main",
        &head.to_string(),
        Some("origin"),
    )
    .await
    .expect("failed to create remote-tracking ref");

    let output = run_libra_command(
        &["rev-parse", "--symbolic-full-name", "origin/main"],
        repo.path(),
    );
    assert_cli_success(&output, "rev-parse --symbolic-full-name origin/main");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "refs/remotes/origin/main"
    );
}

#[test]
fn test_rev_parse_multiple_specs_resolve_each() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "branch feature",
    );
    let sha = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();

    // Each positional spec resolves on its own line (matching git rev-parse).
    let out = run_libra_command(&["rev-parse", "HEAD", "feature"], p);
    assert_cli_success(&out, "rev-parse HEAD feature");
    let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines, vec![sha.clone(), sha.clone()], "both specs resolve");
}

#[test]
fn test_rev_parse_output_filter_modes() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "branch feature",
    );
    let sha = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();

    let lines = |args: &[&str]| -> Vec<String> {
        let out = run_libra_command(args, p);
        assert_cli_success(&out, "rev-parse filter");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    };

    // --flags: keep revisions (resolved) + flags, drop non-flag paths.
    assert_eq!(
        lines(&["rev-parse", "--flags", "HEAD", "feature", "-x", "--foo"]),
        vec![
            sha.clone(),
            sha.clone(),
            "-x".to_string(),
            "--foo".to_string()
        ]
    );
    // --no-flags: drop flags, keep revisions (and paths).
    assert_eq!(
        lines(&["rev-parse", "--no-flags", "HEAD", "-x", "feature"]),
        vec![sha.clone(), sha.clone()]
    );
    // --revs-only: keep only revisions, drop flags and non-rev paths.
    assert_eq!(
        lines(&[
            "rev-parse",
            "--revs-only",
            "HEAD",
            "-x",
            "definitely-not-a-rev"
        ]),
        vec![sha.clone()]
    );
    // --no-revs: drop revisions, keep flags and non-rev paths.
    assert_eq!(
        lines(&["rev-parse", "--no-revs", "HEAD", "-x", "afile"]),
        vec!["-x".to_string(), "afile".to_string()]
    );
    // `--` terminates flag detection and is itself emitted as part of the path
    // output (Git emits it so an argv can be reconstructed): a following `-x` is
    // then a path.
    assert_eq!(
        lines(&["rev-parse", "--no-revs", "HEAD", "--", "-x"]),
        vec!["--".to_string(), "-x".to_string()]
    );
    // ...but `--revs-only` (which drops paths) drops the `--` too.
    assert_eq!(
        lines(&["rev-parse", "--revs-only", "HEAD", "--", "-x"]),
        vec![sha.clone()]
    );

    // `--default` supplies the single arg to classify when no positional is given.
    assert_eq!(
        lines(&["rev-parse", "--revs-only", "--default", "HEAD"]),
        vec![sha.clone()]
    );

    // --quiet suppresses filter output (exit 0, nothing printed).
    let quiet = run_libra_command(&["--quiet", "rev-parse", "--flags", "HEAD", "-x"], p);
    assert_cli_success(&quiet, "quiet filter");
    assert!(quiet.stdout.is_empty(), "--quiet prints nothing");
}

#[test]
fn test_rev_parse_json_single_object_multi_array_and_filter() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "branch feature",
    );

    let json = |args: &[&str]| -> serde_json::Value {
        let out = run_libra_command(args, p);
        assert_cli_success(&out, "rev-parse --json");
        serde_json::from_slice(&out.stdout).expect("valid JSON")
    };

    // Single spec: `data` is a single object (the unchanged contract).
    let single = json(&["--json", "rev-parse", "HEAD"]);
    assert!(
        single["data"].is_object(),
        "single-spec data is an object: {single}"
    );

    // Multiple specs: `data` is a JSON array (one entry per spec), not multiple
    // envelopes.
    let multi = json(&["--json", "rev-parse", "HEAD", "feature"]);
    assert!(
        multi["data"].is_array(),
        "multi-spec data is an array: {multi}"
    );
    assert_eq!(multi["data"].as_array().unwrap().len(), 2);

    // Filter mode: `data` is a JSON array of the filtered tokens.
    let filter = json(&["--json", "rev-parse", "--revs-only", "HEAD", "-x"]);
    assert!(
        filter["data"].is_array(),
        "filter data is an array: {filter}"
    );
    assert_eq!(
        filter["data"].as_array().unwrap().len(),
        1,
        "only the rev kept"
    );
}

#[test]
fn test_rev_parse_dashdash_separator() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "branch feature",
    );
    let sha = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();

    let lines = |args: &[&str]| -> Vec<String> {
        let out = run_libra_command(args, p);
        assert_cli_success(&out, "rev-parse --");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    };

    // A LEADING `--` (clap strips it, routing paths to a separate field): in
    // `--revs-only` the path is dropped (empty output); in `--no-revs` the `--`
    // and path are both emitted.
    assert!(lines(&["rev-parse", "--revs-only", "--", "HEAD"]).is_empty());
    assert_eq!(
        lines(&["rev-parse", "--no-revs", "--", "HEAD"]),
        vec!["--".to_string(), "HEAD".to_string()]
    );

    // After `--`, a real branch name is a PATH, not a revision: `--revs-only`
    // must DROP it (not re-resolve it to a commit).
    assert_eq!(
        lines(&["rev-parse", "--revs-only", "HEAD", "--", "feature"]),
        vec![sha.clone()]
    );
    assert_eq!(
        lines(&["rev-parse", "--no-revs", "HEAD", "--", "feature"]),
        vec!["--".to_string(), "feature".to_string()]
    );

    // Non-filter mode also splits at `--`: resolved revision, then the `--` and
    // the paths after it verbatim (matching `git rev-parse <rev> -- <path>`).
    assert_eq!(
        lines(&["rev-parse", "HEAD", "--", "file"]),
        vec![sha.clone(), "--".to_string(), "file".to_string()]
    );
    assert_eq!(
        lines(&["rev-parse", "--", "file"]),
        vec!["--".to_string(), "file".to_string()]
    );

    // A BARE leading `--` (no paths after) is discarded by clap; its presence is
    // recovered from the raw argv so the `--` still prints, matching git.
    assert_eq!(lines(&["rev-parse", "--"]), vec!["--".to_string()]);
    assert_eq!(
        lines(&["rev-parse", "HEAD", "--"]),
        vec![sha.clone(), "--".to_string()]
    );
    // The bare `--` under filters: `--revs-only` drops it, `--no-revs` keeps it.
    assert!(lines(&["rev-parse", "--revs-only", "--"]).is_empty());
    assert_eq!(
        lines(&["rev-parse", "--no-revs", "--"]),
        vec!["--".to_string()]
    );
}

#[test]
fn test_rev_parse_verify_requires_single_revision() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "branch feature",
    );

    // `--verify` with one spec succeeds; with multiple it fails (exit 128) like
    // git's "Needed a single revision".
    assert_cli_success(
        &run_libra_command(&["rev-parse", "--verify", "HEAD"], p),
        "verify one",
    );
    let multi = run_libra_command(&["rev-parse", "--verify", "HEAD", "feature"], p);
    assert_eq!(
        multi.status.code(),
        Some(128),
        "--verify with >1 revision exits 128: {}",
        String::from_utf8_lossy(&multi.stderr)
    );

    // The single-revision modes (`--verify`/`--short`) cannot be combined with
    // the output-filter flags — Git's behavior there is ill-defined, so Libra
    // rejects the combination with a usage error (exit 129).
    for args in [
        ["rev-parse", "--verify", "--revs-only", "HEAD"].as_slice(),
        ["rev-parse", "--short", "--no-revs", "HEAD"].as_slice(),
        ["rev-parse", "--short", "--revs-only", "HEAD", "feature"].as_slice(),
    ] {
        let out = run_libra_command(args, p);
        assert_eq!(
            out.status.code(),
            Some(129),
            "{args:?} (single-rev mode + filter) is a usage error: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // `--verify` requires EXACTLY one revision: zero revisions before a `--`
    // (only paths, or a bare `--`) is an error, like git's "Needed a single
    // revision".
    for args in [
        ["rev-parse", "--verify", "--", "file"].as_slice(),
        ["rev-parse", "--verify", "--"].as_slice(),
    ] {
        let out = run_libra_command(args, p);
        assert_eq!(
            out.status.code(),
            Some(128),
            "{args:?} (no revision) exits 128: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // `--verify <rev> -- <path>` verifies the one revision and prints ONLY its
    // object — never the post-`--` paths.
    let verify_path = run_libra_command(&["rev-parse", "--verify", "HEAD", "--", "file"], p);
    assert_cli_success(&verify_path, "verify with trailing path");
    let lines: Vec<&str> = std::str::from_utf8(&verify_path.stdout)
        .unwrap()
        .lines()
        .collect();
    assert_eq!(lines.len(), 1, "only the verified object prints: {lines:?}");
    assert!(
        !lines[0].contains("file"),
        "path is not echoed under --verify"
    );
}

#[test]
fn test_rev_parse_short_and_query_with_dashdash() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "branch feature",
    );

    // `--short` is a SINGLE-REVISION mode (like `--verify`): it prints only the
    // one abbreviated object and never the post-`--` paths.
    let short_path = run_libra_command(&["rev-parse", "--short", "HEAD", "--", "file"], p);
    assert_cli_success(&short_path, "--short HEAD -- file");
    let lines: Vec<&str> = std::str::from_utf8(&short_path.stdout)
        .unwrap()
        .lines()
        .collect();
    assert_eq!(lines.len(), 1, "--short prints only the object: {lines:?}");
    assert!(!lines[0].contains("file"));

    // `--short` requires EXACTLY one revision: zero (bare `--`) or more than one
    // is an error (exit 128).
    for args in [
        ["rev-parse", "--short", "--"].as_slice(),
        ["rev-parse", "--short", "HEAD", "feature"].as_slice(),
    ] {
        let out = run_libra_command(args, p);
        assert_eq!(
            out.status.code(),
            Some(128),
            "{args:?} is not a single revision: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Repository-query modes still print their value, THEN the `--`, when a
    // separator is present (git emits both).
    let toplevel_dd = run_libra_command(&["rev-parse", "--show-toplevel", "--"], p);
    assert_cli_success(&toplevel_dd, "--show-toplevel --");
    let lines: Vec<&str> = std::str::from_utf8(&toplevel_dd.stdout)
        .unwrap()
        .lines()
        .collect();
    assert_eq!(lines.len(), 2, "query value + separator: {lines:?}");
    assert_eq!(lines[1], "--");
}

#[test]
fn test_rev_parse_default_with_dashdash() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let sha = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    let lines = |args: &[&str]| -> Vec<String> {
        let out = run_libra_command(args, p);
        assert_cli_success(&out, "rev-parse --default --");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    };

    // `--default` supplies the revision when there are no revision args before the
    // separator — even with paths after `--` (matching git).
    assert_eq!(
        lines(&["rev-parse", "--default", "HEAD", "--", "file"]),
        vec![sha.clone(), "--".to_string(), "file".to_string()]
    );
    assert_eq!(
        lines(&[
            "rev-parse",
            "--revs-only",
            "--default",
            "HEAD",
            "--",
            "file"
        ]),
        vec![sha.clone()]
    );
    assert_eq!(
        lines(&["rev-parse", "--no-revs", "--default", "HEAD", "--", "file"]),
        vec!["--".to_string(), "file".to_string()]
    );

    // `--default` with a bare trailing `--` (no paths): the default resolves and
    // the `--` still prints.
    assert_eq!(
        lines(&["rev-parse", "--default", "HEAD", "--"]),
        vec![sha.clone(), "--".to_string()]
    );
}

// ── RV-01: `:<path>` / `:<n>:<path>` index-path specs (M-IDX I1–I8) ──

/// Assert a failing rev-parse produced `LBR-CLI-003` (exit 129) with the
/// expected explanation substring in stderr.
fn assert_invalid_index_spec(repo: &std::path::Path, spec: &str, explanation: &str) {
    let output = run_libra_command(&["rev-parse", spec], repo);
    let (stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(output.status.code(), Some(129), "{spec}: {stderr}");
    assert_eq!(report.error_code, "LBR-CLI-003", "{spec}: {stderr}");
    assert!(
        stderr.contains(explanation),
        "{spec}: expected {explanation:?} in stderr: {stderr}"
    );
}

#[test]
fn test_rev_parse_index_path_matrix() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    // I1: `:foo` returns the INDEX blob even when the worktree moved on.
    fs::write(p.join("tracked.txt"), "index version\n").expect("restage tracked.txt");
    let add = run_libra_command(&["add", "tracked.txt"], p);
    assert_cli_success(&add, "re-stage tracked.txt");
    let index_oid = run_libra_command(&["rev-parse", ":tracked.txt"], p);
    assert_cli_success(&index_oid, "I1 rev-parse :tracked.txt");
    let index_oid = String::from_utf8_lossy(&index_oid.stdout)
        .trim()
        .to_string();
    fs::write(p.join("tracked.txt"), "worktree version\n").expect("dirty worktree");
    let i1 = run_libra_command(&["rev-parse", ":tracked.txt"], p);
    assert_cli_success(&i1, "I1 rev-parse :tracked.txt (dirty worktree)");
    assert_eq!(
        String::from_utf8_lossy(&i1.stdout).trim(),
        index_oid,
        "I1: :tracked.txt must keep returning the index blob after a worktree edit"
    );
    // The returned OID is really the staged content, not the worktree edit.
    let blob = run_libra_command(&["cat-file", "-p", &index_oid], p);
    assert_cli_success(&blob, "I1 cat-file staged blob");
    assert_eq!(
        String::from_utf8_lossy(&blob.stdout),
        "index version\n",
        "I1: the resolved object must hold the INDEX content"
    );

    // I2: `--verify :foo` succeeds and prints the same OID.
    let i2 = run_libra_command(&["rev-parse", "--verify", ":tracked.txt"], p);
    assert_cli_success(&i2, "I2 rev-parse --verify :tracked.txt");
    assert_eq!(String::from_utf8_lossy(&i2.stdout).trim(), index_oid);

    // I3: `:0:foo` is `:foo`.
    let i3 = run_libra_command(&["rev-parse", ":0:tracked.txt"], p);
    assert_cli_success(&i3, "I3 rev-parse :0:tracked.txt");
    assert_eq!(String::from_utf8_lossy(&i3.stdout).trim(), index_oid);

    // I5: nested paths.
    fs::create_dir_all(p.join("sub")).expect("mkdir sub");
    fs::write(p.join("sub").join("s.txt"), "nested\n").expect("write sub/s.txt");
    let add = run_libra_command(&["add", "sub/s.txt"], p);
    assert_cli_success(&add, "add sub/s.txt");
    let commit = run_libra_command(&["commit", "-m", "add nested", "--no-verify"], p);
    assert_cli_success(&commit, "commit nested path");
    let i5 = run_libra_command(&["rev-parse", ":sub/s.txt"], p);
    assert_cli_success(&i5, "I5 rev-parse :sub/s.txt");
    let head_path = run_libra_command(&["rev-parse", "HEAD:sub/s.txt"], p);
    assert_cli_success(&head_path, "I5 rev-parse HEAD:sub/s.txt");
    assert_eq!(
        String::from_utf8_lossy(&i5.stdout).trim(),
        String::from_utf8_lossy(&head_path.stdout).trim(),
        "I5: :sub/s.txt and HEAD:sub/s.txt must resolve the same blob"
    );

    // I6: `./` / `../` resolve relative to the CURRENT directory.
    let sub = p.join("sub");
    let i6_here = run_libra_command(&["rev-parse", ":./s.txt"], &sub);
    assert_cli_success(&i6_here, "I6 rev-parse :./s.txt from sub/");
    assert_eq!(
        String::from_utf8_lossy(&i6_here.stdout).trim(),
        String::from_utf8_lossy(&i5.stdout).trim(),
        "I6: :./s.txt from sub/ must resolve sub/s.txt"
    );
    let i6_up = run_libra_command(&["rev-parse", ":../tracked.txt"], &sub);
    assert_cli_success(&i6_up, "I6 rev-parse :../tracked.txt from sub/");
    assert_eq!(
        String::from_utf8_lossy(&i6_up.stdout).trim(),
        index_oid,
        "I6: :../tracked.txt from sub/ must resolve tracked.txt"
    );

    // I7: a path absent from the index is LBR-CLI-003 / exit 129.
    assert_invalid_index_spec(p, ":not-in-index.txt", "is not in the index");

    // I4: `:1:` / `:2:` / `:3:` return the matching conflict stages.
    let blob_oid = |content: &str| -> String {
        let out = run_libra_command_with_stdin(&["hash-object", "-w", "--stdin"], p, content);
        assert_cli_success(&out, "hash-object -w --stdin");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let stages = [
        (1, blob_oid("stage 1\n"), "stage 1\n"),
        (2, blob_oid("stage 2\n"), "stage 2\n"),
        (3, blob_oid("stage 3\n"), "stage 3\n"),
    ];
    {
        use git_internal::{
            hash::HashKind,
            internal::index::{Index, IndexEntry},
        };
        let index_path = p.join(".libra/index");
        let mut index =
            Index::load_with_hash_kind(HashKind::Sha1, &index_path).expect("load index");
        for (stage, oid, content) in &stages {
            let hash: ObjectHash = oid.parse().expect("stage blob oid");
            let mut entry =
                IndexEntry::new_from_blob("conflict.txt".to_string(), hash, content.len() as u32);
            entry.flags.stage = *stage;
            index.add(entry);
        }
        index
            .save_with_hash_kind(HashKind::Sha1, &index_path)
            .expect("save conflicted index");
    }
    for (stage, oid, _) in &stages {
        let spec = format!(":{stage}:conflict.txt");
        let out = run_libra_command(&["rev-parse", &spec], p);
        assert_cli_success(&out, &format!("I4 rev-parse {spec}"));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            *oid,
            "I4: {spec} must return the stage-{stage} blob"
        );
    }

    // I8: stage 0 missing but higher stages present → LBR-CLI-003 with the
    // stage-0 explanation.
    assert_invalid_index_spec(p, ":conflict.txt", "in the index, but not at stage 0");
}

#[test]
fn test_cat_file_and_show_accept_index_path() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::write(p.join("tracked.txt"), "index content\n").expect("restage tracked.txt");
    let add = run_libra_command(&["add", "tracked.txt"], p);
    assert_cli_success(&add, "re-stage tracked.txt");

    let cat = run_libra_command(&["cat-file", "-p", ":tracked.txt"], p);
    assert_cli_success(&cat, "cat-file -p :tracked.txt");
    assert_eq!(
        String::from_utf8_lossy(&cat.stdout),
        "index content\n",
        "cat-file -p :tracked.txt must print the index content"
    );

    let show = run_libra_command(&["show", ":tracked.txt"], p);
    assert_cli_success(&show, "show :tracked.txt");
    assert!(
        String::from_utf8_lossy(&show.stdout).contains("index content\n"),
        "show :tracked.txt must print the index content: {}",
        String::from_utf8_lossy(&show.stdout)
    );
}

// ── RV-02: `:/<regex>` and `<rev>^{/<regex>}` message search (M-MSG M1–M9) ──

#[test]
fn test_rev_parse_message_search_matrix() {
    let repo = tempdir().expect("tempdir");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    let p = repo.path();

    // Controlled dates keep the newest-first walk deterministic:
    //   t1 "base: no match"          (main, root)
    //   t2 "common-old"              (main)
    //   t3 "side-only !boom marker"  (side branch from t1; not on main)
    //   t4 "common-new"              (main, HEAD)
    let commit = |file: &str, content: &str, message: &str, stamp: &str| {
        fs::write(p.join(file), content).expect("write commit file");
        let add = run_libra_command(&["add", file], p);
        assert_cli_success(&add, &format!("add {file}"));
        let env = [("GIT_COMMITTER_DATE", stamp), ("GIT_AUTHOR_DATE", stamp)];
        let out = run_libra_command_with_env(&["commit", "-m", message, "--no-verify"], p, &env);
        assert_cli_success(&out, &format!("commit {message}"));
    };
    let oid_of = |spec: &str| -> String {
        let out = run_libra_command(&["rev-parse", spec], p);
        assert_cli_success(&out, &format!("rev-parse {spec}"));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    commit(
        "base.txt",
        "base\n",
        "base: no match",
        "2020-01-01T00:00:00Z",
    );
    let base = oid_of("HEAD");
    commit("old.txt", "old\n", "common-old", "2020-01-02T00:00:00Z");
    let common_old = oid_of("HEAD");
    let branch = run_libra_command(&["branch", "side"], p);
    assert_cli_success(&branch, "branch side");
    commit(
        "side.txt",
        "side\n",
        "side-only !boom marker",
        "2020-01-03T00:00:00Z",
    );
    let side_tip = oid_of("HEAD");
    let switch = run_libra_command(&["switch", "main"], p);
    assert_cli_success(&switch, "switch back to main");
    commit("new.txt", "new\n", "common-new", "2020-01-04T00:00:00Z");
    let common_new = oid_of("HEAD");
    assert_ne!(base, common_old);
    assert_ne!(common_old, side_tip);
    assert_ne!(side_tip, common_new);

    // M1 / M2: newer of the two matching commits wins for `:/` and `^{/}`.
    assert_eq!(oid_of(":/common"), common_new, "M1: :/common");
    assert_eq!(oid_of("HEAD^{/common}"), common_new, "M2: HEAD^{{/common}}");

    // M3: `!-` negates: the newest commit whose message does NOT match.
    assert_eq!(oid_of(":/!-common"), side_tip, "M3: :/!-common");

    // M4: `!!` keeps a literal leading `!` in the pattern.
    assert_eq!(oid_of(":/!!boom"), side_tip, "M4: :/!!boom");

    // M5: a bare leading `!` is an error.
    assert_invalid_index_spec(p, ":/!x", "leading '!'");
    // M6: an invalid regex is an error.
    assert_invalid_index_spec(p, ":/(unclosed", "invalid message-search pattern");
    // M7: no matching commit is an error.
    assert_invalid_index_spec(p, ":/no-such-message-anywhere", "invalid reference");

    // M8: a commit reachable only from a non-current branch is findable.
    assert_eq!(
        oid_of(":/side-only"),
        side_tip,
        "M8: :/side-only must find the side-branch-only commit from main"
    );

    // M9: the result is a commit-ish usable by `log` and `show`.
    let log = run_libra_command(&["log", ":/common"], p);
    assert_cli_success(&log, "M9 log :/common");
    assert!(
        String::from_utf8_lossy(&log.stdout).contains(&common_new),
        "M9: log :/common must print the newer matching commit: {}",
        String::from_utf8_lossy(&log.stdout)
    );
    let show = run_libra_command(&["show", ":/common"], p);
    assert_cli_success(&show, "M9 show :/common");
    assert!(
        String::from_utf8_lossy(&show.stdout).contains(&common_new),
        "M9: show :/common must print the newer matching commit: {}",
        String::from_utf8_lossy(&show.stdout)
    );
}

#[test]
fn test_rev_parse_resolves_describe_suffix() {
    use crate::command::{assert_cli_success, run_libra_command};

    let repo = tempfile::tempdir().expect("repo");
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["init"], p), "init");
    run_libra_command(&["config", "set", "user.name", "t"], p);
    run_libra_command(&["config", "set", "user.email", "t@t"], p);
    std::fs::write(p.join("f.txt"), "one\n").expect("write f");
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    let c1 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    let short = &c1[..8];
    assert_cli_success(&run_libra_command(&["tag", "v1"], p), "tag v1");

    // A second commit for the literal-precedence case.
    std::fs::write(p.join("f.txt"), "two\n").expect("write f2");
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "stage f2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );
    let c2 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();

    let resolve = |spec: &str| {
        let out = run_libra_command(&["rev-parse", spec], p);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    // AC1: `<tag>-<count>-g<abbrev>` names the abbreviated commit.
    assert_eq!(resolve(&format!("v1-1-g{short}")), c1, "AC1");

    // AC2: the count may contain hex letters and the trailing id may be full.
    assert_eq!(resolve(&format!("x-1a-g{c1}")), c1, "AC2");

    // AC3: an existing ref with that exact name wins over the suffix parse.
    assert_cli_success(
        &run_libra_command(&["branch", &format!("v1-1-g{short}"), &c2], p),
        "branch with a describe-shaped name",
    );
    assert_eq!(
        resolve(&format!("v1-1-g{short}")),
        c2,
        "AC3: the literal branch wins"
    );
}

#[test]
fn describe_suffix_missing_and_ambiguous() {
    use crate::command::{assert_cli_success, run_libra_command};

    let repo = tempfile::tempdir().expect("repo");
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["init"], p), "init");
    run_libra_command(&["config", "set", "user.name", "t"], p);
    run_libra_command(&["config", "set", "user.email", "t@t"], p);
    std::fs::write(p.join("f.txt"), "one\n").expect("write f");
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    let c1 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();

    let fails = |spec: &str, label: &str| {
        let out = run_libra_command(&["rev-parse", spec], p);
        assert!(!out.status.success(), "{label}: '{spec}' must fail");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-003"),
            "{label}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    // AC1/AC2/AC6/AC7: missing, non-hex and too-short suffixes all fail.
    fails("x-1-g00000000", "AC1");
    fails("x-1-g0000000000000000000000000000000000000000", "AC2");
    fails("x-1-gzzzzzzzz", "AC6");
    fails(&format!("x-1-g{}", &c1[..3]), "AC7");
}

#[test]
fn describe_suffix_tag_object() {
    use crate::command::{assert_cli_success, run_libra_command};

    let repo = tempfile::tempdir().expect("repo");
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["init"], p), "init");
    run_libra_command(&["config", "set", "user.name", "t"], p);
    run_libra_command(&["config", "set", "user.email", "t@t"], p);
    std::fs::write(p.join("f.txt"), "one\n").expect("write f");
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "v1", "v1"], p),
        "annotated v1",
    );
    let tag_oid =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "refs/tags/v1"], p).stdout)
            .trim()
            .to_string();
    let commit_oid = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    assert_ne!(tag_oid, commit_oid, "the annotated tag is its own object");
    let short = &tag_oid[..8];

    let resolve = |spec: &str| run_libra_command(&["rev-parse", spec], p);

    // AC1: the abbreviated suffix resolves to the TAG OBJECT, not its commit.
    let out = resolve(&format!("x-1-g{short}"));
    assert_cli_success(&out, "AC1: abbreviated tag object");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        tag_oid,
        "AC1: the tag object itself, unpeeled"
    );

    // AC2: same for the full object id.
    let out = resolve(&format!("x-1-g{tag_oid}"));
    assert_cli_success(&out, "AC2: full tag object");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        tag_oid,
        "AC2: the tag object itself, unpeeled"
    );
}

#[test]
fn describe_suffix_blob_and_tree_objects() {
    use crate::command::{assert_cli_success, run_libra_command};

    let repo = tempfile::tempdir().expect("repo");
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["init"], p), "init");
    run_libra_command(&["config", "set", "user.name", "t"], p);
    run_libra_command(&["config", "set", "user.email", "t@t"], p);
    std::fs::write(p.join("f.txt"), "one\n").expect("write f");
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    let rev = |spec: &str| {
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", spec], p).stdout)
            .trim()
            .to_string()
    };
    let blob = rev("HEAD:f.txt");
    let tree = rev("HEAD^{tree}");
    assert!(
        !blob.is_empty() && !tree.is_empty() && blob != tree,
        "fixture ids"
    );

    let resolve = |spec: &str| {
        let out = run_libra_command(&["rev-parse", spec], p);
        assert_cli_success(&out, "describe suffix resolution");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    // AC1/AC2: blob suffixes (abbreviated and full).
    assert_eq!(resolve(&format!("x-1-g{}", &blob[..8])), blob, "AC1");
    assert_eq!(resolve(&format!("x-1-g{blob}")), blob, "AC2");

    // AC3/AC4: tree suffixes (abbreviated and full).
    assert_eq!(resolve(&format!("x-1-g{}", &tree[..8])), tree, "AC3");
    assert_eq!(resolve(&format!("x-1-g{tree}")), tree, "AC4");
}

#[test]
fn describe_suffix_inside_the_tags_namespace() {
    use crate::command::{assert_cli_success, run_libra_command};

    let repo = tempfile::tempdir().expect("repo");
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["init"], p), "init");
    run_libra_command(&["config", "set", "user.name", "t"], p);
    run_libra_command(&["config", "set", "user.email", "t@t"], p);
    std::fs::write(p.join("f.txt"), "one\n").expect("write f");
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    let c1 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    std::fs::write(p.join("f.txt"), "two\n").expect("write f2");
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "stage f2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );

    let short = &c1[..8];
    let resolve = |spec: &str| {
        let out = run_libra_command(&["rev-parse", spec], p);
        assert_cli_success(&out, "describe suffix in the tags namespace");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    // AC1/AC2: a `tags/`-prefixed describe suffix resolves to the commit.
    assert_eq!(resolve(&format!("tags/x-1-g{short}")), c1, "AC1");
    assert_eq!(resolve(&format!("tags/x-1-g{c1}")), c1, "AC2");

    // AC3: a literal tag with that exact name wins over the suffix parse.
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "t", &format!("tlit-1-g{short}")], p),
        "literal tag",
    );
    let literal = resolve(&format!("tags/tlit-1-g{short}"));
    assert_ne!(
        literal, c1,
        "AC3: the literal tag wins over the suffix object"
    );

    // AC5: the describe atom composes with later revision operators.
    assert_cli_success(
        &run_libra_command(&["tag", "v-parent"], p),
        "tag for the parented commit",
    );
    let head = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    assert_eq!(
        resolve(&format!("tags/x-1-g{head}~1")),
        c1,
        "AC5: the suffix atom is peeled one commit back"
    );

    // AC7: the complete-name lookup also reaches remote-tracking branches.
    let remote_dir = tempfile::tempdir().expect("remote root");
    let remote_path = remote_dir.path().join("tags-remote");
    std::fs::create_dir_all(&remote_path).expect("mkdir remote");
    assert_cli_success(&run_libra_command(&["init"], &remote_path), "init remote");
    run_libra_command(&["config", "set", "user.name", "t"], &remote_path);
    run_libra_command(&["config", "set", "user.email", "t@t"], &remote_path);
    std::fs::write(remote_path.join("g.txt"), "g\n").expect("write g");
    assert_cli_success(
        &run_libra_command(&["add", "g.txt"], &remote_path),
        "stage g",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "sc", "--no-verify"], &remote_path),
        "commit sc",
    );
    let sc =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], &remote_path).stdout)
            .trim()
            .to_string();
    assert_cli_success(
        &run_libra_command(&["branch", &format!("dwr-1-g{short}"), &sc], &remote_path),
        "branch with a describe-shaped name on the remote",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "remote",
                "add",
                "tags",
                remote_path.to_str().expect("remote"),
            ],
            p,
        ),
        "add the tags remote",
    );
    assert_cli_success(&run_libra_command(&["fetch", "tags"], p), "fetch tags");
    assert_eq!(
        resolve(&format!("tags/dwr-1-g{short}")),
        sc,
        "AC7: the remote-tracking branch wins the complete-name lookup"
    );

    // AC6: a local branch literally named `tags/<describe>` is found by the
    // complete-name lookup before the suffix parse.
    assert_cli_success(
        &run_libra_command(&["branch", &format!("tags/dw-1-g{short}"), &c1], p),
        "literal branch",
    );
    assert_eq!(
        resolve(&format!("tags/dw-1-g{short}")),
        c1,
        "AC6: the complete-name branch lookup wins"
    );
}

#[test]
fn describe_suffix_inside_the_heads_namespace() {
    use crate::command::{assert_cli_success, run_libra_command};

    let repo = tempfile::tempdir().expect("repo");
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["init"], p), "init");
    run_libra_command(&["config", "set", "user.name", "t"], p);
    run_libra_command(&["config", "set", "user.email", "t@t"], p);
    std::fs::write(p.join("f.txt"), "one\n").expect("write f");
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    let c1 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    std::fs::write(p.join("f.txt"), "two\n").expect("write f2");
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "stage f2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );
    let short = &c1[..8];
    let resolve = |spec: &str| {
        let out = run_libra_command(&["rev-parse", spec], p);
        assert_cli_success(&out, "describe suffix in the heads namespace");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    // AC1/AC2: `heads/`-prefixed describe suffixes resolve to the commit.
    assert_eq!(resolve(&format!("heads/x-1-g{short}")), c1, "AC1");
    assert_eq!(resolve(&format!("heads/x-1-g{c1}")), c1, "AC2");

    // AC3: a literal local branch with that exact name wins.
    assert_cli_success(
        &run_libra_command(&["branch", &format!("blit-1-g{short}"), &c1], p),
        "literal branch",
    );
    assert_eq!(
        resolve(&format!("heads/blit-1-g{short}")),
        c1,
        "AC3: the literal branch wins"
    );

    // AC5: a lightweight tag with that complete name is found for a commit the
    // branch does not point at.
    assert_cli_success(
        &run_libra_command(&["tag", &format!("dw-1-g{short}")], p),
        "lightweight tag",
    );
    assert_eq!(
        resolve(&format!("heads/dw-1-g{short}")),
        c1,
        "AC5: the complete-name tag lookup wins"
    );
}

#[test]
fn describe_suffix_inside_the_remotes_namespace() {
    use crate::command::{assert_cli_success, run_libra_command};

    let repo = tempfile::tempdir().expect("repo");
    let p = repo.path();
    assert_cli_success(&run_libra_command(&["init"], p), "init");
    run_libra_command(&["config", "set", "user.name", "t"], p);
    run_libra_command(&["config", "set", "user.email", "t@t"], p);
    std::fs::write(p.join("f.txt"), "one\n").expect("write f");
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "stage");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    let c1 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    std::fs::write(p.join("f.txt"), "two\n").expect("write f2");
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "stage f2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );
    let short = &c1[..8];
    let resolve = |spec: &str| {
        let out = run_libra_command(&["rev-parse", spec], p);
        assert_cli_success(&out, "describe suffix in the remotes namespace");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    // AC1/AC2: `remotes/`-prefixed describe suffixes resolve to the commit.
    assert_eq!(resolve(&format!("remotes/x-1-g{short}")), c1, "AC1");
    assert_eq!(resolve(&format!("remotes/x-1-g{c1}")), c1, "AC2");

    // AC3: an existing remote-tracking ref with that exact name wins.
    let remote_dir = tempfile::tempdir().expect("remote root");
    let remote_path = remote_dir.path().join("origin-remote");
    std::fs::create_dir_all(&remote_path).expect("mkdir remote");
    assert_cli_success(&run_libra_command(&["init"], &remote_path), "init remote");
    run_libra_command(&["config", "set", "user.name", "t"], &remote_path);
    run_libra_command(&["config", "set", "user.email", "t@t"], &remote_path);
    std::fs::write(remote_path.join("g.txt"), "g\\n").expect("write g");
    assert_cli_success(
        &run_libra_command(&["add", "g.txt"], &remote_path),
        "stage g",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "sc", "--no-verify"], &remote_path),
        "commit sc",
    );
    let sc =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], &remote_path).stdout)
            .trim()
            .to_string();
    assert_cli_success(
        &run_libra_command(&["branch", &format!("rlit-1-g{short}"), &sc], &remote_path),
        "describe-shaped branch on the remote",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "remote",
                "add",
                "origin",
                remote_path.to_str().expect("remote"),
            ],
            p,
        ),
        "add origin",
    );
    assert_cli_success(&run_libra_command(&["fetch", "origin"], p), "fetch origin");
    assert_eq!(
        resolve(&format!("remotes/origin/rlit-1-g{short}")),
        sc,
        "AC3: the literal remote-tracking branch wins"
    );
}
