//! CLI-level tests for the `describe` command.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use super::*;

#[test]
fn test_describe_json_returns_tag_match() {
    let repo = create_committed_repo_via_cli();

    let tag_output = run_libra_command(&["tag", "-m", "Release v1.0", "v1.0"], repo.path());
    assert_cli_success(&tag_output, "failed to create tag for describe test");

    let output = run_libra_command(&["describe", "--json"], repo.path());
    assert_cli_success(&output, "describe --json should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "describe");
    assert_eq!(json["data"]["result"], "v1.0");
    assert_eq!(json["data"]["tag"], "v1.0");
    assert_eq!(json["data"]["distance"], 0);
    assert_eq!(json["data"]["used_always"], false);
}

#[test]
fn test_describe_tags_json_includes_lightweight_tag() {
    let repo = create_committed_repo_via_cli();

    let tag_output = run_libra_command(&["tag", "v1.0"], repo.path());
    assert_cli_success(
        &tag_output,
        "failed to create lightweight tag for describe test",
    );

    let output = run_libra_command(&["describe", "--tags", "--json"], repo.path());
    assert_cli_success(&output, "describe --tags --json should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "describe");
    assert_eq!(json["data"]["result"], "v1.0");
    assert_eq!(json["data"]["tag"], "v1.0");
    assert_eq!(json["data"]["distance"], 0);
    assert_eq!(json["data"]["used_always"], false);
}

#[test]
fn test_describe_always_json_without_tags_returns_abbrev_commit() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["describe", "--always", "--json"], repo.path());
    assert_cli_success(&output, "describe --always --json should succeed");

    let json = parse_json_stdout(&output);
    let result = json["data"]["result"]
        .as_str()
        .expect("result should be a string");
    assert_eq!(
        result.len(),
        7,
        "default abbreviated commit length should be 7"
    );
    assert!(json["data"]["tag"].is_null());
    assert_eq!(json["data"]["used_always"], true);
}

#[test]
fn test_describe_always_respects_explicit_abbrev_length() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(
        &["describe", "--always", "--abbrev=5", "--json"],
        repo.path(),
    );
    assert_cli_success(
        &output,
        "describe --always --abbrev=5 --json should succeed",
    );

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["result"].as_str().unwrap().len(), 5);
    assert_eq!(
        json["data"]["abbreviated_commit"].as_str().unwrap().len(),
        5
    );
    assert_eq!(json["data"]["used_always"], true);
}

#[test]
fn test_describe_always_abbrev_zero_returns_full_hash() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(
        &["describe", "--always", "--abbrev=0", "--json"],
        repo.path(),
    );
    assert_cli_success(
        &output,
        "describe --always --abbrev=0 --json should succeed",
    );

    let json = parse_json_stdout(&output);
    let resolved_commit = json["data"]["resolved_commit"]
        .as_str()
        .expect("resolved_commit should be a string");
    assert_eq!(json["data"]["result"], resolved_commit);
    assert_eq!(json["data"]["abbreviated_commit"], resolved_commit);
    assert_eq!(json["data"]["used_always"], true);
}

#[test]
fn test_describe_abbrev_zero_forces_suffix_with_full_oid() {
    let repo = create_committed_repo_via_cli();

    let tag_output = run_libra_command(&["tag", "-m", "Release v1.0", "v1.0"], repo.path());
    assert_cli_success(&tag_output, "failed to create tag for describe test");

    std::fs::write(repo.path().join("tracked.txt"), "tracked\nnext\n")
        .expect("failed to update tracked file");
    let add_output = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert_cli_success(&add_output, "failed to stage updated tracked file");
    let commit_output = run_libra_command(&["commit", "-m", "next", "--no-verify"], repo.path());
    assert_cli_success(&commit_output, "failed to create second commit");

    let head =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], repo.path()).stdout)
            .trim()
            .to_string();
    let output = run_libra_command(&["describe", "--abbrev=0", "--json"], repo.path());
    assert_cli_success(&output, "describe --abbrev=0 --json should succeed");

    let json = parse_json_stdout(&output);
    // DESC-05: `--abbrev=0` forces the `-<n>-g<oid>` suffix with the FULL oid
    // (Git's repo_find_unique_abbrev(oid, 0)) instead of suppressing it.
    assert_eq!(json["data"]["result"], format!("v1.0-1-g{head}"));
    assert_eq!(json["data"]["tag"], "v1.0");
    assert_eq!(json["data"]["distance"], 1);
    assert_eq!(json["data"]["abbreviated_commit"], head);
    assert_eq!(json["data"]["used_always"], false);
}

#[test]
fn test_describe_tags_prefers_annotated_tag_over_lightweight_tag() {
    let repo = create_committed_repo_via_cli();

    let lightweight = run_libra_command(&["tag", "v-light"], repo.path());
    assert_cli_success(&lightweight, "failed to create lightweight tag");

    let annotated = run_libra_command(&["tag", "-m", "Release v-ann", "v-ann"], repo.path());
    assert_cli_success(&annotated, "failed to create annotated tag");

    let output = run_libra_command(&["describe", "--tags", "--json"], repo.path());
    assert_cli_success(&output, "describe --tags --json should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["result"], "v-ann");
    assert_eq!(json["data"]["tag"], "v-ann");
    assert_eq!(json["data"]["distance"], 0);
}

#[test]
fn test_describe_lowercase_head_resolves_named_ref_not_head() {
    let repo = create_committed_repo_via_cli();

    let first_tag = run_libra_command(&["tag", "-m", "Release v1", "v1"], repo.path());
    assert_cli_success(&first_tag, "failed to create first describe tag");

    let branch_output = run_libra_command(&["branch", "head"], repo.path());
    assert_cli_success(&branch_output, "failed to create lowercase head branch");

    std::fs::write(repo.path().join("tracked.txt"), "tracked\nnext\n")
        .expect("failed to update tracked file");
    let add_output = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert_cli_success(&add_output, "failed to stage updated tracked file");
    let commit_output = run_libra_command(&["commit", "-m", "next", "--no-verify"], repo.path());
    assert_cli_success(&commit_output, "failed to create second commit");

    let second_tag = run_libra_command(&["tag", "-m", "Release v2", "v2"], repo.path());
    assert_cli_success(&second_tag, "failed to create second describe tag");

    let current_output = run_libra_command(&["describe", "--json"], repo.path());
    assert_cli_success(&current_output, "describe --json should succeed");
    let current_json = parse_json_stdout(&current_output);
    assert_eq!(current_json["data"]["result"], "v2");

    let output = run_libra_command(&["describe", "head", "--json"], repo.path());
    assert_cli_success(&output, "describe head --json should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["result"], "v1");
    assert_eq!(json["data"]["tag"], "v1");
    assert_eq!(json["data"]["distance"], 0);
    assert_eq!(json["data"]["exact_match"], true);
}

#[test]
fn test_describe_invalid_reference_returns_cli_invalid_target() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["describe", "missing-ref"], repo.path());
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(129));
    assert_eq!(report.error_code, "LBR-CLI-003");
}

