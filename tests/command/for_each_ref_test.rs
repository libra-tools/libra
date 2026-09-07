//! Integration tests for `libra for-each-ref`.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::{fs, io::Write};

use serial_test::serial;
use tempfile::tempdir;

use super::*;

/// Create a repo, add a file and commit with the given message.
async fn setup_repo_with_commit(temp: &tempfile::TempDir) {
    test::setup_with_new_libra_in(temp.path()).await;
    let _guard = ChangeDirGuard::new(temp.path());

    let mut f = fs::File::create("a.txt").unwrap();
    writeln!(f, "hello").unwrap();

    add::execute(AddArgs {
        pathspec: vec!["a.txt".into()],
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
    })
    .await;

    commit::execute(CommitArgs {
        message: Some("initial".into()),
        ..Default::default()
    })
    .await;
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_lists_heads() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await;

    let output = run_libra_command(&["for-each-ref", "--heads"], temp.path());
    assert_cli_success(&output, "for-each-ref --heads should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("refs/heads/main"),
        "expected refs/heads/main in output, got: {stdout}"
    );
}

#[test]
fn test_for_each_ref_contains_filter() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    std::fs::write(p.join("f1.txt"), "1\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f1.txt"], p), "add f1");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    // `old` points at c1 and never advances.
    assert_cli_success(&run_libra_command(&["branch", "old"], p), "branch old");

    std::fs::write(p.join("f2.txt"), "2\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f2.txt"], p), "add f2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );

    let head = run_libra_command(&["rev-parse", "HEAD"], p);
    let c2 = String::from_utf8_lossy(&head.stdout).trim().to_string();

    // Only main (at c2) contains c2; `old` (at c1) does not.
    let out = run_libra_command(&["for-each-ref", "--heads", "--contains", &c2], p);
    assert_cli_success(&out, "for-each-ref --contains c2");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("refs/heads/main"),
        "main should contain c2: {stdout}"
    );
    assert!(
        !stdout.contains("refs/heads/old"),
        "old should NOT contain c2: {stdout}"
    );
}