#[test]
fn test_describe_exact_match_fails_after_head_moves_past_tag() {
    let repo = create_committed_repo_via_cli();

    let tag_output = run_libra_command(&["tag", "-m", "Release v1.0", "v1.0"], repo.path());
    assert_cli_success(&tag_output, "failed to create tag for describe test");

    std::fs::write(repo.path().join("tracked.txt"), "tracked\nnext\n")
        .expect("failed to update tracked file");
    let add_output = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert_cli_success(&add_output, "failed to stage updated tracked file");
    let commit_output = run_libra_command(&["commit", "-m", "next", "--no-verify"], repo.path());
    assert_cli_success(&commit_output, "failed to create second commit");

    let output = run_libra_command(&["describe", "--exact-match"], repo.path());
    let (human, report) = parse_cli_error_stderr(&output.stderr);

    assert_eq!(output.status.code(), Some(128));
    assert_eq!(report.error_code, "LBR-REPO-003");
    assert!(
        human.contains("no tag exactly matches"),
        "exact-match failure should explain why describe failed: {human}"
    );
}

#[test]
fn test_describe_exact_match_succeeds_on_tagged_head() {
    let repo = create_committed_repo_via_cli();

    let tag_output = run_libra_command(&["tag", "-m", "Release v1.0", "v1.0"], repo.path());
    assert_cli_success(&tag_output, "failed to create tag for describe test");

    let output = run_libra_command(&["describe", "--exact-match", "--json"], repo.path());
    assert_cli_success(&output, "describe --exact-match --json should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["result"], "v1.0");
    assert_eq!(json["data"]["exact_match"], true);
    assert_eq!(json["data"]["dirty"], false);
}

#[test]
fn test_describe_dirty_appends_default_suffix_for_tracked_changes() {
    let repo = create_committed_repo_via_cli();

    let tag_output = run_libra_command(&["tag", "-m", "Release v1.0", "v1.0"], repo.path());
    assert_cli_success(&tag_output, "failed to create tag for describe test");
    std::fs::write(repo.path().join("tracked.txt"), "tracked\nchanged\n")
        .expect("failed to dirty tracked file");

    let output = run_libra_command(&["describe", "--dirty", "--json"], repo.path());
    assert_cli_success(&output, "describe --dirty --json should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["result"], "v1.0-dirty");
    assert_eq!(json["data"]["dirty"], true);
    assert_eq!(json["data"]["dirty_mark"], "-dirty");
}

#[test]
fn test_describe_dirty_ignores_untracked_files_like_git() {
    let repo = create_committed_repo_via_cli();

    let tag_output = run_libra_command(&["tag", "-m", "Release v1.0", "v1.0"], repo.path());
    assert_cli_success(&tag_output, "failed to create tag for describe test");
    std::fs::write(repo.path().join("untracked.txt"), "untracked\n")
        .expect("failed to write untracked file");

    let output = run_libra_command(&["describe", "--dirty", "--json"], repo.path());
    assert_cli_success(&output, "describe --dirty --json should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["result"], "v1.0");
    assert_eq!(json["data"]["dirty"], false);
    assert!(json["data"]["dirty_mark"].is_null());
}

#[test]
fn test_describe_dirty_accepts_custom_suffix() {
    let repo = create_committed_repo_via_cli();

    let tag_output = run_libra_command(&["tag", "-m", "Release v1.0", "v1.0"], repo.path());
    assert_cli_success(&tag_output, "failed to create tag for describe test");
    std::fs::write(repo.path().join("tracked.txt"), "tracked\nchanged\n")
        .expect("failed to dirty tracked file");

    let output = run_libra_command(&["describe", "--dirty=-worktree", "--json"], repo.path());
    assert_cli_success(&output, "describe --dirty=-worktree --json should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["result"], "v1.0-worktree");
    assert_eq!(json["data"]["dirty"], true);
    assert_eq!(json["data"]["dirty_mark"], "-worktree");
}

/// `--match` keeps only tags whose name matches the glob; a non-matching tag that
/// would otherwise win the lexicographic tie-break is filtered out.
#[test]
fn test_describe_match_single_glob() {
    let repo = create_committed_repo_via_cli();
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "rel", "a-other"], repo.path()),
        "tag a-other",
    );
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "rel", "v1.0"], repo.path()),
        "tag v1.0",
    );

    // Without --match the lexicographic tie-break picks "a-other"; --match forces v1.0.
    let output = run_libra_command(&["describe", "--match", "v1.*", "--json"], repo.path());
    assert_cli_success(&output, "describe --match v1.* should succeed");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["tag"], "v1.0");
    assert_eq!(json["data"]["result"], "v1.0");
}

/// Multiple `--match` globs are OR-combined: matching any one is enough.
#[test]
fn test_describe_match_multiple_globs_or() {
    let repo = create_committed_repo_via_cli();
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "rel", "a-other"], repo.path()),
        "tag a-other",
    );
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "rel", "v1.0"], repo.path()),
        "tag v1.0",
    );

    let output = run_libra_command(
        &["describe", "--match", "x*", "--match", "v1.*", "--json"],
        repo.path(),
    );
    assert_cli_success(&output, "describe with two --match globs should succeed");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["tag"], "v1.0");
}

/// `--exclude` removes a matched tag even when it would otherwise win.
#[test]
fn test_describe_exclude_filters_matched_tag() {
    let repo = create_committed_repo_via_cli();
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "rel", "a-rc"], repo.path()),
        "tag a-rc",
    );
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "rel", "v1.0"], repo.path()),
        "tag v1.0",
    );

    // Without exclude the tie-break would pick "a-rc"; --exclude removes it.
    let output = run_libra_command(&["describe", "--exclude", "*rc*", "--json"], repo.path());
    assert_cli_success(&output, "describe --exclude *rc* should succeed");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["tag"], "v1.0");
}

/// `--match` and `--exclude` combine: exclude takes precedence over match.
#[test]
fn test_describe_match_and_exclude_combined() {
    let repo = create_committed_repo_via_cli();
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "rel", "v1.0-rc"], repo.path()),
        "tag v1.0-rc",
    );
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "rel", "v1.0"], repo.path()),
        "tag v1.0",
    );

    let output = run_libra_command(
        &["describe", "--match", "v1.*", "--exclude", "*rc*", "--json"],
        repo.path(),
    );
    assert_cli_success(&output, "describe --match + --exclude should succeed");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["tag"], "v1.0");
}

/// A glob longer than 256 chars is rejected at the parsing stage (129).
#[test]
fn test_describe_glob_over_256_chars_rejected() {
    let repo = create_committed_repo_via_cli();
    let pattern = "a".repeat(257);
    let output = run_libra_command(&["describe", "--match", &pattern], repo.path());
    assert_eq!(
        output.status.code(),
        Some(129),
        "overlong glob should be rejected: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
}

/// A malformed glob pattern is rejected with a usage error rather than panicking.
#[test]
fn test_describe_invalid_glob_rejected() {
    let repo = create_committed_repo_via_cli();
    let output = run_libra_command(&["describe", "--match", "v{1"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(129),
        "invalid glob should be rejected: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
}

/// `--first-parent` is accepted and produces a normal description on a linear
/// history (where it has no effect beyond the default walk).
#[test]
fn test_describe_first_parent_linear_history() {
    let repo = create_committed_repo_via_cli();
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "rel", "v1.0"], repo.path()),
        "tag v1.0",
    );

    let output = run_libra_command(&["describe", "--first-parent", "--json"], repo.path());
    assert_cli_success(&output, "describe --first-parent should succeed");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["tag"], "v1.0");
    assert_eq!(json["data"]["result"], "v1.0");
}

#[test]
fn test_describe_candidates_zero_requires_exact_match() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    let tag = run_libra_command(&["tag", "-m", "Release v1.0", "v1.0"], p);
    assert_cli_success(&tag, "create annotated tag v1.0");

    // HEAD is exactly tagged: `--candidates 0` succeeds with the exact match
    // (Git documents `--candidates 0` as "only exact matches").
    let output = run_libra_command(&["describe", "--candidates", "0"], p);
    assert_cli_success(&output, "describe --candidates 0 on exact tag");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "v1.0");

    // Advance HEAD past the tag.
    std::fs::write(p.join("tracked.txt"), "next\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], p),
        "add tracked.txt",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "next", "--no-verify"], p),
        "commit next",
    );

    // `--candidates 0` now fails (no exact match), exactly like `--exact-match`.
    let output = run_libra_command(&["describe", "--candidates", "0"], p);
    assert!(
        !output.status.success(),
        "describe --candidates 0 should fail when HEAD is not exactly tagged"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no tag exactly matches"),
        "expected exact-match failure: {stderr}"
    );

    // `--candidates 5` (>=1) keeps the normal behavior: the nearest tag.
    let output = run_libra_command(&["describe", "--tags", "--candidates", "5", "--json"], p);
    assert_cli_success(&output, "describe --candidates 5");
    let json = parse_json_stdout(&output);
    assert_eq!(json["data"]["tag"], "v1.0");

    // A non-integer value is rejected at the clap layer.
    let output = run_libra_command(&["describe", "--candidates", "abc"], p);
    assert!(
        !output.status.success(),
        "describe --candidates abc should be rejected"
    );
}

#[test]
fn test_describe_all_uses_branches_and_tags_with_prefix() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert!(
        run_libra_command(&["tag", "-m", "release", "v1"], p)
            .status
            .success(),
        "create annotated tag"
    );

    // At the tagged commit both a tag and the branch tip point here; under
    // --all tags take precedence and are shown with the `tags/` prefix.
    let at_tag = run_libra_command(&["describe", "--all"], p);
    assert!(at_tag.status.success(), "describe --all at tag");
    let s = String::from_utf8_lossy(&at_tag.stdout);
    assert_eq!(s.trim(), "tags/v1", "expected tags/v1, got: {s:?}");

    // Advance the branch one commit; now its tip is the nearest ref and is
    // shown with the `heads/` prefix.
    assert!(
        run_libra_command(&["commit", "--allow-empty", "-m", "c1", "--no-verify"], p)
            .status
            .success(),
        "advance branch"
    );
    let at_branch = run_libra_command(&["describe", "--all"], p);
    assert!(at_branch.status.success(), "describe --all at branch tip");
    let b = String::from_utf8_lossy(&at_branch.stdout);
    assert!(
        b.trim().starts_with("heads/"),
        "expected a heads/ ref at the branch tip, got: {b:?}"
    );
}