#[test]
fn test_for_each_ref_merged_filter() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    std::fs::write(p.join("f1.txt"), "1\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f1.txt"], p), "add f1");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c1", "--no-verify"], p),
        "commit c1",
    );
    let c1 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    // `old` points at c1 and never advances.
    assert_cli_success(&run_libra_command(&["branch", "old"], p), "branch old");
    // An annotated tag at c1: its entry peels to the commit for reachability.
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "anno", "atag"], p),
        "annotated tag atag at c1",
    );

    std::fs::write(p.join("f2.txt"), "2\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f2.txt"], p), "add f2");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c2", "--no-verify"], p),
        "commit c2",
    );
    let c2 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    // A lightweight tag at c2, used to exercise fully-qualified ref targets.
    assert_cli_success(
        &run_libra_command(&["tag", "lw"], p),
        "lightweight tag lw at c2",
    );

    // --merged=c2: both main (c2) and old (c1) are reachable from c2.
    let out = run_libra_command(&["for-each-ref", "--heads", "--merged", &c2], p);
    assert_cli_success(&out, "for-each-ref --merged c2");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("refs/heads/main") && stdout.contains("refs/heads/old"),
        "both main and old should be merged into c2: {stdout}"
    );

    // --no-merged=c1: main (c2) is not reachable from c1; old (c1) is.
    let out = run_libra_command(&["for-each-ref", "--heads", "--no-merged", &c1], p);
    assert_cli_success(&out, "for-each-ref --no-merged c1");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("refs/heads/main"),
        "main should NOT be merged into c1: {stdout}"
    );
    assert!(
        !stdout.contains("refs/heads/old"),
        "old should be merged into c1 and thus excluded: {stdout}"
    );

    // Annotated tag peeling: atag (at c1) is reachable from c2, so --merged=c2
    // includes it; --no-merged=c1 excludes it (c1 is merged into c1).
    let out = run_libra_command(&["for-each-ref", "--tags", "--merged", &c2], p);
    assert_cli_success(&out, "for-each-ref --tags --merged c2");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("refs/tags/atag"),
        "annotated tag atag should be merged into c2: {stdout}"
    );

    let out = run_libra_command(&["for-each-ref", "--tags", "--no-merged", &c1], p);
    assert_cli_success(&out, "for-each-ref --tags --no-merged c1");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("refs/tags/atag"),
        "annotated tag atag (c1) should be merged into c1 and excluded: {stdout}"
    );

    // The merge TARGET may itself be an annotated tag name; it peels to its
    // commit (atag -> c1), so only refs reachable from c1 are "merged".
    let out = run_libra_command(&["for-each-ref", "--heads", "--merged", "atag"], p);
    assert_cli_success(&out, "for-each-ref --heads --merged atag");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("refs/heads/old"),
        "old (c1) should be merged into atag (c1): {stdout}"
    );
    assert!(
        !stdout.contains("refs/heads/main"),
        "main (c2) should NOT be merged into atag (c1): {stdout}"
    );

    // Fully-qualified ref targets must resolve too (no regression vs the legacy
    // resolver): --contains refs/tags/lw (lw -> c2) keeps only refs containing c2.
    let out = run_libra_command(
        &["for-each-ref", "--heads", "--contains", "refs/tags/lw"],
        p,
    );
    assert_cli_success(&out, "for-each-ref --contains refs/tags/lw");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("refs/heads/main") && !stdout.contains("refs/heads/old"),
        "only main should contain c2 (refs/tags/lw): {stdout}"
    );

    // Namespace disambiguation: a branch named `atag` at c2 collides with the
    // annotated tag `atag` at c1. `--merged refs/tags/atag` must resolve the TAG
    // (c1), not the branch (c2) — otherwise main (c2) would be reported merged.
    assert_cli_success(
        &run_libra_command(&["branch", "atag"], p),
        "branch atag at c2 (collides with tag atag)",
    );
    let out = run_libra_command(
        &["for-each-ref", "--heads", "--merged", "refs/tags/atag"],
        p,
    );
    assert_cli_success(&out, "for-each-ref --merged refs/tags/atag");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("refs/heads/old") && !stdout.contains("refs/heads/main"),
        "refs/tags/atag must resolve the TAG (c1), not branch atag (c2): {stdout}"
    );

    // The branch-namespace counterpart: refs/heads/atag must resolve the BRANCH
    // (c2), so main (c2) IS reported merged — proving both directions.
    let out = run_libra_command(
        &["for-each-ref", "--heads", "--merged", "refs/heads/atag"],
        p,
    );
    assert_cli_success(&out, "for-each-ref --merged refs/heads/atag");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("refs/heads/main") && stdout.contains("refs/heads/old"),
        "refs/heads/atag must resolve the BRANCH (c2), so main is merged: {stdout}"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_for_each_ref_merged_resolves_remote_tracking_namespace() {
    use libra::internal::branch::Branch;

    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let _guard = test::ChangeDirGuard::new(temp.path());
    let p = temp.path();

    // c1 then c2 on main.
    std::fs::write("a.txt", "1\n").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["a.txt".into()],
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
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("c1".into()),
        no_verify: true,
        ..Default::default()
    })
    .await;
    let c1 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();

    std::fs::write("a.txt", "2\n").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["a.txt".into()],
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
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("c2".into()),
        no_verify: true,
        ..Default::default()
    })
    .await;

    // Remote-tracking origin/main at c1, stored under the full ref name with the
    // `remote` column set — exactly as `libra fetch` persists it — plus a
    // colliding LOCAL branch literally named `refs/remotes/origin/main` at c2.
    Branch::update_branch("refs/remotes/origin/main", &c1, Some("origin"))
        .await
        .expect("create remote-tracking origin/main");
    assert_cli_success(
        &run_libra_command(&["branch", "refs/remotes/origin/main"], p),
        "create colliding local branch",
    );

    // `--no-merged refs/remotes/origin/main` must resolve the REMOTE-tracking ref
    // (c1), so main (c2) is NOT merged into c1 and is listed. If the local shadow
    // (c2) were used instead, main would be excluded.
    let out = run_libra_command(
        &[
            "for-each-ref",
            "--heads",
            "--no-merged",
            "refs/remotes/origin/main",
        ],
        p,
    );
    assert_cli_success(&out, "for-each-ref --no-merged refs/remotes/origin/main");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("refs/heads/main"),
        "refs/remotes/origin/main must resolve the remote-tracking ref (c1), not \
         the colliding local branch (c2); main should be listed: {stdout}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_format_and_json() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await;

    let output = run_libra_command(
        &["--json", "for-each-ref", "--heads", "--format=%(refname)"],
        temp.path(),
    );
    assert_cli_success(&output, "for-each-ref --json should succeed");

    let json = parse_json_stdout(&output);
    assert_eq!(json["command"], "for-each-ref");
    let entries = json["data"].as_array().expect("data should be an array");
    assert!(
        entries
            .iter()
            .any(|entry| entry["refname"] == "refs/heads/main"),
        "expected refs/heads/main in JSON output"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_sort_and_count() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await;

    let output = run_libra_command(&["for-each-ref", "--count=1"], temp.path());
    assert_cli_success(&output, "for-each-ref --count should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().count(),
        1,
        "expected exactly one line, got: {stdout}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_points_at_matches_direct_and_peeled_tag_targets() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await;

    let lightweight = run_libra_command(&["tag", "lw"], temp.path());
    assert_cli_success(&lightweight, "tag lw should succeed");
    let annotated = run_libra_command(&["tag", "-m", "annotated", "ann"], temp.path());
    assert_cli_success(&annotated, "tag -m ann should succeed");

    let head_output = run_libra_command(
        &[
            "for-each-ref",
            "--points-at",
            "HEAD",
            "--format=%(refname) %(objecttype)",
        ],
        temp.path(),
    );
    assert_cli_success(&head_output, "for-each-ref --points-at HEAD should succeed");
    let head_stdout = String::from_utf8_lossy(&head_output.stdout);
    assert!(
        head_stdout.contains("refs/heads/main commit"),
        "expected main branch in --points-at HEAD output, got: {head_stdout}"
    );
    assert!(
        head_stdout.contains("refs/tags/lw commit"),
        "expected lightweight tag in --points-at HEAD output, got: {head_stdout}"
    );
    assert!(
        head_stdout.contains("refs/tags/ann tag"),
        "expected annotated tag in --points-at HEAD output, got: {head_stdout}"
    );

    let tag_object_output = run_libra_command(
        &["for-each-ref", "--points-at", "ann", "--format=%(refname)"],
        temp.path(),
    );
    assert_cli_success(
        &tag_object_output,
        "for-each-ref --points-at ann should succeed",
    );
    let tag_stdout = String::from_utf8_lossy(&tag_object_output.stdout);
    assert_eq!(
        tag_stdout.trim(),
        "refs/tags/ann",
        "expected only annotated tag object ref, got: {tag_stdout}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_unknown_sort_rejects() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await;

    let output = run_libra_command(&["for-each-ref", "--sort=unknown"], temp.path());
    assert!(
        !output.status.success(),
        "expected failure for unsupported sort key"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported for-each-ref sort key"),
        "got: {stderr}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_sort_version_refname() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await;
    let p = temp.path();
    for v in ["v1.10", "v1.9", "v1.2", "v2.0", "v1.10.1"] {
        run_libra_command(&["tag", v], p);
    }

    let output = run_libra_command(
        &[
            "for-each-ref",
            "--sort=version:refname",
            "--format=%(refname)",
        ],
        p,
    );
    assert_cli_success(&output, "for-each-ref --sort=version:refname");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let tags: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("refs/tags/v"))
        .collect();
    let pos = |needle: &str| {
        tags.iter()
            .position(|l| l.ends_with(needle))
            .unwrap_or_else(|| panic!("missing {needle} in {tags:?}"))
    };
    // Numeric ordering: v1.9 must come before v1.10 (lexical sort gets this wrong).
    assert!(pos("v1.2") < pos("v1.9"), "v1.2 before v1.9: {tags:?}");
    assert!(
        pos("v1.9") < pos("v1.10"),
        "v1.9 before v1.10 (numeric, not lexical): {tags:?}"
    );
    assert!(
        pos("v1.10") < pos("v1.10.1"),
        "v1.10 before v1.10.1: {tags:?}"
    );
    assert!(
        pos("v1.10.1") < pos("v2.0"),
        "v1.10.1 before v2.0: {tags:?}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_format_short_atoms() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await;
    let p = temp.path();
    run_libra_command(&["branch", "feature-x"], p);

    // %(refname:short) strips the refs/heads/ namespace.
    let out = run_libra_command(
        &[
            "for-each-ref",
            "--heads",
            "--format=%(refname) => %(refname:short)",
        ],
        p,
    );
    assert_cli_success(&out, "for-each-ref %(refname:short)");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("refs/heads/main => main"),
        "short refname for main: {s:?}"
    );
    assert!(
        s.contains("refs/heads/feature-x => feature-x"),
        "short refname for feature-x: {s:?}"
    );

    // %(objectname:short) is the 7-char prefix of %(objectname).
    let out = run_libra_command(
        &[
            "for-each-ref",
            "--heads",
            "--format=%(objectname) %(objectname:short)",
        ],
        p,
    );
    assert_cli_success(&out, "for-each-ref %(objectname:short)");
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s.lines().next().unwrap_or("");
    let parts: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(parts.len(), 2, "expected full + short hash: {line:?}");
    assert_eq!(parts[1].len(), 7, "short hash should be 7 chars: {line:?}");
    assert!(
        parts[0].starts_with(parts[1]),
        "short hash must prefix the full hash: {line:?}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_head_marker_atom() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await; // checked out on main
    let p = temp.path();
    run_libra_command(&["branch", "feature-x"], p);

    // %(HEAD) is `*` for the current branch and a space otherwise.
    let out = run_libra_command(
        &[
            "for-each-ref",
            "--heads",
            "--format=%(HEAD)%(refname:short)",
        ],
        p,
    );
    assert_cli_success(&out, "for-each-ref %(HEAD)");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.lines().any(|l| l == "*main"),
        "current branch should be marked with *: {s:?}"
    );
    assert!(
        s.lines().any(|l| l == " feature-x"),
        "non-current branch should get a space: {s:?}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_upstream_atom() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await;
    let p = temp.path();
    run_libra_command(&["branch", "feature-y"], p); // no upstream

    // Configure main's upstream tracking ref (origin/main).
    assert_cli_success(
        &run_libra_command(&["config", "branch.main.remote", "origin"], p),
        "config branch.main.remote",
    );
    assert_cli_success(
        &run_libra_command(&["config", "branch.main.merge", "refs/heads/main"], p),
        "config branch.main.merge",
    );

    let out = run_libra_command(
        &[
            "for-each-ref",
            "--heads",
            "--format=%(refname:short)|%(upstream)|%(upstream:short)",
        ],
        p,
    );
    assert_cli_success(&out, "for-each-ref %(upstream)");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.lines()
            .any(|l| l == "main|refs/remotes/origin/main|origin/main"),
        "configured upstream atoms for main: {s:?}"
    );
    assert!(
        s.lines().any(|l| l == "feature-y||"),
        "branch without upstream has empty %(upstream): {s:?}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_push_atom() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await;
    let p = temp.path();

    // Read main's `%(push)` / `%(push:short)` line.
    let push_line = || -> String {
        let out = run_libra_command(
            &[
                "for-each-ref",
                "--heads",
                "--format=%(refname:short)|%(push)|%(push:short)",
            ],
            p,
        );
        assert_cli_success(&out, "for-each-ref %(push)");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|line| line.starts_with("main|"))
            .unwrap_or("")
            .to_string()
    };

    // With no remote config, %(push) is empty.
    assert_eq!(push_line(), "main||", "no remote config → empty push");

    // With only branch.main.remote, the push ref equals the upstream ref.
    assert_cli_success(
        &run_libra_command(&["config", "branch.main.remote", "origin"], p),
        "branch.main.remote",
    );
    assert_cli_success(
        &run_libra_command(&["config", "branch.main.merge", "refs/heads/main"], p),
        "branch.main.merge",
    );
    assert_eq!(
        push_line(),
        "main|refs/remotes/origin/main|origin/main",
        "push falls back to branch remote"
    );

    // With BOTH remote.pushDefault and branch.main.pushRemote set, pushRemote wins
    // (pins the full pushRemote > pushDefault > remote order).
    assert_cli_success(
        &run_libra_command(&["config", "remote.pushDefault", "pdef"], p),
        "remote.pushDefault",
    );
    assert_cli_success(
        &run_libra_command(&["config", "branch.main.pushRemote", "fork"], p),
        "branch.main.pushRemote",
    );
    assert_eq!(
        push_line(),
        "main|refs/remotes/fork/main|fork/main",
        "pushRemote overrides both pushDefault and remote"
    );

    // With pushRemote unset, remote.pushDefault applies (over branch.main.remote).
    assert_cli_success(
        &run_libra_command(&["config", "--unset", "branch.main.pushRemote"], p),
        "unset pushRemote",
    );
    assert_eq!(
        push_line(),
        "main|refs/remotes/pdef/main|pdef/main",
        "pushDefault applies before branch remote"
    );

    // The lowercase Git-config variable form (`pushremote`) is honored too.
    assert_cli_success(
        &run_libra_command(&["config", "branch.main.pushremote", "lower"], p),
        "branch.main.pushremote (lowercase)",
    );
    assert_eq!(
        push_line(),
        "main|refs/remotes/lower/main|lower/main",
        "lowercase pushremote variable is honored"
    );

    // Variable names are case-insensitive. In the (anomalous) case where two
    // case-variant rows coexist, resolution is deterministic: the most recently
    // inserted variant wins. Inserting a fresh camelCase row now takes
    // precedence over the earlier lowercase value.
    assert_cli_success(
        &run_libra_command(&["config", "branch.main.pushRemote", "camel2"], p),
        "branch.main.pushRemote (re-set, newest)",
    );
    assert_eq!(
        push_line(),
        "main|refs/remotes/camel2/main|camel2/main",
        "most recently inserted case variant wins"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_subject_atom() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await; // commits with subject "initial"
    let p = temp.path();

    // %(subject) renders the first line of the commit message.
    let out = run_libra_command(
        &[
            "for-each-ref",
            "--heads",
            "--format=%(refname:short)|%(subject)",
        ],
        p,
    );
    assert_cli_success(&out, "for-each-ref %(subject)");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.lines().any(|l| l == "main|initial"),
        "subject for main's commit should be 'initial': {s:?}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_subject_with_percent_paren_is_literal() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await;
    let p = temp.path();
    // A commit whose subject itself contains `%(` must NOT be re-parsed as a
    // format atom nor trip the unknown-atom error.
    std::fs::write(p.join("x.txt"), "x\n").unwrap();
    run_libra_command(&["add", "x.txt"], p);
    run_libra_command(&["commit", "-m", "fix %(weird) thing", "--no-verify"], p);

    let out = run_libra_command(&["for-each-ref", "--heads", "--format=%(subject)"], p);
    assert_cli_success(&out, "for-each-ref %(subject) with %( in subject");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.lines().any(|l| l == "fix %(weird) thing"),
        "subject containing %( must render literally: {s:?}"
    );

    // A genuinely unknown atom still errors.
    let bad = run_libra_command(&["for-each-ref", "--heads", "--format=%(bogus)"], p);
    assert!(
        !bad.status.success(),
        "unknown atom should fail: {}",
        String::from_utf8_lossy(&bad.stderr)
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_author_committer_atoms() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await;
    let p = temp.path();

    let out = run_libra_command(
        &[
            "for-each-ref",
            "--heads",
            "--format=%(authorname)|%(authoremail)|%(committername)|%(committeremail)",
        ],
        p,
    );
    assert_cli_success(&out, "for-each-ref author/committer atoms");
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s.lines().next().unwrap_or("");
    let f: Vec<&str> = line.split('|').collect();
    assert_eq!(f.len(), 4, "four author/committer fields: {line:?}");
    assert!(
        !f[0].is_empty(),
        "authorname non-empty for a commit ref: {line:?}"
    );
    assert!(
        f[1].starts_with('<') && f[1].ends_with('>'),
        "authoremail is angle-bracketed: {line:?}"
    );
    assert!(!f[2].is_empty(), "committername non-empty: {line:?}");
    assert!(
        f[3].starts_with('<') && f[3].ends_with('>'),
        "committeremail is angle-bracketed: {line:?}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_tagger_atoms() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await;
    let p = temp.path();
    // Create an annotated tag (`-m` implies annotated; it carries a tagger).
    run_libra_command(&["tag", "-m", "release one", "v1"], p);

    // %(taggername)/%(taggeremail) populated for the annotated tag.
    let out = run_libra_command(
        &[
            "for-each-ref",
            "--tags",
            "--format=%(taggername)|%(taggeremail)",
        ],
        p,
    );
    assert_cli_success(&out, "for-each-ref tagger atoms");
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s.lines().next().unwrap_or("");
    let f: Vec<&str> = line.split('|').collect();
    assert_eq!(f.len(), 2, "two tagger fields: {line:?}");
    assert!(
        !f[0].is_empty(),
        "taggername non-empty for annotated tag: {line:?}"
    );
    assert!(
        f[1].starts_with('<') && f[1].ends_with('>'),
        "taggeremail is angle-bracketed: {line:?}"
    );

    // For a commit (branch) ref, tagger atoms are empty.
    let out = run_libra_command(
        &[
            "for-each-ref",
            "--heads",
            "--format=[%(taggername)][%(taggeremail)]",
        ],
        p,
    );
    assert_cli_success(&out, "for-each-ref tagger on commit");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.lines().any(|l| l == "[][]"),
        "tagger atoms empty for a commit ref: {s:?}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_date_atoms() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await;
    let p = temp.path();

    // %(committerdate)/%(authordate) render in Git's default date format
    // (`Day Mon DD HH:MM:SS YYYY +ZZZZ`), in UTC (consistent with `libra log`).
    let out = run_libra_command(
        &[
            "for-each-ref",
            "--heads",
            "--format=%(authordate)|%(committerdate)",
        ],
        p,
    );
    assert_cli_success(&out, "for-each-ref date atoms");
    let s = String::from_utf8_lossy(&out.stdout);
    let line = s.lines().next().unwrap_or("");
    let (adate, cdate) = line.split_once('|').unwrap_or(("", ""));
    // Default format ends with a `+ZZZZ` zone and contains a 4-digit year.
    for d in [adate, cdate] {
        assert!(
            d.contains("+0000"),
            "date renders in UTC default format: {line:?}"
        );
        assert!(d.contains("20"), "date contains a year: {line:?}");
    }
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_refname_lstrip_rstrip() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await; // refs/heads/main
    let p = temp.path();
    let f = |spec: &str| {
        let fmt = format!("--format=%(refname:{spec})");
        let out = run_libra_command(&["for-each-ref", "--heads", &fmt], p);
        assert_cli_success(&out, spec);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .to_string()
    };
    assert_eq!(
        f("lstrip=1"),
        "heads/main",
        "lstrip=1 drops 1 leading component"
    );
    assert_eq!(f("lstrip=2"), "main", "lstrip=2 drops 2 leading components");
    assert_eq!(f("lstrip=-1"), "main", "lstrip=-1 keeps the last component");
    assert_eq!(
        f("rstrip=1"),
        "refs/heads",
        "rstrip=1 drops 1 trailing component"
    );
    assert_eq!(
        f("rstrip=-1"),
        "refs",
        "rstrip=-1 keeps the first component"
    );
    assert_eq!(f("lstrip=5"), "", "lstrip beyond depth yields empty");
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_contents_and_body_atoms() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await;
    let p = temp.path();
    // A commit with a subject and a body paragraph (single message with a
    // blank-line separator).
    std::fs::write(p.join("x.txt"), "x\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "x.txt"], p), "add x.txt");
    // `--cleanup=verbatim` preserves the blank line separating subject/body
    // (the default cleanup would collapse it).
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "-m",
                "the subject\n\nthe body",
                "--cleanup=verbatim",
                "--no-verify",
            ],
            p,
        ),
        "commit subject+body",
    );

    let field = |spec: &str| {
        let fmt = format!("--format=[%({spec})]");
        let out = run_libra_command(&["for-each-ref", "--heads", &fmt], p);
        assert_cli_success(&out, spec);
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    // %(contents:subject) is the subject only.
    let subj = field("contents:subject");
    assert!(subj.contains("the subject"), "subject present: {subj:?}");
    assert!(
        !subj.contains("the body"),
        "subject excludes body: {subj:?}"
    );
    // %(body) is the body only.
    let body = field("body");
    assert!(body.contains("the body"), "body present: {body:?}");
    assert!(
        !body.contains("the subject"),
        "body excludes subject: {body:?}"
    );
    // %(contents) has both.
    let contents = field("contents");
    assert!(
        contents.contains("the subject") && contents.contains("the body"),
        "contents has subject and body: {contents:?}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_objectname_short_n() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await;
    let p = temp.path();

    let full = {
        let out = run_libra_command(&["for-each-ref", "--heads", "--format=%(objectname)"], p);
        assert_cli_success(&out, "objectname");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .to_string()
    };
    let field = |spec: &str| {
        let fmt = format!("--format=%(objectname:short={spec})");
        let out = run_libra_command(&["for-each-ref", "--heads", &fmt], p);
        assert_cli_success(&out, spec);
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .to_string()
    };
    let s10 = field("10");
    assert_eq!(s10.len(), 10, "short=10 yields 10 chars: {s10:?}");
    assert!(
        full.starts_with(&s10),
        "short=10 is a prefix of the full oid"
    );
    let s4 = field("4");
    assert_eq!(s4.len(), 4, "short=4 yields 4 chars: {s4:?}");
    // N beyond the hash length yields the full oid.
    let big = field("64");
    assert_eq!(big, full, "short=64 yields the full oid for sha1");
}

#[test]
fn test_for_each_ref_exclude_filter() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    for b in ["feature-a", "feature-b", "release-x"] {
        assert_cli_success(&run_libra_command(&["branch", b], p), "branch");
    }

    // Without --exclude, all heads are listed.
    let all = run_libra_command(&["for-each-ref", "--heads"], p);
    assert_cli_success(&all, "for-each-ref --heads");
    let all_s = String::from_utf8_lossy(&all.stdout);
    assert!(
        all_s.contains("feature-a") && all_s.contains("release-x"),
        "all heads listed: {all_s}"
    );

    // --exclude drops refs whose name matches the pattern (applied after includes).
    let ex = run_libra_command(&["for-each-ref", "--heads", "--exclude", "feature"], p);
    assert_cli_success(&ex, "for-each-ref --exclude");
    let ex_s = String::from_utf8_lossy(&ex.stdout);
    assert!(
        !ex_s.contains("feature-a") && !ex_s.contains("feature-b"),
        "feature refs excluded: {ex_s}"
    );
    assert!(
        ex_s.contains("release-x") && ex_s.contains("main"),
        "non-matching refs kept: {ex_s}"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_for_each_ref_sort_by_committerdate() {
    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let _guard = test::ChangeDirGuard::new(temp.path());
    let p = temp.path();

    // c1 on main, then branch `older` at c1.
    std::fs::write("a.txt", "1\n").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["a.txt".into()],
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
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("c1".into()),
        no_verify: true,
        ..Default::default()
    })
    .await;
    assert_cli_success(&run_libra_command(&["branch", "older"], p), "branch older");

    // Ensure c2's committer timestamp is at least one whole second later than
    // c1's, so the date ordering is unambiguous (commit timestamps are
    // second-granularity).
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    std::fs::write("a.txt", "2\n").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["a.txt".into()],
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
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("c2".into()),
        no_verify: true,
        ..Default::default()
    })
    .await;
    assert_cli_success(&run_libra_command(&["branch", "newer"], p), "branch newer");

    let heads = |args: &[&str]| -> Vec<String> {
        let mut full = vec!["for-each-ref", "--heads", "--format=%(refname:short)"];
        full.extend_from_slice(args);
        let out = run_libra_command(&full, p);
        assert_cli_success(&out, "for-each-ref date sort");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    };

    // Ascending: `older` (c1) first; `main` and `newer` (both c2) tie-break by
    // refname ascending.
    assert_eq!(
        heads(&["--sort=committerdate"]),
        vec!["older".to_string(), "main".to_string(), "newer".to_string()],
    );
    // Descending reverses the date order; the c2 tie still breaks by refname.
    assert_eq!(
        heads(&["--sort=-committerdate"]),
        vec!["main".to_string(), "newer".to_string(), "older".to_string()],
    );
    // authordate and creatordate (on commits) order the same as committerdate here.
    assert_eq!(
        heads(&["--sort=authordate"]),
        vec!["older".to_string(), "main".to_string(), "newer".to_string()],
    );
    assert_eq!(
        heads(&["--sort=creatordate"]),
        vec!["older".to_string(), "main".to_string(), "newer".to_string()],
    );

    // An unknown sort key is still rejected.
    let bad = run_libra_command(&["for-each-ref", "--sort=bogus"], p);
    assert_eq!(bad.status.code(), Some(129));
}

#[tokio::test]
#[serial(cwd)]
async fn test_for_each_ref_sort_creatordate_uses_tagger_date_for_annotated_tags() {
    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let _guard = test::ChangeDirGuard::new(temp.path());
    let p = temp.path();
    assert_cli_success(
        &run_libra_command(&["config", "user.name", "T"], p),
        "user.name",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "t@t"], p),
        "user.email",
    );

    // c1, remember its hash, and branch `bbb` at it.
    std::fs::write("a.txt", "1\n").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["a.txt".into()],
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
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("c1".into()),
        no_verify: true,
        ..Default::default()
    })
    .await;
    let c1 = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    assert_cli_success(&run_libra_command(&["branch", "bbb"], p), "branch bbb");

    // c2 strictly later, on main.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    std::fs::write("a.txt", "2\n").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["a.txt".into()],
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
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("c2".into()),
        no_verify: true,
        ..Default::default()
    })
    .await;

    // Strictly later still, create an ANNOTATED tag pointing back at c1 (detach
    // HEAD to c1 first, since `libra tag` tags HEAD). Its tagger date is now the
    // latest timestamp, while it peels to c1 (the earliest commit).
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert_cli_success(&run_libra_command(&["checkout", &c1], p), "detach to c1");
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "annotated aaa", "aaa"], p),
        "annotated tag aaa",
    );

    let order = |args: &[&str]| -> Vec<String> {
        let mut full = vec!["for-each-ref", "--format=%(refname:short)"];
        full.extend_from_slice(args);
        let out = run_libra_command(&full, p);
        assert_cli_success(&out, "for-each-ref date sort");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    };

    // committerdate / authordate PEEL the annotated tag to its commit (c1, the
    // earliest), so `aaa` sorts with the c1-era refs — `bbb`(c1) then `aaa`(→c1,
    // tie broken by full refname refs/heads/bbb < refs/tags/aaa), then `main`(c2).
    let by_committer = order(&["--sort=committerdate"]);
    assert_eq!(
        by_committer,
        vec!["bbb".to_string(), "aaa".to_string(), "main".to_string()],
        "committerdate peels the tag to c1"
    );
    assert_eq!(
        order(&["--sort=authordate"]),
        by_committer,
        "authordate also peels the tag to c1 (commits set author == committer)"
    );

    // creatordate uses the annotated tag's OWN tagger date (the latest), so `aaa`
    // sorts last instead — distinguishing it from committerdate.
    assert_eq!(
        order(&["--sort=creatordate"]),
        vec!["bbb".to_string(), "main".to_string(), "aaa".to_string()],
        "creatordate uses the tag's tagger date"
    );
}