/// `describe --contains` names a commit relative to the nearest descendant tag
/// (git name-rev): the tagged commit itself prints the tag, and ancestors print
/// `<tag>~<n>`. A commit with no descendant tag cannot be named and fails.
#[test]
fn test_describe_contains_names_relative_to_descendant_tag() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let head = |p: &std::path::Path| -> String {
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
            .trim()
            .to_string()
    };

    let c0 = head(p); // the base commit
    std::fs::write(p.join("a.txt"), "a").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add a");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "c1",
    );
    let c1 = head(p);
    std::fs::write(p.join("b.txt"), "b").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.txt"], p), "add b");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "c2",
    );
    let c2 = head(p);

    // Tag the tip (C2).
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "release", "v1.0"], p),
        "tag v1.0",
    );

    // The tagged commit prints the tag itself; ancestors print <tag>~<n>.
    let r2 = run_libra_command(&["describe", "--contains", &c2], p);
    assert_cli_success(&r2, "describe --contains c2");
    assert_eq!(String::from_utf8_lossy(&r2.stdout).trim(), "v1.0^0");
    let r1 = run_libra_command(&["describe", "--contains", &c1], p);
    assert_cli_success(&r1, "describe --contains c1");
    assert_eq!(String::from_utf8_lossy(&r1.stdout).trim(), "v1.0~1");
    let r0 = run_libra_command(&["describe", "--contains", &c0], p);
    assert_cli_success(&r0, "describe --contains c0");
    assert_eq!(String::from_utf8_lossy(&r0.stdout).trim(), "v1.0~2");

    // A commit created after the tag has no descendant tag → cannot be named.
    std::fs::write(p.join("c.txt"), "c").unwrap();
    assert_cli_success(&run_libra_command(&["add", "c.txt"], p), "add c");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c3", "--no-verify"], p),
        "c3",
    );
    let c3 = head(p);
    let nc = run_libra_command(&["describe", "--contains", &c3], p);
    assert!(
        !nc.status.success(),
        "a commit with no descendant tag must fail"
    );
    // The failure must be the dedicated NoContainingTag error (message + the
    // contains-specific hint), not the generic NoNamesFound (`--tags`/`--always`).
    let nc_err = String::from_utf8_lossy(&nc.stderr);
    assert!(
        nc_err.contains("no tag contains it"),
        "expected the dedicated no-containing-tag message, got: {nc_err}"
    );
    assert!(
        nc_err.contains("names a commit by a tag that descends from it"),
        "expected the contains-specific hint, got: {nc_err}"
    );
}

/// Equal-weight ties (a commit reachable from two distinct descendant tags at
/// the same weight) must resolve deterministically. name-rev seeds tags in
/// tag-name order, so the lexicographically smaller tag wins — and the result
/// is reproducible across runs (no HashMap-iteration nondeterminism). The two
/// tags sit on **different** commits (each one first-parent step above C0) so
/// the tie is genuinely decided by seed order, not by same-commit tag collapse.
#[test]
fn test_describe_contains_tie_break_is_deterministic() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let head = |p: &std::path::Path| -> String {
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
            .trim()
            .to_string()
    };

    // Base commit C0, then two sibling commits that each have C0 as their sole
    // (first) parent: Ca on the current branch and Cz on a branch forked at C0.
    // C0 is exactly one first-parent step below each tag → a weight-1 tie.
    let c0 = head(p);
    assert_cli_success(
        &run_libra_command(&["branch", "sib", &c0], p),
        "branch sib at c0",
    );

    std::fs::write(p.join("a.txt"), "a").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add a");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ca", "--no-verify"], p),
        "ca",
    );
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "a", "v-aaa"], p),
        "tag v-aaa on Ca",
    );

    assert_cli_success(&run_libra_command(&["switch", "sib"], p), "switch sib");
    std::fs::write(p.join("z.txt"), "z").unwrap();
    assert_cli_success(&run_libra_command(&["add", "z.txt"], p), "add z");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "cz", "--no-verify"], p),
        "cz",
    );
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "z", "v-zzz"], p),
        "tag v-zzz on Cz",
    );

    // C0 is `~1` below both tags; the smaller tag name (v-aaa) must win, the
    // same way every run.
    let expected = "v-aaa~1";
    for _ in 0..3 {
        let r = run_libra_command(&["describe", "--contains", &c0], p);
        assert_cli_success(&r, "describe --contains tie");
        assert_eq!(
            String::from_utf8_lossy(&r.stdout).trim(),
            expected,
            "equal-weight tie must resolve to the lexicographically smallest tag, every run"
        );
    }
}

/// `--contains --exact-match` (and `--contains --candidates 0`) accept only a
/// tag that points directly at the target; a relative `<tag>~N` name is rejected
/// with the exact-match error, matching the forward describe path.
#[test]
fn test_describe_contains_exact_match_requires_direct_tag() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let head = |p: &std::path::Path| -> String {
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
            .trim()
            .to_string()
    };

    let c0 = head(p);
    std::fs::write(p.join("a.txt"), "a").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt"], p), "add a");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "c1",
    );
    let c1 = head(p);
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "release", "v1.0"], p),
        "tag v1.0",
    );

    // The tagged commit itself is an exact contains match under both spellings.
    // `v1.0` is annotated, so an exact hit renders `<tag>^0` (DESC-01a).
    for flag in [
        ["--exact-match"].as_slice(),
        ["--candidates", "0"].as_slice(),
    ] {
        let mut argv = vec!["describe", "--contains"];
        argv.extend_from_slice(flag);
        argv.push(&c1);
        let r = run_libra_command(&argv, p);
        assert_cli_success(&r, "exact contains at tagged commit");
        assert_eq!(String::from_utf8_lossy(&r.stdout).trim(), "v1.0^0");
    }

    // An ancestor (named v1.0~1 without exact-match) is NOT an exact match —
    // under BOTH spellings (`--exact-match` and `--candidates 0`).
    for flag in [
        ["--exact-match"].as_slice(),
        ["--candidates", "0"].as_slice(),
    ] {
        let mut argv = vec!["describe", "--contains"];
        argv.extend_from_slice(flag);
        argv.push(&c0);
        let r = run_libra_command(&argv, p);
        assert!(
            !r.status.success(),
            "exact contains must reject a relative (~N) name for {flag:?}"
        );
        let err = String::from_utf8_lossy(&r.stderr);
        assert!(
            err.contains("no tag exactly matches"),
            "expected exact-match failure for a relative contains name ({flag:?}), got: {err}"
        );
    }
    // Sanity: without exact-match the same ancestor IS nameable as v1.0~1.
    let r = run_libra_command(&["describe", "--contains", &c0], p);
    assert_cli_success(&r, "non-exact contains names the ancestor");
    assert_eq!(String::from_utf8_lossy(&r.stdout).trim(), "v1.0~1");

    // A commit created AFTER the tag has no descendant tag at all. Under
    // exact-match the failure must still be "no tag exactly matches" (NOT the
    // contains-specific "no tag contains it") for both spellings — this pins the
    // round-3 regression where the no-descendant-tag path fell through to
    // NoContainingTag instead of NoExactMatch.
    std::fs::write(p.join("b.txt"), "b").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.txt"], p), "add b");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "c2",
    );
    let c2 = head(p);
    for flag in [
        ["--exact-match"].as_slice(),
        ["--candidates", "0"].as_slice(),
    ] {
        let mut argv = vec!["describe", "--contains"];
        argv.extend_from_slice(flag);
        argv.push(&c2);
        let r = run_libra_command(&argv, p);
        assert!(
            !r.status.success(),
            "exact contains on an untagged-descendant commit must fail for {flag:?}"
        );
        let err = String::from_utf8_lossy(&r.stderr);
        assert!(
            err.contains("no tag exactly matches"),
            "no-descendant-tag exact-match must report exact-match failure ({flag:?}), got: {err}"
        );
        assert!(
            !err.contains("no tag contains it"),
            "exact-match must NOT use the contains-specific error ({flag:?}), got: {err}"
        );
    }
}