#[tokio::test]
#[serial(hash_kind, cwd)]
async fn test_for_each_ref_sort_peels_nested_annotated_tags() {
    use libra::{
        command::for_each_ref::MAX_TAG_PEEL_DEPTH,
        internal::{db::get_db_conn_instance, model::reference},
    };
    use sea_orm::{ActiveModelTrait, Set};

    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let _guard = test::ChangeDirGuard::new(temp.path());
    let p = temp.path();

    // A single real commit c1; `main` and `bbb` both point at it.
    std::fs::write("a.txt", "1\n").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["a.txt".into()],
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
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("c1".into()),
        no_verify: true,
        ..Default::default()
    })
    .await;
    assert_cli_success(&run_libra_command(&["branch", "bbb"], p), "branch bbb");

    // Craft a NESTED annotated-tag chain (libra's `tag` cannot produce tag→tag)
    // of exactly MAX_TAG_PEEL_DEPTH levels, ending at c1:
    //   outer == t[N-1] (tag) -> t[N-2] (tag) -> ... -> t[0] (tag) -> c1 (commit)
    // This exercises the deepest chain `peel_to_commit` must still resolve (a
    // one-level peel — or an off-by-one bound — leaves `outer` at timestamp 0).
    // The crafted tagger timestamp is 1 (earliest possible) while c1's commit
    // date is "now" (latest), so committerdate/authordate must peel `outer` all
    // the way to c1 (sorting it with the c1-era refs), whereas creatordate uses
    // `outer`'s own tagger date (1) and sorts it first.
    let _hash_guard = set_hash_kind_for_test(HashKind::Sha1);
    let c1 = Head::current_commit().await.expect("HEAD commit");
    let tagger = || Signature {
        signature_type: SignatureType::Tagger,
        name: "t".to_string(),
        email: "t@t".to_string(),
        timestamp: 1,
        timezone: "+0000".to_string(),
    };
    let mut target = c1;
    let mut target_type = ObjectType::Commit;
    for i in 0..MAX_TAG_PEEL_DEPTH {
        let tag = GitTag::new(
            target,
            target_type,
            format!("t{i}"),
            tagger(),
            format!("t{i}"),
        );
        save_object(&tag, &tag.id).expect("save nested tag object");
        target = tag.id;
        target_type = ObjectType::Tag;
    }
    // `target` is the outermost tag; peeling it requires MAX_TAG_PEEL_DEPTH
    // dereferences to reach c1.
    let db = get_db_conn_instance().await;
    reference::ActiveModel {
        name: Set(Some("refs/tags/outer".to_string())),
        kind: Set(reference::ConfigKind::Tag),
        commit: Set(Some(target.to_string())),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("register refs/tags/outer");

    let order = |args: &[&str]| -> Vec<String> {
        let mut full = vec!["for-each-ref", "--format=%(refname:short)"];
        full.extend_from_slice(args);
        let out = run_libra_command(&full, p);
        assert_cli_success(&out, "for-each-ref nested date sort");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    };

    // committerdate/authordate peel outer -> inner -> c1, so `outer` sorts at
    // c1's (latest) date with `bbb`/`main` (all c1, tie broken by full refname:
    // refs/heads/bbb < refs/heads/main < refs/tags/outer).
    let expected_peeled = vec!["bbb".to_string(), "main".to_string(), "outer".to_string()];
    assert_eq!(
        order(&["--sort=committerdate"]),
        expected_peeled,
        "committerdate peels the nested tag all the way to c1"
    );
    assert_eq!(
        order(&["--sort=authordate"]),
        expected_peeled,
        "authordate likewise peels the nested tag to c1"
    );
    // creatordate uses `outer`'s own tagger date (1, the earliest), so it leads.
    assert_eq!(
        order(&["--sort=creatordate"]),
        vec!["outer".to_string(), "bbb".to_string(), "main".to_string()],
        "creatordate uses the outer tag's tagger date, not the peeled commit"
    );
}

#[test]
fn test_for_each_ref_quoting_styles() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // A commit whose subject contains a single quote, to exercise escaping.
    std::fs::write(p.join("q.txt"), "x\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "q.txt"], p), "add q");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "it's a test", "--no-verify"], p),
        "commit q",
    );

    let line = |args: &[&str]| -> String {
        let out = run_libra_command(args, p);
        assert_cli_success(&out, "for-each-ref quoting");
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    };

    // --shell quotes each interpolated field; literal format text (the space)
    // stays unquoted.
    assert_eq!(
        line(&[
            "for-each-ref",
            "--shell",
            "--format=%(refname:short) %(objecttype)",
            "refs/heads/main",
        ]),
        "'main' 'commit'"
    );
    // A single quote in the value escapes as the classic '\'' sequence.
    assert_eq!(
        line(&[
            "for-each-ref",
            "--shell",
            "--format=%(contents:subject)",
            "refs/heads/main",
        ]),
        "'it'\\''s a test'"
    );
    // --tcl wraps in double quotes.
    assert_eq!(
        line(&[
            "for-each-ref",
            "--tcl",
            "--format=%(refname)",
            "refs/heads/main",
        ]),
        "\"refs/heads/main\""
    );
    // --perl single-quotes (backslash/quote escaped); refname has none here.
    assert_eq!(
        line(&[
            "for-each-ref",
            "--perl",
            "--format=%(refname)",
            "refs/heads/main",
        ]),
        "'refs/heads/main'"
    );
    // The default format (no --format) quotes its two fields independently.
    let def = line(&["for-each-ref", "--shell", "refs/heads/main"]);
    assert!(
        def.starts_with('\'') && def.ends_with("' 'refs/heads/main'"),
        "default fields quoted: {def}"
    );

    // Shell also escapes `!` (git's sq_quote_buf): `!` → `'\!'`.
    std::fs::write(p.join("b.txt"), "y\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "b.txt"], p), "add b");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "bang! here", "--no-verify"], p),
        "commit bang",
    );
    assert_eq!(
        line(&[
            "for-each-ref",
            "--shell",
            "--format=%(contents:subject)",
            "refs/heads/main",
        ]),
        "'bang'\\!' here'"
    );

    // A multi-line commit message: `%(contents)` then spans physical newlines.
    std::fs::write(p.join("c.txt"), "z\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "c.txt"], p), "add c");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "ml subject\nml body", "--no-verify"], p),
        "commit ml",
    );
    // Python converts each newline to a literal `\n`, keeping a single-line
    // Python string literal.
    let py = String::from_utf8_lossy(
        &run_libra_command(
            &[
                "for-each-ref",
                "--python",
                "--format=%(contents)",
                "refs/heads/main",
            ],
            p,
        )
        .stdout,
    )
    .trim_end()
    .to_string();
    assert!(
        !py.contains('\n') && py.contains("\\n") && py.contains("ml subject"),
        "python escapes the newline to \\n: {py:?}"
    );
    // Perl leaves the newline physical (output spans multiple lines).
    let perl = String::from_utf8_lossy(
        &run_libra_command(
            &[
                "for-each-ref",
                "--perl",
                "--format=%(contents)",
                "refs/heads/main",
            ],
            p,
        )
        .stdout,
    )
    .to_string();
    assert!(
        perl.contains("ml subject\nml body"),
        "perl keeps the newline physical: {perl:?}"
    );

    // Two quoting styles are mutually exclusive (clap usage error, exit 129).
    let conflict = run_libra_command(
        &["for-each-ref", "--shell", "--tcl", "--format=%(refname)"],
        p,
    );
    assert_eq!(conflict.status.code(), Some(129));
}

#[test]
fn test_for_each_ref_objectsize_atom_and_sort() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // Two annotated tags whose object sizes differ only by message length, so
    // `small` < `big` deterministically. (Commit signing makes commit sizes an
    // unreliable baseline, so the sort assertion scopes to `--tags`.)
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "s", "small"], p),
        "small tag",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "tag",
                "-m",
                "an annotated tag with a deliberately long message body to inflate its object size",
                "big",
            ],
            p,
        ),
        "big tag",
    );

    // `%(objectsize)` is the size of the tag object itself; the longer message
    // makes `big` larger than `small`.
    let size = |reff: &str| -> i64 {
        let out = run_libra_command(&["for-each-ref", reff, "--format=%(objectsize)"], p);
        assert_cli_success(&out, "objectsize atom");
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("objectsize is numeric")
    };
    let small_size = size("refs/tags/small");
    let big_size = size("refs/tags/big");
    assert!(small_size > 0, "tag objectsize is positive: {small_size}");
    assert!(
        big_size > small_size,
        "the longer-message tag ({big_size}) is larger than the short one ({small_size})"
    );

    // `--sort=objectsize` orders the tags ascending (small first); `-objectsize`
    // reverses.
    let tags = |args: &[&str]| -> Vec<String> {
        let mut full = vec!["for-each-ref", "--tags", "--format=%(refname:short)"];
        full.extend_from_slice(args);
        let out = run_libra_command(&full, p);
        assert_cli_success(&out, "objectsize sort");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    };
    assert_eq!(
        tags(&["--sort=objectsize"]),
        vec!["small".to_string(), "big".to_string()],
        "ascending objectsize: small before big"
    );
    assert_eq!(
        tags(&["--sort=-objectsize"]),
        vec!["big".to_string(), "small".to_string()],
        "descending objectsize: big before small"
    );
}

#[test]
fn test_for_each_ref_objectsize_errors_on_unreadable_object() {
    use super::{
        assert_cli_success, create_committed_repo_via_cli, loose_object_path, run_libra_command,
    };

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    let head = run_libra_command(&["rev-parse", "HEAD"], p);
    assert_cli_success(&head, "rev-parse HEAD");
    let hash = String::from_utf8_lossy(&head.stdout).trim().to_string();

    // Remove the commit's loose object so refs/heads/main points at an object
    // that cannot be read.
    std::fs::remove_file(loose_object_path(p, &hash)).expect("remove loose object");

    // `%(objectsize)` surfaces a read error (not a silent 0) naming the ref.
    let atom = run_libra_command(
        &["for-each-ref", "refs/heads/main", "--format=%(objectsize)"],
        p,
    );
    assert!(
        !atom.status.success(),
        "%(objectsize) must error on an unreadable object, got: {}",
        String::from_utf8_lossy(&atom.stdout)
    );
    assert!(
        String::from_utf8_lossy(&atom.stderr).contains("refs/heads/main"),
        "the error names the ref: {}",
        String::from_utf8_lossy(&atom.stderr)
    );

    // `--sort=objectsize` likewise errors rather than treating the size as 0.
    let sort = run_libra_command(&["for-each-ref", "--sort=objectsize"], p);
    assert!(
        !sort.status.success(),
        "--sort=objectsize must error on an unreadable object"
    );
}