#[test]
fn contains_exact_tag_appends_caret_zero() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let head = |p: &std::path::Path| -> String {
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
            .trim()
            .to_string()
    };

    std::fs::write(p.join("c1.txt"), "c1").unwrap();
    assert_cli_success(&run_libra_command(&["add", "c1.txt"], p), "add c1");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    let c1 = head(p);
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "release", "v1.0"], p),
        "annotated tag v1.0",
    );

    std::fs::write(p.join("c2.txt"), "c2").unwrap();
    assert_cli_success(&run_libra_command(&["add", "c2.txt"], p), "add c2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );
    let c2 = head(p);
    assert_cli_success(&run_libra_command(&["tag", "lw"], p), "lightweight tag lw");

    assert_cli_success(
        &run_libra_command(&["switch", "--detach", &c1], p),
        "detach at c1",
    );

    let implicit = run_libra_command(&["describe", "--contains"], p);
    assert_cli_success(&implicit, "describe --contains");
    assert_eq!(String::from_utf8_lossy(&implicit.stdout).trim(), "v1.0^0");

    let explicit = run_libra_command(&["describe", "--contains", &c1], p);
    assert_cli_success(&explicit, "describe --contains c1");
    assert_eq!(String::from_utf8_lossy(&explicit.stdout).trim(), "v1.0^0");

    let lightweight = run_libra_command(&["describe", "--contains", &c2], p);
    assert_cli_success(&lightweight, "describe --contains c2");
    assert_eq!(String::from_utf8_lossy(&lightweight.stdout).trim(), "lw");

    let json = run_libra_command(&["describe", "--contains", "--json", &c1], p);
    assert_cli_success(&json, "describe --contains --json c1");
    let json = parse_json_stdout(&json);
    assert_eq!(json["data"]["result"], "v1.0^0");
    assert_eq!(json["data"]["tag"], "v1.0");
    assert_eq!(json["data"]["exact_match"], true);
}

#[test]
fn contains_merge_parent_not_exact() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let head = |p: &std::path::Path| -> String {
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
            .trim()
            .to_string()
    };
    let main_branch =
        String::from_utf8_lossy(&run_libra_command(&["branch", "--show-current"], p).stdout)
            .trim()
            .to_string();

    assert_cli_success(&run_libra_command(&["branch", "side"], p), "branch side");
    std::fs::write(p.join("m1.txt"), "m1").unwrap();
    assert_cli_success(&run_libra_command(&["add", "m1.txt"], p), "add m1");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "m1", "--no-verify"], p),
        "commit m1",
    );

    assert_cli_success(&run_libra_command(&["switch", "side"], p), "switch side");
    std::fs::write(p.join("s0.txt"), "s0").unwrap();
    assert_cli_success(&run_libra_command(&["add", "s0.txt"], p), "add s0");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "s0", "--no-verify"], p),
        "commit s0",
    );
    std::fs::write(p.join("s1.txt"), "s1").unwrap();
    assert_cli_success(&run_libra_command(&["add", "s1.txt"], p), "add s1");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "s1", "--no-verify"], p),
        "commit s1",
    );
    let s1 = head(p);

    assert_cli_success(
        &run_libra_command(&["switch", &main_branch], p),
        "switch main",
    );
    assert_cli_success(&run_libra_command(&["merge", "side"], p), "merge side");
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "release", "v2.0"], p),
        "annotated tag v2.0",
    );
    let second_parent = run_libra_command(&["rev-parse", "HEAD^2"], p);
    assert_cli_success(&second_parent, "rev-parse HEAD^2");
    assert_eq!(String::from_utf8_lossy(&second_parent.stdout).trim(), s1);

    let output = run_libra_command(&["describe", "--contains", &s1], p);
    assert_cli_success(&output, "describe --contains s1");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "v2.0^2");

    let s1_parent = format!("{s1}~1");
    let parent = run_libra_command(&["describe", "--contains", &s1_parent], p);
    assert_cli_success(&parent, "describe --contains s1~1");
    assert_eq!(String::from_utf8_lossy(&parent.stdout).trim(), "v2.0^2~1");

    let json = run_libra_command(&["describe", "--contains", "--json", &s1], p);
    assert_cli_success(&json, "describe --contains --json s1");
    let json = parse_json_stdout(&json);
    assert_eq!(json["data"]["exact_match"], false);
}