#[test]
fn test_for_each_ref_deref_objectname_atom_and_sort() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // An annotated tag dereferences to the commit; a lightweight tag and the
    // branch do not (their %(*objectname) is empty).
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "annotated", "atag"], p),
        "annotated tag",
    );
    assert_cli_success(&run_libra_command(&["tag", "lw"], p), "lightweight tag");

    let field = |reff: &str, fmt: &str| -> String {
        let out = run_libra_command(&["for-each-ref", reff, &format!("--format={fmt}")], p);
        assert_cli_success(&out, "for-each-ref field");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    // `%(*objectname)` of the annotated tag equals the commit it points at; it is
    // empty for the branch and the lightweight tag.
    let commit = field("refs/heads/main", "%(objectname)");
    assert!(!commit.is_empty(), "commit objectname is non-empty");
    assert_eq!(
        field("refs/tags/atag", "%(*objectname)"),
        commit,
        "annotated tag dereferences to the commit"
    );
    assert_eq!(
        field("refs/tags/atag", "%(*objectname:short)"),
        commit.chars().take(7).collect::<String>(),
        "%(*objectname:short) is the 7-char abbreviation"
    );
    assert_eq!(
        field("refs/heads/main", "%(*objectname)"),
        "",
        "a branch has no dereferenced object"
    );
    assert_eq!(
        field("refs/tags/lw", "%(*objectname)"),
        "",
        "a lightweight tag has no dereferenced object"
    );

    // `--sort=*objectname`: refs with an empty dereference sort first, so the
    // annotated tag (the only non-empty one) comes last; `-*objectname` reverses.
    let order = |args: &[&str]| -> Vec<String> {
        let mut full = vec!["for-each-ref", "--format=%(refname:short)"];
        full.extend_from_slice(args);
        let out = run_libra_command(&full, p);
        assert_cli_success(&out, "deref sort");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    };
    assert_eq!(
        order(&["--sort=*objectname"]).last().map(String::as_str),
        Some("atag"),
        "ascending: annotated tag (non-empty deref) sorts last"
    );
    assert_eq!(
        order(&["--sort=-*objectname"]).first().map(String::as_str),
        Some("atag"),
        "descending: annotated tag sorts first"
    );
}

#[test]
fn test_for_each_ref_deref_objecttype_and_objectsize_atoms() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "annotated", "atag"], p),
        "annotated tag",
    );
    assert_cli_success(&run_libra_command(&["tag", "lw"], p), "lightweight tag");

    let field = |reff: &str, fmt: &str| -> String {
        let out = run_libra_command(&["for-each-ref", reff, &format!("--format={fmt}")], p);
        assert_cli_success(&out, "for-each-ref field");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    // The annotated tag dereferences to the commit: its *objecttype is `commit`
    // and its *objectsize equals the commit's own objectsize.
    assert_eq!(
        field("refs/tags/atag", "%(*objecttype)"),
        "commit",
        "annotated tag dereferences to a commit"
    );
    let commit_size = field("refs/heads/main", "%(objectsize)");
    assert!(!commit_size.is_empty(), "commit objectsize is present");
    assert_eq!(
        field("refs/tags/atag", "%(*objectsize)"),
        commit_size,
        "*objectsize is the dereferenced commit's size"
    );

    // Non-tag refs (branch, lightweight tag) have empty *objecttype/*objectsize.
    for reff in ["refs/heads/main", "refs/tags/lw"] {
        assert_eq!(
            field(reff, "%(*objecttype)"),
            "",
            "{reff} has no dereferenced type"
        );
        assert_eq!(
            field(reff, "%(*objectsize)"),
            "",
            "{reff} has no dereferenced size"
        );
    }
}

#[tokio::test]
#[serial(hash_kind, cwd)]
async fn test_for_each_ref_deref_size_errors_on_broken_tag_chain() {
    use libra::internal::{db::get_db_conn_instance, model::reference};
    use sea_orm::{ActiveModelTrait, Set};

    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let _guard = test::ChangeDirGuard::new(temp.path());
    let p = temp.path();

    std::fs::write("a.txt", "1\n").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["a.txt".into()],
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
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("c1".into()),
        no_verify: true,
        ..Default::default()
    })
    .await;

    // Craft a nested tag chain outer(tag) -> inner(tag) -> c1(commit), then
    // DELETE the inner tag object so the chain cannot be peeled.
    let _hash_guard = set_hash_kind_for_test(HashKind::Sha1);
    let c1 = Head::current_commit().await.expect("HEAD commit");
    let tagger = Signature {
        signature_type: SignatureType::Tagger,
        name: "t".to_string(),
        email: "t@t".to_string(),
        timestamp: 1,
        timezone: "+0000".to_string(),
    };
    let inner = GitTag::new(
        c1,
        ObjectType::Commit,
        "inner".to_string(),
        tagger.clone(),
        "inner".to_string(),
    );
    save_object(&inner, &inner.id).expect("save inner tag");
    let outer = GitTag::new(
        inner.id,
        ObjectType::Tag,
        "outer".to_string(),
        tagger,
        "outer".to_string(),
    );
    save_object(&outer, &outer.id).expect("save outer tag");

    let db = get_db_conn_instance().await;
    reference::ActiveModel {
        name: Set(Some("refs/tags/outer".to_string())),
        kind: Set(reference::ConfigKind::Tag),
        commit: Set(Some(outer.id.to_string())),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("register refs/tags/outer");

    // Remove the intermediate tag object: outer still loads (so the ref lists as
    // a tag), but peeling it must read `inner`, which is now gone.
    std::fs::remove_file(super::loose_object_path(p, &inner.id.to_string()))
        .expect("remove inner tag object");

    // `%(*objectsize)` must surface the read failure (naming the ref), NOT render
    // empty like a non-tag ref.
    let out = run_libra_command(
        &["for-each-ref", "refs/tags/outer", "--format=%(*objectsize)"],
        p,
    );
    assert!(
        !out.status.success(),
        "%(*objectsize) must error on a broken tag chain, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("refs/tags/outer"),
        "the error names the ref: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn test_for_each_ref_sort_by_deref_objecttype_and_objectsize() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // Only the annotated tag has a dereferenced object; the branch and the
    // lightweight tag have an empty *objecttype/*objectsize.
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "annotated", "atag"], p),
        "annotated tag",
    );
    assert_cli_success(&run_libra_command(&["tag", "lw"], p), "lightweight tag");

    let order = |key: &str| -> Vec<String> {
        let out = run_libra_command(
            &[
                "for-each-ref",
                &format!("--sort={key}"),
                "--format=%(refname:short)",
            ],
            p,
        );
        assert_cli_success(&out, "deref sort");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    };

    // Refs with an empty dereference sort first (ascending), so the annotated
    // tag (the only non-empty one) comes last; the `-` prefix reverses.
    for key in ["*objecttype", "*objectsize"] {
        assert_eq!(
            order(key).last().map(String::as_str),
            Some("atag"),
            "ascending {key}: annotated tag sorts last"
        );
        assert_eq!(
            order(&format!("-{key}")).first().map(String::as_str),
            Some("atag"),
            "descending {key}: annotated tag sorts first"
        );
    }
}

#[test]
fn test_for_each_ref_align_atom_pads_to_width() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // `main` already exists; add a short and a long branch to exercise padding
    // and the no-truncation rule.
    assert_cli_success(&run_libra_command(&["branch", "ab"], p), "branch ab");
    assert_cli_success(
        &run_libra_command(&["branch", "longbranchname"], p),
        "branch long",
    );

    let render = |fmt: &str, reff: &str| -> String {
        let out = run_libra_command(&["for-each-ref", &format!("--format={fmt}"), reff], p);
        assert_cli_success(&out, "for-each-ref align");
        String::from_utf8_lossy(&out.stdout)
            .trim_end_matches('\n')
            .to_string()
    };

    // left (default), right, middle padding of a 2-char ref to width 6.
    assert_eq!(
        render("[%(align:6,left)%(refname:short)%(end)]", "refs/heads/ab"),
        "[ab    ]"
    );
    assert_eq!(
        render("[%(align:6,right)%(refname:short)%(end)]", "refs/heads/ab"),
        "[    ab]"
    );
    assert_eq!(
        render("[%(align:6,middle)%(refname:short)%(end)]", "refs/heads/ab"),
        "[  ab  ]"
    );
    // width-only defaults to left.
    assert_eq!(
        render("[%(align:4)%(refname:short)%(end)]", "refs/heads/ab"),
        "[ab  ]"
    );
    // key=value form.
    assert_eq!(
        render(
            "[%(align:width=6,position=right)%(refname:short)%(end)]",
            "refs/heads/ab"
        ),
        "[    ab]"
    );
    // middle with odd padding biases the extra space to the right (ab → width 5).
    assert_eq!(
        render("[%(align:5,middle)%(refname:short)%(end)]", "refs/heads/ab"),
        "[ ab  ]"
    );
    // Content wider than the width is not truncated.
    assert_eq!(
        render(
            "[%(align:4,left)%(refname:short)%(end)]",
            "refs/heads/longbranchname"
        ),
        "[longbranchname]"
    );

    // `%(align)` without a matching `%(end)` is a usage error.
    let no_end = run_libra_command(
        &[
            "for-each-ref",
            "--format=%(align:5)%(refname)",
            "refs/heads/ab",
        ],
        p,
    );
    assert_eq!(
        no_end.status.code(),
        Some(129),
        "align without %(end) is a usage error: {}",
        String::from_utf8_lossy(&no_end.stderr)
    );

    // An invalid align spec (no width) is a usage error.
    let bad = run_libra_command(
        &[
            "for-each-ref",
            "--format=%(align:left)%(refname)%(end)",
            "refs/heads/ab",
        ],
        p,
    );
    assert_eq!(bad.status.code(), Some(129), "align without a width errors");

    // Under `--shell` (and the other quote modes) the contents of an align block
    // render raw and the whole padded block is quoted once — matching Git, which
    // quotes only the topmost align block (not per-atom, not the block literals
    // separately). `ab` right-aligned to width 6 → `    ab` → `'    ab'`.
    let shell = run_libra_command(
        &[
            "for-each-ref",
            "--shell",
            "--format=[%(align:6,right)%(refname:short)%(end)]",
            "refs/heads/ab",
        ],
        p,
    );
    assert_cli_success(&shell, "for-each-ref --shell align");
    assert_eq!(
        String::from_utf8_lossy(&shell.stdout).trim_end_matches('\n'),
        "['    ab']"
    );
}

#[test]
fn test_for_each_ref_if_then_else_conditional() {
    use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    // `main` is checked out (so `%(HEAD)` is `*`); `other` is not (so `%(HEAD)`
    // is a single space — exercising the whitespace-trim rule). An annotated tag
    // gives an `objecttype` of `tag` for the equals/notequals comparisons.
    assert_cli_success(&run_libra_command(&["branch", "other"], p), "branch other");
    assert_cli_success(
        &run_libra_command(&["tag", "-m", "annotated", "atag"], p),
        "annotated tag",
    );

    let render = |fmt: &str, reff: &str| -> String {
        let out = run_libra_command(&["for-each-ref", &format!("--format={fmt}"), reff], p);
        assert_cli_success(&out, "for-each-ref if");
        String::from_utf8_lossy(&out.stdout)
            .trim_end_matches('\n')
            .to_string()
    };

    // Plain `%(if)`: a non-empty (after trim) condition picks the then-branch.
    // `%(HEAD)` is `*` for the checked-out branch (truthy) and a space for the
    // other branch (whitespace → treated as empty → else-branch).
    assert_eq!(
        render("%(if)%(HEAD)%(then)Y%(else)N%(end)", "refs/heads/main"),
        "Y"
    );
    assert_eq!(
        render("%(if)%(HEAD)%(then)Y%(else)N%(end)", "refs/heads/other"),
        "N"
    );
    // `%(else)` is optional.
    assert_eq!(
        render("[%(if)%(HEAD)%(then)*%(end)]", "refs/heads/other"),
        "[]"
    );

    // `%(if:equals=…)` / `%(if:notequals=…)` compare the raw rendered value.
    assert_eq!(
        render(
            "%(if:equals=commit)%(objecttype)%(then)C%(else)NC%(end)",
            "refs/heads/main"
        ),
        "C"
    );
    assert_eq!(
        render(
            "%(if:equals=commit)%(objecttype)%(then)C%(else)NC%(end)",
            "refs/tags/atag"
        ),
        "NC"
    );
    assert_eq!(
        render(
            "%(if:notequals=commit)%(objecttype)%(then)NC%(else)C%(end)",
            "refs/tags/atag"
        ),
        "NC"
    );

    // An `%(if)` block nests inside an `%(align)` block and shares the `%(end)`
    // terminator; the chosen branch is what gets padded.
    assert_eq!(
        render(
            "[%(align:6,left)%(if)%(HEAD)%(then)on%(else)off%(end)%(end)]",
            "refs/heads/main"
        ),
        "[on    ]"
    );

    // A conditional nested inside another conditional: the inner `%(then)` /
    // `%(end)` must not be mistaken for the outer block's markers
    // (`find_if_marker` skips markers at depth > 0). Outer holds (HEAD = `*`),
    // then the inner equals-check selects `commit`.
    assert_eq!(
        render(
            "%(if)%(HEAD)%(then)<%(if:equals=commit)%(objecttype)%(then)C%(else)X%(end)>%(else)none%(end)",
            "refs/heads/main"
        ),
        "<C>"
    );
    // The outer else-branch (also containing a nested conditional) is taken when
    // the outer condition is false (HEAD = space on a non-checked-out branch).
    assert_eq!(
        render(
            "%(if)%(HEAD)%(then)yes%(else)[%(if:equals=commit)%(objecttype)%(then)c%(end)]%(end)",
            "refs/heads/other"
        ),
        "[c]"
    );

    // Malformed conditionals are usage errors (exit 129).
    for fmt in [
        "%(if)%(refname)%(end)",                 // %(if) without %(then)
        "%(if)%(refname)%(then)x",               // %(if)/%(then) without %(end)
        "%(then)x",                              // stray %(then)
        "%(if)%(refname)%(else)x%(end)",         // %(else) without %(then)
        "%(if:bogus=1)%(refname)%(then)y%(end)", // invalid condition spec
    ] {
        let out = run_libra_command(
            &[
                "for-each-ref",
                &format!("--format={fmt}"),
                "refs/heads/main",
            ],
            p,
        );
        assert_eq!(
            out.status.code(),
            Some(129),
            "malformed conditional `{fmt}` should be a usage error: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// `%(tree)`, `%(tree:short)`, `%(parent)`, `%(parent:short)`, and
/// `%(numparent)` expand from the ref's commit. Covers a merge commit
/// (multi-parent, space-joined), a root commit (zero parents → empty),
/// full-vs-short prefix matching, and an annotated tag (a non-commit ref →
/// all commit-graph atoms empty).
#[test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
fn test_for_each_ref_commit_graph_atoms() {
    let temp = tempdir().unwrap();
    init_repo_via_cli(temp.path());
    let p = temp.path();
    let run = |a: &[&str]| run_libra_command(a, p);
    let stdout_trim = |a: &[&str]| String::from_utf8_lossy(&run(a).stdout).trim().to_string();
    run(&["config", "set", "user.name", "t"]);
    run(&["config", "set", "user.email", "t@t"]);

    // c1 (root) on main; branch `side` at c1; c2 on main; sideC on side.
    fs::write(p.join("a.txt"), "1\n").unwrap();
    run(&["add", "a.txt"]);
    run(&["commit", "-m", "c1", "--no-verify"]);
    let c1 = stdout_trim(&["rev-parse", "HEAD"]);
    run(&["branch", "side"]);
    fs::write(p.join("a.txt"), "2\n").unwrap();
    run(&["add", "a.txt"]);
    run(&["commit", "-m", "c2", "--no-verify"]);
    let c2 = stdout_trim(&["rev-parse", "HEAD"]);
    run(&["switch", "side"]);
    fs::write(p.join("b.txt"), "s\n").unwrap();
    run(&["add", "b.txt"]);
    run(&["commit", "-m", "sideC", "--no-verify"]);
    let side_c = stdout_trim(&["rev-parse", "HEAD"]);
    run(&["switch", "main"]);
    assert!(
        run(&["merge", "side"]).status.success(),
        "merge side into main"
    );
    // A branch at the root commit so we can inspect a zero-parent ref.
    run(&["branch", "root", &c1]);

    // Merge commit on main: two parents (first-parent = main's c2, then sideC),
    // space-joined as full hashes, with `:short` the 7-char prefixes.
    assert_eq!(
        stdout_trim(&["for-each-ref", "--format=%(numparent)", "refs/heads/main"]),
        "2",
        "merge has two parents"
    );
    assert_eq!(
        stdout_trim(&["for-each-ref", "--format=%(parent)", "refs/heads/main"]),
        format!("{c2} {side_c}"),
        "full parents are space-joined in first-parent order"
    );
    assert_eq!(
        stdout_trim(&[
            "for-each-ref",
            "--format=%(parent:short)",
            "refs/heads/main"
        ]),
        format!("{} {}", &c2[..7], &side_c[..7]),
        "short parents are the 7-char prefixes, space-joined"
    );

    // Root commit: zero parents → %(numparent)=0 and %(parent) empty.
    assert_eq!(
        stdout_trim(&["for-each-ref", "--format=%(numparent)", "refs/heads/root"]),
        "0",
        "root commit has no parents"
    );
    assert_eq!(
        stdout_trim(&["for-each-ref", "--format=[%(parent)]", "refs/heads/root"]),
        "[]",
        "root commit's %(parent) is empty"
    );

    // %(tree) full hash with %(tree:short) its 7-char prefix.
    let trees = stdout_trim(&[
        "for-each-ref",
        "--format=%(tree) %(tree:short)",
        "refs/heads/root",
    ]);
    let parts: Vec<&str> = trees.split(' ').collect();
    assert_eq!(parts.len(), 2, "two fields: {trees}");
    assert_eq!(parts[0].len(), 40, "tree is a full 40-char hash: {trees}");
    assert_eq!(
        parts[1],
        &parts[0][..7],
        "tree:short is the 7-char prefix: {trees}"
    );

    // A non-commit ref (annotated tag, objecttype=tag): all commit-graph atoms
    // are empty.
    assert!(
        run(&["tag", "atag", "-m", "annotated"]).status.success(),
        "create annotated tag"
    );
    assert_eq!(
        stdout_trim(&[
            "for-each-ref",
            "--format=[%(tree)][%(tree:short)][%(parent)][%(parent:short)][%(numparent)]",
            "refs/tags/atag",
        ]),
        "[][][][][]",
        "commit-graph atoms are empty for a non-commit (annotated-tag) ref"
    );
}

/// `%(committerdate:<fmt>)` and friends honor date-format modifiers, reusing the
/// shared timestamp formatter; `%(creatordate)` resolves (committer date for
/// commits, tagger date for annotated tags); an inapplicable date is empty; and
/// `:relative` produces git-style "… ago" output.
#[test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
fn test_for_each_ref_date_format_modifiers() {
    let temp = tempdir().unwrap();
    init_repo_via_cli(temp.path());
    let p = temp.path();
    let run = |a: &[&str]| run_libra_command(a, p);
    let stdout_trim = |a: &[&str]| String::from_utf8_lossy(&run(a).stdout).trim().to_string();
    run(&["config", "set", "user.name", "t"]);
    run(&["config", "set", "user.email", "t@t"]);
    fs::write(p.join("a.txt"), "1\n").unwrap();
    run(&["add", "a.txt"]);
    run(&["commit", "-m", "c1", "--no-verify"]);
    run(&["tag", "annot", "-m", "annotated"]);

    // unix modifier is a bare epoch integer; short is YYYY-MM-DD; iso-strict has a `T`.
    let unix = stdout_trim(&[
        "for-each-ref",
        "--format=%(committerdate:unix)",
        "refs/heads/main",
    ]);
    assert!(
        unix.parse::<u64>().is_ok() && !unix.is_empty(),
        "committerdate:unix is an epoch integer: {unix}"
    );
    let short = stdout_trim(&[
        "for-each-ref",
        "--format=%(committerdate:short)",
        "refs/heads/main",
    ]);
    assert_eq!(short.len(), 10, "short date is YYYY-MM-DD: {short}");
    assert_eq!(
        short.matches('-').count(),
        2,
        "short date has two dashes: {short}"
    );
    let strict = stdout_trim(&[
        "for-each-ref",
        "--format=%(committerdate:iso-strict)",
        "refs/heads/main",
    ]);
    assert!(
        strict.contains('T'),
        "iso-strict uses RFC3339 with T: {strict}"
    );

    // bare committerdate (default format) is unchanged and differs from :unix.
    let default = stdout_trim(&[
        "for-each-ref",
        "--format=%(committerdate)",
        "refs/heads/main",
    ]);
    assert!(
        default.contains("2026") || default.contains("20"),
        "default date: {default}"
    );
    assert_ne!(default, unix, "default format differs from :unix");

    // creatordate resolves for the annotated tag (its tagger date).
    let creator = stdout_trim(&[
        "for-each-ref",
        "--format=%(creatordate:short)",
        "refs/tags/annot",
    ]);
    assert_eq!(
        creator.len(),
        10,
        "creatordate:short for an annotated tag: {creator}"
    );

    // authordate does not apply to an annotated tag → empty.
    let auth_on_tag = stdout_trim(&[
        "for-each-ref",
        "--format=[%(authordate:iso)]",
        "refs/tags/annot",
    ]);
    assert_eq!(
        auth_on_tag, "[]",
        "authordate is empty for a tag: {auth_on_tag}"
    );

    // :relative produces git-style "… ago" wording (the commit is fresh).
    let rel = stdout_trim(&[
        "for-each-ref",
        "--format=%(committerdate:relative)",
        "refs/heads/main",
    ]);
    assert!(rel.ends_with("ago"), "relative date ends with 'ago': {rel}");
}

/// `%(color:<spec>)` emits ANSI escapes when color is enabled (`--color=always`),
/// nothing when disabled (`--color=never`), and rejects an unrecognized color.
#[test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
fn test_for_each_ref_color_atom() {
    let temp = tempdir().unwrap();
    init_repo_via_cli(temp.path());
    let p = temp.path();
    let run = |a: &[&str]| run_libra_command(a, p);
    run(&["config", "set", "user.name", "t"]);
    run(&["config", "set", "user.email", "t@t"]);
    fs::write(p.join("a.txt"), "1\n").unwrap();
    run(&["add", "a.txt"]);
    run(&["commit", "-m", "c1", "--no-verify"]);

    // --color=always emits the escape (red … reset).
    let always = run(&[
        "--color=always",
        "for-each-ref",
        "--format=%(color:red)%(refname:short)%(color:reset)",
        "refs/heads/main",
    ]);
    assert_cli_success(&always, "for-each-ref %(color) --color=always");
    let out = String::from_utf8(always.stdout).unwrap();
    assert_eq!(
        out.trim_end(),
        "\u{1b}[31mmain\u{1b}[m",
        "ANSI red + git reset (ESC[m) emitted: {out:?}"
    );

    // --color=never strips color: just the ref name.
    let never = run(&[
        "--color=never",
        "for-each-ref",
        "--format=%(color:red)%(refname:short)%(color:reset)",
        "refs/heads/main",
    ]);
    assert_cli_success(&never, "for-each-ref %(color) --color=never");
    assert_eq!(
        String::from_utf8(never.stdout).unwrap().trim_end(),
        "main",
        "no escapes under --color=never"
    );

    // An unrecognized color is a format error even when color is off.
    let bad = run(&[
        "--color=never",
        "for-each-ref",
        "--format=%(color:bogus)",
        "refs/heads/main",
    ]);
    assert!(
        !bad.status.success(),
        "bad color spec is rejected: {}",
        String::from_utf8_lossy(&bad.stderr)
    );

    // A third color is rejected (git allows at most foreground + background).
    let too_many = run(&[
        "for-each-ref",
        "--format=%(color:red green blue)",
        "refs/heads/main",
    ]);
    assert!(!too_many.status.success(), "a third color is rejected");

    // A row that leaves color active gets a trailing git reset (ESC[m) appended,
    // so color never bleeds past the record; an explicit trailing reset is not
    // doubled.
    let trailing = run(&[
        "--color=always",
        "for-each-ref",
        "--format=%(color:red)X",
        "refs/heads/main",
    ]);
    assert_cli_success(&trailing, "for-each-ref trailing-reset");
    assert_eq!(
        String::from_utf8(trailing.stdout).unwrap().trim_end(),
        "\u{1b}[31mX\u{1b}[m",
        "trailing git reset appended when color left active"
    );

    // Under `--shell`, the color atom is still quoted like any field, and the
    // appended trailing reset is a separate quoted field (a lone `%(color:red)`
    // leaves color active).
    let shell_on = run(&[
        "--color=always",
        "for-each-ref",
        "--shell",
        "--format=%(color:red)",
        "refs/heads/main",
    ]);
    assert_cli_success(&shell_on, "for-each-ref --shell %(color) --color=always");
    assert_eq!(
        String::from_utf8(shell_on.stdout).unwrap().trim_end(),
        "\'\u{1b}[31m\'\'\u{1b}[m\'",
        "shell mode quotes the color escape and the adjacent (no-space) trailing reset"
    );
    let shell_off = run(&[
        "--color=never",
        "for-each-ref",
        "--shell",
        "--format=%(color:red)",
        "refs/heads/main",
    ]);
    assert_cli_success(&shell_off, "for-each-ref --shell %(color) --color=never");
    assert_eq!(
        String::from_utf8(shell_off.stdout).unwrap().trim_end(),
        "\'\'",
        "shell mode quotes an empty color field when color is off (no trailing reset)"
    );
}

#[test]
fn test_for_each_ref_raw_atom() {
    let temp = tempdir().unwrap();
    init_repo_via_cli(temp.path());
    let p = temp.path();
    let run = |a: &[&str]| run_libra_command(a, p);
    run(&["config", "set", "user.name", "t"]);
    run(&["config", "set", "user.email", "t@t"]);
    fs::write(p.join("a.txt"), "1\n").unwrap();
    run(&["add", "a.txt"]);
    run(&["commit", "-m", "raw subject", "--no-verify"]);

    // %(raw:size) equals %(objectsize) (same decompressed-object byte count).
    let sizes = run(&[
        "for-each-ref",
        "--format=%(raw:size) %(objectsize)",
        "refs/heads/main",
    ]);
    assert_cli_success(&sizes, "for-each-ref %(raw:size)");
    let size_line = String::from_utf8(sizes.stdout)
        .unwrap()
        .trim_end()
        .to_string();
    let mut parts = size_line.split_whitespace();
    let raw_size = parts.next().unwrap_or("");
    let object_size = parts.next().unwrap_or("");
    assert_eq!(raw_size, object_size, "raw:size must equal objectsize");
    assert!(
        raw_size.parse::<u64>().is_ok_and(|n| n > 0),
        "raw:size is a positive integer: {size_line}"
    );

    // %(raw) is the canonical commit object content (tree/author/committer/body).
    let raw = run(&["for-each-ref", "--format=%(raw)", "refs/heads/main"]);
    assert_cli_success(&raw, "for-each-ref %(raw)");
    let raw_text = String::from_utf8(raw.stdout).unwrap();
    assert!(
        raw_text.starts_with("tree "),
        "raw starts with tree: {raw_text:?}"
    );
    assert!(
        raw_text.contains("\nauthor ") && raw_text.contains("\ncommitter "),
        "raw has author/committer lines: {raw_text:?}"
    );
    assert!(
        raw_text.contains("raw subject"),
        "raw has the message: {raw_text:?}"
    );

    // %(raw) is binary-unsafe under quoting: rejected with --shell (exit 128),
    // while %(raw:size) is allowed.
    let bad = run(&[
        "for-each-ref",
        "--shell",
        "--format=%(raw)",
        "refs/heads/main",
    ]);
    assert_eq!(
        bad.status.code(),
        Some(128),
        "%(raw) with --shell should fail (128): {}",
        String::from_utf8_lossy(&bad.stderr)
    );
    let ok = run(&[
        "for-each-ref",
        "--shell",
        "--format=%(raw:size)",
        "refs/heads/main",
    ]);
    assert_cli_success(&ok, "%(raw:size) is allowed with --shell");
}

#[test]
fn test_for_each_ref_raw_rejects_non_utf8() {
    let temp = tempdir().unwrap();
    init_repo_via_cli(temp.path());
    let p = temp.path();
    let run = |a: &[&str]| run_libra_command(a, p);
    run(&["config", "set", "user.name", "t"]);
    run(&["config", "set", "user.email", "t@t"]);
    fs::write(p.join("a.txt"), "1\n").unwrap();
    run(&["add", "a.txt"]);
    run(&["commit", "-m", "c1", "--no-verify"]);

    // Grab the tree of HEAD to build a structurally-valid commit object whose
    // message contains non-UTF-8 bytes.
    let cat = run(&["cat-file", "-p", "HEAD"]);
    let head_body = String::from_utf8_lossy(&cat.stdout);
    let tree = head_body
        .lines()
        .find_map(|l| l.strip_prefix("tree "))
        .expect("HEAD commit has a tree line")
        .to_string();

    // Craft a commit object with an invalid-UTF-8 message and hash it literally.
    let mut commit_bytes =
        format!("tree {tree}\nauthor a <a@b> 1 +0000\ncommitter a <a@b> 1 +0000\n\n").into_bytes();
    commit_bytes.extend_from_slice(&[0xff, 0xfe, 0x00]);
    commit_bytes.extend_from_slice(b"binary message\n");
    let craft = p.join("craft.commit");
    fs::write(&craft, &commit_bytes).unwrap();
    let hashed = run(&[
        "hash-object",
        "-t",
        "commit",
        "--literally",
        "-w",
        craft.to_str().unwrap(),
    ]);
    assert_cli_success(&hashed, "hash-object crafted commit");
    let oid = String::from_utf8_lossy(&hashed.stdout).trim().to_string();

    // Point a branch at the binary commit so for-each-ref will list it.
    assert_cli_success(
        &run(&["branch", "bincommit", &oid]),
        "branch -> binary commit",
    );

    // %(raw) must reject the non-UTF-8 object (rather than emit corrupted bytes).
    let raw = run(&["for-each-ref", "--format=%(raw)", "refs/heads/bincommit"]);
    assert_eq!(
        raw.status.code(),
        Some(129),
        "%(raw) on a non-UTF-8 object should fail (LBR-CLI-002 / 129): {}",
        String::from_utf8_lossy(&raw.stdout)
    );
    let (_h, report) = parse_cli_error_stderr(&raw.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(
        report.message.contains("not valid UTF-8"),
        "error should explain the non-UTF-8 rejection: {}",
        report.message
    );

    // %(raw:size) still reports the true byte length and succeeds.
    let size = run(&[
        "for-each-ref",
        "--format=%(raw:size)",
        "refs/heads/bincommit",
    ]);
    assert_cli_success(&size, "%(raw:size) on a non-UTF-8 object");
    assert!(
        String::from_utf8_lossy(&size.stdout)
            .trim()
            .parse::<u64>()
            .is_ok_and(|n| n > 0),
        "%(raw:size) is a positive byte count even for a binary object"
    );
}

#[test]
fn test_for_each_ref_describe_atom() {
    let temp = tempdir().unwrap();
    init_repo_via_cli(temp.path());
    let p = temp.path();
    let run = |a: &[&str]| run_libra_command(a, p);
    run(&["config", "set", "user.name", "t"]);
    run(&["config", "set", "user.email", "t@t"]);
    fs::write(p.join("a.txt"), "1\n").unwrap();
    run(&["add", "a.txt"]);
    run(&["commit", "-m", "c1", "--no-verify"]);
    // Annotated tag, then a second commit so the branch tip is 1 past the tag.
    assert_cli_success(&run(&["tag", "-m", "rel", "v1.0"]), "annotated tag v1.0");
    fs::write(p.join("a.txt"), "1\n2\n").unwrap();
    run(&["add", "a.txt"]);
    run(&["commit", "-m", "c2", "--no-verify"]);

    let out = |args: &[&str]| {
        let o = run(args);
        (
            String::from_utf8_lossy(&o.stdout).trim().to_string(),
            o.status.code(),
        )
    };

    // %(describe) of a commit one past the annotated tag: "v1.0-1-g<7hex>".
    let (v, code) = out(&["for-each-ref", "--format=%(describe)", "refs/heads/main"]);
    assert_eq!(code, Some(0));
    assert!(
        v.starts_with("v1.0-1-g") && v.len() == "v1.0-1-g".len() + 7,
        "%(describe) should be v1.0-1-g<7hex>: {v:?}"
    );

    // %(describe:abbrev=4) shortens the abbreviated-hash suffix to 4.
    let (v4, _) = out(&[
        "for-each-ref",
        "--format=%(describe:abbrev=4)",
        "refs/heads/main",
    ]);
    assert!(
        v4.starts_with("v1.0-1-g") && v4.len() == "v1.0-1-g".len() + 4,
        "%(describe:abbrev=4) should be v1.0-1-g<4hex>: {v4:?}"
    );

    // %(describe:abbrev=0) collapses to the bare tag name.
    let (v0, _) = out(&[
        "for-each-ref",
        "--format=%(describe:abbrev=0)",
        "refs/heads/main",
    ]);
    assert_eq!(v0, "v1.0", "%(describe:abbrev=0) is the tag name only");

    // A lightweight tag is invisible to bare %(describe) (annotated-only) but
    // visible to %(describe:tags).
    assert_cli_success(&run(&["tag", "lw"]), "lightweight tag");
    fs::write(p.join("a.txt"), "1\n2\n3\n").unwrap();
    run(&["add", "a.txt"]);
    run(&["commit", "-m", "c3", "--no-verify"]);
    let (with_tags, _) = out(&[
        "for-each-ref",
        "--format=%(describe:tags)",
        "refs/heads/main",
    ]);
    assert!(
        with_tags.starts_with("lw-") || with_tags.starts_with("v1.0-"),
        "%(describe:tags) considers the lightweight tag: {with_tags:?}"
    );

    // An unrecognized option is a usage error (exit 129) — validated even when no
    // ref matches.
    let bogus = run(&[
        "for-each-ref",
        "--format=%(describe:bogus)",
        "refs/heads/main",
    ]);
    assert_eq!(
        bogus.status.code(),
        Some(129),
        "bad %(describe) option exits 129"
    );
    assert!(
        String::from_utf8_lossy(&bogus.stderr).contains("unrecognized %(describe) argument"),
        "error names the unrecognized argument"
    );
    let bogus_empty = run(&[
        "for-each-ref",
        "--format=%(describe:bogus)",
        "refs/tags/none",
    ]);
    assert_eq!(
        bogus_empty.status.code(),
        Some(129),
        "%(describe) options are validated up front, even with no matching refs"
    );

    // `--json` ignores `--format` entirely (the describe cache is skipped from the
    // JSON schema), so a bad %(describe) option must NOT fail a JSON listing.
    let json_bogus = run(&[
        "--json",
        "for-each-ref",
        "--format=%(describe:bogus)",
        "refs/heads/main",
    ]);
    assert_eq!(
        json_bogus.status.code(),
        Some(0),
        "--json bypasses --format, so a bad %(describe) option does not fail it: {}",
        String::from_utf8_lossy(&json_bogus.stderr)
    );

    // `--quiet` emits nothing and likewise must not be failed by describe
    // validation/computation.
    let quiet_bogus = run(&[
        "--quiet",
        "for-each-ref",
        "--format=%(describe:bogus)",
        "refs/heads/main",
    ]);
    assert_eq!(
        quiet_bogus.status.code(),
        Some(0),
        "--quiet emits nothing and is not failed by a bad %(describe) option"
    );
    assert!(quiet_bogus.stdout.is_empty(), "--quiet produces no stdout");

    // A commit with no reachable tag describes as an empty string. Craft a
    // parentless root commit (a separate history with no ancestor tag) and point
    // a branch at it — `checkout --orphan` is not available in Libra.
    let tree = {
        let cat = run(&["cat-file", "-p", "HEAD"]);
        String::from_utf8_lossy(&cat.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("tree ").map(str::to_string))
            .expect("HEAD has a tree")
    };
    let root = format!("tree {tree}\nauthor a <a@b> 1 +0000\ncommitter a <a@b> 1 +0000\n\nroot\n");
    let craft = p.join("root.commit");
    fs::write(&craft, root).unwrap();
    let hashed = run(&[
        "hash-object",
        "-t",
        "commit",
        "--literally",
        "-w",
        craft.to_str().unwrap(),
    ]);
    assert_cli_success(&hashed, "hash-object root commit");
    let oid = String::from_utf8_lossy(&hashed.stdout).trim().to_string();
    assert_cli_success(
        &run(&["branch", "rootless", &oid]),
        "branch -> rootless commit",
    );
    let (orphan, code) = out(&[
        "for-each-ref",
        "--format=[%(describe)]",
        "refs/heads/rootless",
    ]);
    assert_eq!(code, Some(0));
    assert_eq!(orphan, "[]", "no reachable tag -> empty %(describe)");
}

#[tokio::test]
#[serial(cwd)]
async fn test_for_each_ref_symref_atom() {
    use libra::internal::branch::Branch;

    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let _guard = test::ChangeDirGuard::new(temp.path());
    let p = temp.path();

    std::fs::write("a.txt", "1\n").unwrap();
    add::execute(AddArgs {
        pathspec: vec!["a.txt".into()],
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
    })
    .await;
    commit::execute(CommitArgs {
        message: Some("c1".into()),
        no_verify: true,
        ..Default::default()
    })
    .await;

    let head = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD"], p).stdout)
        .trim()
        .to_string();
    // A remote-tracking ref, then a symbolic remote HEAD pointing at it.
    Branch::update_branch("refs/remotes/origin/main", &head, Some("origin"))
        .await
        .expect("create remote-tracking origin/main");
    assert_cli_success(
        &run_libra_command(&["remote", "add", "origin", "https://example.com/r.git"], p),
        "remote add",
    );
    assert_cli_success(
        &run_libra_command(&["remote", "set-head", "origin", "main"], p),
        "remote set-head",
    );

    let out = run_libra_command(
        &[
            "for-each-ref",
            "--remotes",
            "--format=%(refname)|%(symref)|%(symref:short)",
        ],
        p,
    );
    assert_cli_success(&out, "for-each-ref %(symref)");
    let s = String::from_utf8_lossy(&out.stdout);
    // The symbolic remote HEAD shows its target; an ordinary remote ref is empty.
    assert!(
        s.lines()
            .any(|l| l == "refs/remotes/origin/HEAD|refs/remotes/origin/main|origin/main"),
        "symbolic remote HEAD exposes its target via %(symref): {s:?}"
    );
    assert!(
        s.lines().any(|l| l == "refs/remotes/origin/main||"),
        "an ordinary remote-tracking ref has an empty %(symref): {s:?}"
    );
}

#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_worktreepath_atom() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await;
    let p = temp.path();
    run_libra_command(&["branch", "feature"], p);
    run_libra_command(&["tag", "v1"], p);

    let out = run_libra_command(&["for-each-ref", "--format=%(refname)|%(worktreepath)"], p);
    assert_cli_success(&out, "for-each-ref %(worktreepath)");
    let s = String::from_utf8_lossy(&out.stdout);
    // The checked-out branch reports the (canonicalized, absolute) current
    // worktree path; every other ref is empty — matching git for a
    // single-worktree repo.
    let want = p.canonicalize().unwrap().to_string_lossy().into_owned();
    assert!(
        s.lines().any(|l| l == format!("refs/heads/main|{want}")),
        "current branch reports the worktree path: {s:?}"
    );
    assert!(
        s.lines().any(|l| l == "refs/heads/feature|"),
        "a non-checked-out branch has an empty %(worktreepath): {s:?}"
    );
    assert!(
        s.lines().any(|l| l == "refs/tags/v1|"),
        "a tag has an empty %(worktreepath): {s:?}"
    );
}

/// Part C §C.3.3: `%(worktreepath)` resolves a branch to the LINKED worktree
/// that has it checked out, even when `for-each-ref` runs in the main worktree.
#[tokio::test]
#[serial(cloud_live, cwd, env, hash_kind, workspace_failpoints)]
async fn test_for_each_ref_worktreepath_across_worktrees() {
    let temp = tempdir().unwrap();
    setup_repo_with_commit(&temp).await;
    let p = temp.path();
    run_libra_command(&["branch", "feature"], p);

    // Add a linked worktree and check out `feature` inside it.
    let parent = tempdir().unwrap();
    let wt = parent.path().join("wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], p),
        "worktree add",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "feature"], &wt),
        "wt switch feature",
    );

    // Run for-each-ref from the MAIN worktree: `feature` must report the
    // linked worktree's canonical path, `main` reports the main path.
    let out = run_libra_command(&["for-each-ref", "--format=%(refname)|%(worktreepath)"], p);
    assert_cli_success(&out, "for-each-ref across worktrees");
    let s = String::from_utf8_lossy(&out.stdout);
    let wt_path = wt.canonicalize().unwrap().to_string_lossy().into_owned();
    let main_path = p.canonicalize().unwrap().to_string_lossy().into_owned();
    assert!(
        s.lines()
            .any(|l| l == format!("refs/heads/feature|{wt_path}")),
        "feature reports the linked worktree path: {s:?}"
    );
    assert!(
        s.lines()
            .any(|l| l == format!("refs/heads/main|{main_path}")),
        "main reports the main worktree path: {s:?}"
    );
}