/// `describe --contains` over a merge: the first-parent ancestor is named
/// `<tag>~<n>`, the merged-in (second-parent) ancestor is named `<tag>^2`, a
/// lightweight tag is honored (name-rev --tags), and `--first-parent` prunes the
/// merged-in side so a second-parent-only commit can no longer be named.
#[test]
fn test_describe_contains_merge_first_parent_and_lightweight() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let head = |p: &std::path::Path| -> String {
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
            .trim()
            .to_string()
    };
    let main_branch =
        String::from_utf8_lossy(&run_libra_command(&["branch", "--show-current"], p).stdout)
            .trim()
            .to_string();

    // Branch `feature` from the base, then diverge: C1 on the main branch and
    // Cf on feature.
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], p),
        "branch feature",
    );
    std::fs::write(p.join("m.txt"), "m").unwrap();
    assert_cli_success(&run_libra_command(&["add", "m.txt"], p), "add m");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "c1",
    );
    let c1 = head(p);

    assert_cli_success(
        &run_libra_command(&["switch", "feature"], p),
        "switch feature",
    );
    std::fs::write(p.join("f.txt"), "f").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], p), "add f");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "cf", "--no-verify"], p),
        "cf",
    );
    let cf = head(p);

    assert_cli_success(
        &run_libra_command(&["switch", &main_branch], p),
        "switch back",
    );
    assert_cli_success(
        &run_libra_command(&["merge", "feature"], p),
        "merge feature",
    );
    // A lightweight tag (no -m) on the merge commit — --contains must honor it.
    assert_cli_success(
        &run_libra_command(&["tag", "v2.0"], p),
        "tag v2.0 (lightweight)",
    );

    // First-parent ancestor → ~1; merged-in (second-parent) ancestor → ^2.
    let r1 = run_libra_command(&["describe", "--contains", &c1], p);
    assert_cli_success(&r1, "contains c1");
    assert_eq!(String::from_utf8_lossy(&r1.stdout).trim(), "v2.0~1");
    let rf = run_libra_command(&["describe", "--contains", &cf], p);
    assert_cli_success(&rf, "contains cf");
    assert_eq!(String::from_utf8_lossy(&rf.stdout).trim(), "v2.0^2");

    // --first-parent prunes the merged-in side, so Cf can no longer be named.
    assert!(
        !run_libra_command(&["describe", "--contains", "--first-parent", &cf], p)
            .status
            .success(),
        "with --first-parent, a second-parent-only commit must not be named"
    );
}

#[test]
fn i1062_consumer_describe_dirty_uses_clean_representation() {
    eol_consumer_describe_dirty_uses_clean_representation(false);
}

#[test]
fn i861_consumer_describe_dirty_uses_clean_representation() {
    eol_consumer_describe_dirty_uses_clean_representation(true);
}

fn eol_consumer_describe_dirty_uses_clean_representation(auto: bool) {
    use super::{add_test::i1062_ok, status_test::i1062_committed_scripts};
    let temp = if auto {
        super::status_test::i861_committed_scripts("sha1")
    } else {
        i1062_committed_scripts("sha1")
    };
    let repo = temp.path().join("repo");
    i1062_ok(&repo, &["tag", "-a", "eol-base", "-m", "base"]);
    let clean = i1062_ok(&repo, &["describe", "--dirty"]);
    assert_eq!(String::from_utf8(clean).unwrap().trim(), "eol-base");
    std::fs::write(repo.join("probe.ps1"), b"real change\r\n").unwrap();
    let dirty = i1062_ok(&repo, &["describe", "--dirty"]);
    assert_eq!(String::from_utf8(dirty).unwrap().trim(), "eol-base-dirty");
}

#[test]
fn test_describe_contains_exact_match_renders_tag_peel() {
    // AC1/AC2/AC4/AC5/AC6: an annotated tag pointing straight at the target is
    // reported as `<tag>^0`, while `data.tag` stays the bare tag name.
    let repo = create_committed_repo_via_cli();
    let head =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], repo.path()).stdout)
            .trim()
            .to_string();
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "annotated", "v-ann"], repo.path()),
        "annotated tag",
    );

    for flag in [
        vec!["describe", "--contains", "--exact-match", head.as_str()],
        vec!["describe", "--contains", "--candidates", "0", head.as_str()],
    ] {
        let out = run_libra_command(&flag, repo.path());
        assert_cli_success(&out, "exact contains on an annotated tag");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "v-ann^0",
            "AC1/AC2: the annotated tag is rendered peeled"
        );
    }

    let json = run_libra_command(
        &[
            "describe",
            "--contains",
            "--exact-match",
            "--json",
            head.as_str(),
        ],
        repo.path(),
    );
    assert_cli_success(&json, "AC4: json exact match");
    let parsed = parse_json_stdout(&json);
    assert_eq!(parsed["data"]["result"], "v-ann^0", "AC4: result is peeled");
    assert_eq!(parsed["data"]["tag"], "v-ann", "AC5: tag stays bare");
    assert_eq!(parsed["data"]["exact_match"], true, "AC6: exact match flag");

    // AC3: a lightweight tag stays bare.
    let light = create_committed_repo_via_cli();
    let light_head =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], light.path()).stdout)
            .trim()
            .to_string();
    assert_cli_success(
        &run_libra_command(&["tag", "v-light"], light.path()),
        "lightweight tag",
    );
    let out = run_libra_command(
        &[
            "describe",
            "--contains",
            "--exact-match",
            light_head.as_str(),
        ],
        light.path(),
    );
    assert_cli_success(&out, "AC3: exact contains on a lightweight tag");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "v-light",
        "AC3: lightweight tags are not peeled"
    );
}

#[test]
fn test_describe_no_match_clears_previous_patterns() {
    use clap::Parser as _;

    // c1 (annotated v1.0) → c2 (annotated v2.0) → c3.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "v1", "v1.0"], p),
        "annotated v1.0",
    );
    let c2 = {
        std::fs::write(p.join("tracked.txt"), "second\n").expect("write second");
        assert_cli_success(
            &run_libra_command(&["add", "tracked.txt"], p),
            "stage second",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
            "commit c2",
        );
        assert_cli_success(
            &run_libra_command(&["tag", "-m", "v2", "v2.0"], p),
            "annotated v2.0",
        );
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
            .trim()
            .to_string()
    };
    std::fs::write(p.join("tracked.txt"), "third\n").expect("write third");
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], p),
        "stage third",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c3", "--no-verify"], p),
        "commit c3",
    );
    let short =
        |out: &std::process::Output| String::from_utf8_lossy(&out.stdout).trim().to_string();
    let starts = |text: &str, prefix: &str| text.starts_with(prefix);

    // AC1: `--no-match` clears every earlier `--match`.
    let out = run_libra_command(
        &[
            "describe",
            "--match",
            "v1.*",
            "--match",
            "v0.*",
            "--no-match",
        ],
        p,
    );
    assert_cli_success(&out, "AC1: describe --match --match --no-match");
    assert!(starts(&short(&out), "v2.0-1-g"), "AC1: {}", short(&out));

    // AC2: only the `--match` after `--no-match` applies.
    let out = run_libra_command(
        &[
            "describe",
            "--match",
            "v2.*",
            "--no-match",
            "--match",
            "v1.*",
        ],
        p,
    );
    assert_cli_success(&out, "AC2");
    assert!(starts(&short(&out), "v1.0-2-g"), "AC2: {}", short(&out));

    // AC3: `--contains` with a cleared match hits the annotated tag directly.
    let out = run_libra_command(
        &["describe", "--contains", "--match=B", "--no-match", &c2],
        p,
    );
    assert_cli_success(&out, "AC3");
    assert_eq!(
        short(&out),
        "v2.0^0",
        "AC3: the annotated tag is a direct hit"
    );

    // AC4: `--no-match` alone is valid.
    let out = run_libra_command(&["describe", "--no-match"], p);
    assert_cli_success(&out, "AC4");
    assert!(starts(&short(&out), "v2.0-1-g"), "AC4: {}", short(&out));

    // AC5: a later `--exclude` still applies.
    let out = run_libra_command(
        &[
            "describe",
            "--match",
            "v*",
            "--no-match",
            "--exclude",
            "v2.*",
        ],
        p,
    );
    assert_cli_success(&out, "AC5");
    assert!(starts(&short(&out), "v1.0-2-g"), "AC5: {}", short(&out));

    // AC6: an earlier `--exclude` is untouched by `--no-match`.
    let out = run_libra_command(&["describe", "--exclude", "v2.*", "--no-match"], p);
    assert_cli_success(&out, "AC6");
    assert!(starts(&short(&out), "v1.0-2-g"), "AC6: {}", short(&out));

    // AC7: the parser clears the pattern list.
    let args = libra::command::describe::DescribeArgs::try_parse_from([
        "describe",
        "--match",
        "v1.*",
        "--match",
        "v0.*",
        "--no-match",
    ])
    .expect("the combination parses");
    assert!(args.match_patterns.is_empty(), "AC7: patterns were cleared");
}

#[test]
fn test_describe_all_filters_branch_candidates() {
    // `rel` (annotated) and `br-x` both point at the first commit; a remote `o`
    // contributes `o/main` on a different commit.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "rel", "rel"], p),
        "annotated rel",
    );
    assert_cli_success(&run_libra_command(&["branch", "br-x"], p), "branch br-x");
    let head = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();

    // A dedicated temp dir: sibling directories of the repo temp dir are shared
    // across tests.
    let remote_root = tempfile::tempdir().expect("remote root");
    let remote_dir = remote_root.path().join("o-remote");
    std::fs::create_dir_all(&remote_dir).expect("mkdir remote");
    assert_cli_success(&run_libra_command(&["init"], &remote_dir), "init remote");
    run_libra_command(&["config", "set", "user.name", "t"], &remote_dir);
    run_libra_command(&["config", "set", "user.email", "t@t"], &remote_dir);
    std::fs::write(remote_dir.join("g.txt"), "g\n").expect("write g");
    assert_cli_success(
        &run_libra_command(&["add", "g.txt"], &remote_dir),
        "stage g",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "o1", "--no-verify"], &remote_dir),
        "commit o1",
    );
    assert_cli_success(
        &run_libra_command(
            &["remote", "add", "o", remote_dir.to_str().expect("remote")],
            p,
        ),
        "add remote o",
    );
    assert_cli_success(&run_libra_command(&["fetch", "o"], p), "fetch o");

    let stdout =
        |out: &std::process::Output| String::from_utf8_lossy(&out.stdout).trim().to_string();

    // AC1: nothing matches → the existing "no names" error.
    let out = run_libra_command(
        &["describe", "--contains", "--all", "--match", "zzz*", &head],
        p,
    );
    assert!(!out.status.success(), "AC1: no match must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-REPO-003"),
        "AC1: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // AC2: local branches match on their short name.
    let out = run_libra_command(&["describe", "--all", "--match", "br-*", &head], p);
    assert_cli_success(&out, "AC2");
    assert_eq!(stdout(&out), "heads/br-x", "AC2");

    // AC3: exclusions apply to local and remote short names.
    let out = run_libra_command(
        &[
            "describe",
            "--all",
            "--exclude",
            "main",
            "--exclude",
            "o/*",
            &head,
        ],
        p,
    );
    assert_cli_success(&out, "AC3");
    assert_eq!(stdout(&out), "tags/rel", "AC3: only the tag survives");

    // AC4: remote-tracking branches match on `<remote>/<branch>`.
    let out = run_libra_command(
        &[
            "describe",
            "--all",
            "--match",
            "o/m*",
            "refs/remotes/o/main",
        ],
        p,
    );
    assert_cli_success(&out, "AC4");
    assert_eq!(stdout(&out), "remotes/o/main", "AC4");

    // AC5: `--contains --all` filters local branches the same way.
    let out = run_libra_command(
        &["describe", "--contains", "--all", "--match", "br-*", &head],
        p,
    );
    assert_cli_success(&out, "AC5");
    assert_eq!(stdout(&out), "heads/br-x", "AC5");

    // AC6: a tag that also matches wins over the branch on the same commit.
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "rel", "rel2"], p),
        "second tag",
    );
    let out = run_libra_command(
        &[
            "describe", "--all", "--match", "rel*", "--match", "br-*", &head,
        ],
        p,
    );
    assert_cli_success(&out, "AC6");
    assert!(
        stdout(&out).starts_with("tags/rel"),
        "AC6: {}",
        stdout(&out)
    );

    // AC7: without filters a branch-only repository names the commit by branch.
    let plain = create_committed_repo_via_cli();
    let plain_out = run_libra_command(&["describe", "--all", "HEAD"], plain.path());
    assert_cli_success(&plain_out, "AC7");
    assert!(
        stdout(&plain_out).starts_with("heads/"),
        "AC7: {}",
        stdout(&plain_out)
    );

    // AC8: excluding the only matching branch fails the contains query.
    let out = run_libra_command(
        &[
            "describe",
            "--contains",
            "--all",
            "--match",
            "br-*",
            "--exclude",
            "br-x",
            &head,
        ],
        p,
    );
    assert!(!out.status.success(), "AC8: the only match was excluded");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("LBR-REPO-003"),
        "AC8: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Shared FX-DESC-MN fixture: an annotated tag object whose internal name is
/// `A`, published only as `refs/tags/Q`, plus one detached child commit `c`.
fn misnamed_tag_repo() -> (tempfile::TempDir, String, String) {
    let repo = tempdir().expect("repo");
    let p = repo.path();
    init_repo_via_cli(p);
    configure_identity_via_cli(p);
    std::fs::write(p.join("a"), "a\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a"], p), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "Ca", "--no-verify"], p),
        "Ca",
    );
    let ca = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    assert_cli_success(
        &run_libra_command(&["tag", "-a", "A", "-m", "msg", &ca], p),
        "tag A",
    );
    let tag_oid = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "A"], p).stdout)
        .trim()
        .to_string();
    assert_cli_success(&run_libra_command(&["tag", "Q", &tag_oid], p), "tag Q");
    assert_cli_success(&run_libra_command(&["tag", "-d", "A"], p), "drop A ref");
    assert_cli_success(
        &run_libra_command(&["checkout", "--detach", &ca], p),
        "detach at Ca",
    );
    std::fs::write(p.join("a"), "b\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a"], p), "add c");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "C", "--no-verify"], p),
        "C",
    );
    let c = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    (repo, ca, c)
}

/// DESC-05: a misnamed annotated tag renders its internal name, announces the
/// mismatch once, and `--abbrev=0` still forces the `-<n>-g<oid>` suffix.
#[test]
fn misnamed_annotated_tag_uses_internal_name() {
    let (repo, ca, c) = misnamed_tag_repo();
    let p = repo.path();
    let short = &c[..7];

    let plain = run_libra_command(&["describe", &c], p);
    assert_cli_success(&plain, "AC1 describe");
    assert_eq!(
        String::from_utf8_lossy(&plain.stdout).trim(),
        format!("A-1-g{short}"),
        "AC1 internal name"
    );
    let warnings: Vec<String> = String::from_utf8_lossy(&plain.stderr)
        .lines()
        .filter(|line| line.contains("externally known as"))
        .map(str::to_string)
        .collect();
    assert_eq!(
        warnings,
        vec!["warning: tag 'Q' is externally known as 'A'".to_string()],
        "AC2 one warning for the selected tag"
    );

    let abbrev0 = run_libra_command(&["describe", "--abbrev=0", &c], p);
    assert_cli_success(&abbrev0, "AC3 describe --abbrev=0");
    assert_eq!(
        String::from_utf8_lossy(&abbrev0.stdout).trim(),
        format!("A-1-g{c}"),
        "AC3 forced suffix with full oid"
    );

    let json = run_libra_command(&["describe", "--abbrev=0", "--json", &c], p);
    assert_cli_success(&json, "AC4/AC5 describe --json");
    let parsed = parse_json_stdout(&json);
    assert_eq!(parsed["data"]["tag"], "A", "AC4 tag");
    assert_eq!(parsed["data"]["abbreviated_commit"], c, "AC5 full oid");

    let all = run_libra_command(&["describe", "--all", &c], p);
    assert_cli_success(&all, "AC6 describe --all");
    assert_eq!(
        String::from_utf8_lossy(&all.stdout).trim(),
        format!("tags/Q-1-g{short}"),
        "AC6 --all keeps the reference name"
    );

    let contains = run_libra_command(&["describe", "--contains", &ca], p);
    assert_cli_success(&contains, "AC7 describe --contains");
    assert_eq!(
        String::from_utf8_lossy(&contains.stdout).trim(),
        "Q^0",
        "AC7 --contains reference name"
    );
    assert!(
        !String::from_utf8_lossy(&contains.stderr).contains("externally known as"),
        "AC8 no warning under --contains"
    );
}

/// DESC-04 AC6/AC7: `--no-long` on a misnamed tag keeps the forced suffix, and
/// the rendered name resolves back to the tagged commit.
#[test]
fn misnamed_annotated_tag_no_long_keeps_suffix() {
    let (repo, ca, _c) = misnamed_tag_repo();
    let p = repo.path();
    let out = run_libra_command(&["describe", "--no-long", &ca], p);
    assert_cli_success(&out, "AC6 describe --no-long");
    let rendered = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(rendered, format!("A-0-g{}", &ca[..7]), "AC6 forced suffix");
    let resolved = run_libra_command(&["rev-parse", &rendered], p);
    assert_cli_success(&resolved, "AC7 rev-parse");
    assert_eq!(
        String::from_utf8_lossy(&resolved.stdout).trim(),
        ca,
        "AC7 resolves to the tagged commit"
    );
}
