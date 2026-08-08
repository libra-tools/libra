//! Integration tests for `libra update-ref`.
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network).

use std::{fs, process::Output};

use tempfile::TempDir;

use super::{create_committed_repo_via_cli, parse_json_stdout, run_libra_command};

const ZERO_SHA1: &str = "0000000000000000000000000000000000000000";

fn stdout_trimmed(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn rev_parse(repo: &TempDir, rev: &str) -> Output {
    run_libra_command(&["rev-parse", rev], repo.path())
}

/// A repo with two distinct commits; returns `(repo, first_oid, second_oid)`.
fn repo_with_two_commits() -> (TempDir, String, String) {
    let repo = create_committed_repo_via_cli();
    let c1 = stdout_trimmed(&rev_parse(&repo, "HEAD"));

    fs::write(repo.path().join("tracked.txt"), "second\n").unwrap();
    let add = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert!(add.status.success());
    let commit = run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path());
    assert!(
        commit.status.success(),
        "second commit failed: {}",
        String::from_utf8_lossy(&commit.stderr)
    );
    let c2 = stdout_trimmed(&rev_parse(&repo, "HEAD"));
    assert_ne!(c1, c2, "expected two distinct commits");
    (repo, c1, c2)
}

#[test]
fn creates_a_new_branch_ref() {
    let (repo, c1, _c2) = repo_with_two_commits();
    let out = run_libra_command(&["update-ref", "refs/heads/feature", &c1], repo.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_trimmed(&rev_parse(&repo, "feature")), c1);
}

#[test]
fn updates_an_existing_branch_ref() {
    let (repo, c1, c2) = repo_with_two_commits();
    run_libra_command(&["update-ref", "refs/heads/feature", &c1], repo.path());
    let out = run_libra_command(&["update-ref", "refs/heads/feature", &c2], repo.path());
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout_trimmed(&rev_parse(&repo, "feature")), c2);
}

#[test]
fn compare_and_swap_succeeds_when_old_matches() {
    let (repo, c1, c2) = repo_with_two_commits();
    run_libra_command(&["update-ref", "refs/heads/feature", &c1], repo.path());
    let out = run_libra_command(&["update-ref", "refs/heads/feature", &c2, &c1], repo.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "CAS should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_trimmed(&rev_parse(&repo, "feature")), c2);
}

#[test]
fn compare_and_swap_fails_when_old_mismatches() {
    let (repo, c1, c2) = repo_with_two_commits();
    run_libra_command(&["update-ref", "refs/heads/feature", &c1], repo.path());
    // Current is c1, but we claim it is c2.
    let out = run_libra_command(&["update-ref", "refs/heads/feature", &c2, &c2], repo.path());
    assert_eq!(out.status.code(), Some(128), "CAS mismatch must fail");
    // The ref is unchanged.
    assert_eq!(stdout_trimmed(&rev_parse(&repo, "feature")), c1);
}

#[test]
fn zero_old_value_creates_only_when_absent() {
    let (repo, c1, _c2) = repo_with_two_commits();
    let create = run_libra_command(
        &["update-ref", "refs/heads/fresh", &c1, ZERO_SHA1],
        repo.path(),
    );
    assert_eq!(
        create.status.code(),
        Some(0),
        "create-only should succeed when absent: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    // Now it exists; a second create-only must fail.
    let again = run_libra_command(
        &["update-ref", "refs/heads/fresh", &c1, ZERO_SHA1],
        repo.path(),
    );
    assert_eq!(
        again.status.code(),
        Some(128),
        "create-only must fail when present"
    );
}

#[test]
fn deletes_a_branch_ref() {
    let (repo, c1, _c2) = repo_with_two_commits();
    run_libra_command(&["update-ref", "refs/heads/feature", &c1], repo.path());
    let del = run_libra_command(&["update-ref", "-d", "refs/heads/feature"], repo.path());
    assert_eq!(
        del.status.code(),
        Some(0),
        "delete failed: {}",
        String::from_utf8_lossy(&del.stderr)
    );
    assert_ne!(
        rev_parse(&repo, "feature").status.code(),
        Some(0),
        "deleted ref should no longer resolve"
    );
}

#[test]
fn delete_with_mismatched_old_fails() {
    let (repo, c1, c2) = repo_with_two_commits();
    run_libra_command(&["update-ref", "refs/heads/feature", &c1], repo.path());
    let del = run_libra_command(
        &["update-ref", "-d", "refs/heads/feature", &c2],
        repo.path(),
    );
    assert_eq!(
        del.status.code(),
        Some(128),
        "delete CAS mismatch must fail"
    );
    // Still present.
    assert_eq!(stdout_trimmed(&rev_parse(&repo, "feature")), c1);
}

#[test]
fn deleting_a_missing_ref_fails() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    let del = run_libra_command(&["update-ref", "-d", "refs/heads/ghost"], repo.path());
    assert_eq!(del.status.code(), Some(128));
}

#[test]
fn rejects_head() {
    let (repo, c1, _c2) = repo_with_two_commits();
    let out = run_libra_command(&["update-ref", "HEAD", &c1], repo.path());
    assert_eq!(out.status.code(), Some(128), "HEAD must be rejected");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("HEAD"),
        "error should mention HEAD: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn rejects_non_heads_namespace() {
    let (repo, c1, _c2) = repo_with_two_commits();
    let out = run_libra_command(&["update-ref", "refs/tags/v1", &c1], repo.path());
    assert_eq!(
        out.status.code(),
        Some(128),
        "refs/tags/* must be rejected in v1"
    );
}

#[test]
fn rejects_invalid_object_id() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    let out = run_libra_command(
        &["update-ref", "refs/heads/feature", "deadbeef"],
        repo.path(),
    );
    assert_eq!(out.status.code(), Some(128));
}

#[test]
fn rejects_nonexistent_new_object() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    // A syntactically valid id that is not present in the object store: Git's
    // update-ref refuses to create such a dangling ref.
    let ghost = "a".repeat(40);
    let out = run_libra_command(&["update-ref", "refs/heads/feature", &ghost], repo.path());
    assert_eq!(
        out.status.code(),
        Some(128),
        "update-ref to a nonexistent object must fail: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn rejects_symbolic_ref_value() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    let out = run_libra_command(
        &["update-ref", "refs/heads/feature", "ref:refs/heads/main"],
        repo.path(),
    );
    assert_eq!(out.status.code(), Some(128), "ref: values must be rejected");
}

#[test]
fn json_output_reports_old_and_new() {
    let (repo, c1, c2) = repo_with_two_commits();
    run_libra_command(&["update-ref", "refs/heads/feature", &c1], repo.path());
    let out = run_libra_command(
        &["--json", "update-ref", "refs/heads/feature", &c2],
        repo.path(),
    );
    assert_eq!(out.status.code(), Some(0));
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["ref"].as_str(), Some("refs/heads/feature"));
    assert_eq!(json["data"]["old"].as_str(), Some(c1.as_str()));
    assert_eq!(json["data"]["new"].as_str(), Some(c2.as_str()));
    assert_eq!(json["data"]["deleted"].as_bool(), Some(false));
}

#[test]
fn outside_repository_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_libra_command(&["update-ref", "refs/heads/x", ZERO_SHA1], dir.path());
    assert_eq!(out.status.code(), Some(128));
}

// ─────────────────────────────────────────────────────────────────────────────
// CT1-02 (plan-20260729): the `<newvalue>` operand goes through the shared
// revision engine, with NO implicit peel. Whatever the expression names must
// itself be a commit — Git's rule, which is why a lightweight tag works, a
// bare annotated tag does not, and `<tag>^{commit}` does.
// ─────────────────────────────────────────────────────────────────────────────

/// stderr of a run, for asserting on stable error codes and messages.
fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Point `refs/heads/feature` at `value` and return the run.
fn update_feature(repo: &TempDir, value: &str) -> Output {
    run_libra_command(&["update-ref", "refs/heads/feature", value], repo.path())
}

/// The commit `refs/heads/feature` currently points at.
fn feature_tip(repo: &TempDir) -> String {
    stdout_trimmed(&rev_parse(repo, "refs/heads/feature"))
}

#[test]
fn update_ref_revision_head_accepted() {
    let (repo, _c1, c2) = repo_with_two_commits();
    let out = update_feature(&repo, "HEAD");
    assert_eq!(out.status.code(), Some(0), "HEAD: {}", stderr_of(&out));
    assert_eq!(feature_tip(&repo), c2, "HEAD resolves to the current tip");
}

#[test]
fn update_ref_revision_branch_name_accepted() {
    let (repo, _c1, c2) = repo_with_two_commits();
    let out = update_feature(&repo, "main");
    assert_eq!(out.status.code(), Some(0), "branch: {}", stderr_of(&out));
    assert_eq!(feature_tip(&repo), c2);
}

#[test]
fn update_ref_revision_lightweight_tag_accepted() {
    let (repo, _c1, c2) = repo_with_two_commits();
    // `libra tag <name>` tags HEAD and, without `-m`, is lightweight: the tag
    // ref holds the commit id directly, so no peeling is involved.
    let tag = run_libra_command(&["tag", "light"], repo.path());
    assert_eq!(tag.status.code(), Some(0), "tag: {}", stderr_of(&tag));

    let out = update_feature(&repo, "light");
    assert_eq!(
        out.status.code(),
        Some(0),
        "lightweight tag: {}",
        stderr_of(&out)
    );
    assert_eq!(feature_tip(&repo), c2);
}

#[test]
fn update_ref_revision_parent_revision_accepted() {
    let (repo, c1, _c2) = repo_with_two_commits();
    let out = update_feature(&repo, "HEAD^");
    assert_eq!(out.status.code(), Some(0), "HEAD^: {}", stderr_of(&out));
    assert_eq!(feature_tip(&repo), c1, "HEAD^ is the first commit");
}

#[test]
fn update_ref_revision_ancestor_revision_accepted() {
    let (repo, c1, _c2) = repo_with_two_commits();
    let out = update_feature(&repo, "HEAD~1");
    assert_eq!(out.status.code(), Some(0), "HEAD~1: {}", stderr_of(&out));
    assert_eq!(feature_tip(&repo), c1);
}

#[test]
fn update_ref_revision_abbreviated_oid_accepted() {
    let (repo, c1, _c2) = repo_with_two_commits();
    let short = &c1[..8];
    let out = update_feature(&repo, short);
    assert_eq!(
        out.status.code(),
        Some(0),
        "abbreviated oid {short}: {}",
        stderr_of(&out)
    );
    assert_eq!(feature_tip(&repo), c1);
}

#[test]
fn update_ref_revision_annotated_tag_rejected() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    // `-m` is what makes a Libra tag annotated: the ref then names a tag
    // OBJECT, not the commit.
    let tag = run_libra_command(&["tag", "-m", "release", "annotated"], repo.path());
    assert_eq!(
        tag.status.code(),
        Some(0),
        "annotated tag: {}",
        stderr_of(&tag)
    );

    let out = update_feature(&repo, "annotated");
    assert_eq!(
        out.status.code(),
        Some(128),
        "a bare annotated tag must be refused, as Git refuses it"
    );
    let stderr = stderr_of(&out);
    // AC: the refusal names the type that was resolved, so the user can see
    // why it was refused and what to do instead.
    assert!(
        stderr.contains("tag"),
        "the refusal must name the resolved object type: {stderr}"
    );
    assert!(
        stderr.contains("not a commit"),
        "the refusal must say what was expected: {stderr}"
    );
    assert!(
        stderr.contains("LBR-CLI-003"),
        "an unusable target is LBR-CLI-003: {stderr}"
    );
    // And nothing was written.
    let after = run_libra_command(&["rev-parse", "refs/heads/feature"], repo.path());
    assert_ne!(
        after.status.code(),
        Some(0),
        "the refused update must not have created the ref"
    );
}

#[test]
fn update_ref_revision_explicit_peel_accepted() {
    let (repo, _c1, c2) = repo_with_two_commits();
    let tag = run_libra_command(&["tag", "-m", "release", "annotated"], repo.path());
    assert_eq!(tag.status.code(), Some(0), "tag: {}", stderr_of(&tag));

    // Explicit peel is how the user says "I mean the commit".
    let out = update_feature(&repo, "annotated^{commit}");
    assert_eq!(
        out.status.code(),
        Some(0),
        "explicit peel: {}",
        stderr_of(&out)
    );
    assert_eq!(feature_tip(&repo), c2);
}

#[test]
fn update_ref_revision_unresolvable_is_cli003_exit128() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    let out = update_feature(&repo, "no-such-revision");
    assert_eq!(out.status.code(), Some(128), "unresolvable exits 128");
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("LBR-CLI-003"),
        "an unresolvable revision is LBR-CLI-003: {stderr}"
    );
}

#[test]
fn update_ref_revision_malformed_is_cli002_exit128() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    // `ref:` is refused at the syntax layer, before any lookup: it is
    // `symbolic-ref`'s spelling, not a revision. That keeps the usage class.
    let out = update_feature(&repo, "ref:refs/heads/main");
    assert_eq!(out.status.code(), Some(128), "malformed exits 128");
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("LBR-CLI-002"),
        "a syntax-layer refusal stays LBR-CLI-002: {stderr}"
    );
}

#[test]
fn update_ref_revision_resolver_storage_failure_keeps_repo_code() {
    let (repo, c1, _c2) = repo_with_two_commits();
    // Corrupt the commit object itself. Resolution now fails inside the
    // object store, which must NOT be reported as bad user input — the
    // command line was fine, the repository is not.
    let loose = repo
        .path()
        .join(".libra/objects")
        .join(&c1[..2])
        .join(&c1[2..]);
    assert!(loose.is_file(), "expected a loose object at {loose:?}");
    fs::write(&loose, b"not a zlib stream").unwrap();

    let out = update_feature(&repo, &c1);
    assert_ne!(out.status.code(), Some(0), "a corrupt object must not pass");
    let stderr = stderr_of(&out);
    assert!(
        !stderr.contains("LBR-CLI-002") && !stderr.contains("LBR-CLI-003"),
        "a storage failure must not be downgraded to an input error: {stderr}"
    );
    assert!(
        stderr.contains("LBR-REPO-") || stderr.contains("LBR-IO-"),
        "a storage failure keeps a repository/IO code: {stderr}"
    );
}

#[test]
fn update_ref_revision_ref_operand_still_narrow() {
    let (repo, _c1, c2) = repo_with_two_commits();
    // Widening the VALUE operand must not widen the REF operand: the
    // refs/heads/* narrowing is a registered intentional difference.
    for target in ["refs/tags/v1", "refs/remotes/origin/main", "HEAD"] {
        let out = run_libra_command(&["update-ref", target, &c2], repo.path());
        assert_eq!(
            out.status.code(),
            Some(128),
            "{target} must still be refused: {}",
            stderr_of(&out)
        );
    }
}

#[test]
fn update_ref_output_shape_human() {
    let (repo, c1, c2) = repo_with_two_commits();
    let ok = update_feature(&repo, &c1);
    assert_eq!(
        ok.status.code(),
        Some(0),
        "human success: {}",
        stderr_of(&ok)
    );
    assert_eq!(feature_tip(&repo), c1);

    // A revision operand behaves the same as a raw id on the human surface.
    let ok_rev = update_feature(&repo, "HEAD");
    assert_eq!(ok_rev.status.code(), Some(0));
    assert_eq!(feature_tip(&repo), c2);

    let err = update_feature(&repo, "no-such-revision");
    assert_eq!(err.status.code(), Some(128), "human failure exit code");
    assert!(
        String::from_utf8_lossy(&err.stdout).trim().is_empty(),
        "a failed update must not print to stdout"
    );
    assert_eq!(feature_tip(&repo), c2, "the failure left the ref alone");
}

#[test]
fn update_ref_output_shape_json() {
    let (repo, c1, _c2) = repo_with_two_commits();
    let seed = update_feature(&repo, &c1);
    assert_eq!(seed.status.code(), Some(0), "seed: {}", stderr_of(&seed));

    let ok = run_libra_command(
        &["--json", "update-ref", "refs/heads/feature", "HEAD"],
        repo.path(),
    );
    assert_eq!(
        ok.status.code(),
        Some(0),
        "json success: {}",
        stderr_of(&ok)
    );
    let json = parse_json_stdout(&ok);
    assert_eq!(json["data"]["ref"].as_str(), Some("refs/heads/feature"));
    assert_eq!(json["data"]["old"].as_str(), Some(c1.as_str()));
    assert_eq!(json["data"]["deleted"].as_bool(), Some(false));

    let err = run_libra_command(
        &["--json", "update-ref", "refs/heads/feature", "no-such-rev"],
        repo.path(),
    );
    assert_eq!(err.status.code(), Some(128), "json failure exit code");
    let stderr = stderr_of(&err);
    assert!(
        stderr.contains("\"ok\": false") && stderr.contains("LBR-CLI-003"),
        "json failure envelope: {stderr}"
    );
}

#[test]
fn update_ref_output_shape_machine() {
    let (repo, c1, _c2) = repo_with_two_commits();
    let seed = update_feature(&repo, &c1);
    assert_eq!(seed.status.code(), Some(0), "seed: {}", stderr_of(&seed));

    let ok = run_libra_command(
        &["--machine", "update-ref", "refs/heads/feature", "HEAD"],
        repo.path(),
    );
    assert_eq!(
        ok.status.code(),
        Some(0),
        "machine success: {}",
        stderr_of(&ok)
    );
    let body = String::from_utf8_lossy(&ok.stdout);
    assert!(
        body.contains("\"ref\":\"refs/heads/feature\""),
        "machine success payload: {body}"
    );

    let err = run_libra_command(
        &[
            "--machine",
            "update-ref",
            "refs/heads/feature",
            "no-such-rev",
        ],
        repo.path(),
    );
    assert_eq!(err.status.code(), Some(128), "machine failure exit code");
    assert!(
        stderr_of(&err).contains("LBR-CLI-003"),
        "machine failure carries the stable code: {}",
        stderr_of(&err)
    );
}

// ── CT1-02, edge spellings ───────────────────────────────────────────────────
// The forms below all RESOLVE fine; what makes them interesting is that what
// they resolve to is not a commit. Each must be refused naming the type, and
// must leave the ref untouched — a resolver change that started peeling would
// silently write a branch the user never named.

/// Assert `value` is refused for naming a non-commit of type `type_name`, and
/// that `refs/heads/feature` was not created.
fn assert_refused_as_non_commit(repo: &TempDir, value: &str, type_name: &str) {
    let out = update_feature(repo, value);
    assert_eq!(
        out.status.code(),
        Some(128),
        "{value} must be refused: {}",
        stderr_of(&out)
    );
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains(type_name),
        "the refusal of {value} must name the resolved type {type_name}: {stderr}"
    );
    assert!(
        stderr.contains("not a commit"),
        "the refusal of {value} must say what was expected: {stderr}"
    );
    assert!(
        stderr.contains("LBR-CLI-003"),
        "an unusable target is LBR-CLI-003: {stderr}"
    );
    let after = run_libra_command(&["rev-parse", "refs/heads/feature"], repo.path());
    assert_ne!(
        after.status.code(),
        Some(0),
        "the refused update of {value} must not have created the ref"
    );
}

#[test]
fn update_ref_revision_tree_ish_rejected() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    assert_refused_as_non_commit(&repo, "HEAD^{tree}", "tree");
}

#[test]
fn update_ref_revision_tree_path_blob_rejected() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    // `<rev>:<path>` names the blob at that path.
    assert_refused_as_non_commit(&repo, "HEAD:tracked.txt", "blob");
}

#[test]
fn update_ref_revision_tag_object_id_rejected() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    let tag = run_libra_command(&["tag", "-m", "release", "annotated"], repo.path());
    assert_eq!(tag.status.code(), Some(0), "tag: {}", stderr_of(&tag));

    // Naming the tag OBJECT by its full id is the same refusal as naming it by
    // tag name: the check is on the resolved object, not on the spelling. This
    // is also what protects against a tag whose target is itself a tag.
    let tag_oid = stdout_trimmed(&rev_parse(&repo, "annotated"));
    let cat = run_libra_command(&["cat-file", "-t", &tag_oid], repo.path());
    assert_eq!(
        stdout_trimmed(&cat),
        "tag",
        "expected rev-parse to name the tag object itself"
    );
    assert_refused_as_non_commit(&repo, &tag_oid, "tag");
}

#[test]
fn update_ref_revision_recursive_peel_accepted() {
    let (repo, _c1, c2) = repo_with_two_commits();
    let tag = run_libra_command(&["tag", "-m", "release", "annotated"], repo.path());
    assert_eq!(tag.status.code(), Some(0), "tag: {}", stderr_of(&tag));

    // `^{}` peels recursively to a non-tag, which for a tagged commit is the
    // commit. It must be accepted for the same reason `^{commit}` is.
    let out = update_feature(&repo, "annotated^{}");
    assert_eq!(
        out.status.code(),
        Some(0),
        "recursive peel: {}",
        stderr_of(&out)
    );
    assert_eq!(feature_tip(&repo), c2);
}

#[test]
fn update_ref_revision_foreign_hash_length_rejected() {
    // In a SHA-256 repository a 40-hex string is not an id of this repository.
    // Widening the operand must not make it resolve to something.
    let dir = tempfile::tempdir().unwrap();
    let init = run_libra_command(&["init", "--object-format", "sha256"], dir.path());
    assert_eq!(init.status.code(), Some(0), "init: {}", stderr_of(&init));
    for (key, value) in [("user.name", "T U Ser"), ("user.email", "t@example.com")] {
        let set = run_libra_command(&["config", "--local", key, value], dir.path());
        assert_eq!(
            set.status.code(),
            Some(0),
            "seed {key}: {}",
            stderr_of(&set)
        );
    }
    fs::write(dir.path().join("a.txt"), "a\n").unwrap();
    assert_eq!(
        run_libra_command(&["add", "a.txt"], dir.path())
            .status
            .code(),
        Some(0)
    );
    let commit = run_libra_command(&["commit", "-m", "c1", "--no-verify"], dir.path());
    assert_eq!(
        commit.status.code(),
        Some(0),
        "commit: {}",
        stderr_of(&commit)
    );

    let sha1_shaped = "a".repeat(40);
    let out = run_libra_command(
        &["update-ref", "refs/heads/feature", &sha1_shaped],
        dir.path(),
    );
    assert_eq!(
        out.status.code(),
        Some(128),
        "a foreign-length id is refused"
    );
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("LBR-CLI-002") || stderr.contains("LBR-CLI-003"),
        "a bad operand stays an input error: {stderr}"
    );
    let after = run_libra_command(&["rev-parse", "refs/heads/feature"], dir.path());
    assert_ne!(
        after.status.code(),
        Some(0),
        "the refused update must not have created the ref"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// CT1-03 (plan-20260729): `<oldvalue>` goes through the SAME revision entry
// point as `<newvalue>`, but with no commit type check — it states what the
// ref is expected to point at, so the resolved id is compared verbatim.
// ─────────────────────────────────────────────────────────────────────────────

/// Run SQL against a repository's database through python3's bundled sqlite3
/// module; `sqlite3(1)` is not installed on every dev machine.
fn repo_sqlite(repo: &TempDir, sql: &str) {
    let db = repo.path().join(".libra/libra.db");
    let out = std::process::Command::new("python3")
        .arg("-c")
        .arg(format!(
            "import sqlite3\nc = sqlite3.connect({db:?})\nc.executescript({sql:?})\nc.commit()\n"
        ))
        .output()
        .expect("run python3 sqlite3");
    assert!(
        out.status.success(),
        "sqlite {sql}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The reflog of `refs/heads/<branch>`, for asserting a refused update wrote
/// nothing.
fn reflog_of(repo: &TempDir, branch: &str) -> String {
    let out = run_libra_command(&["reflog", "show", branch], repo.path());
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn update_ref_oldvalue_cas_revision() {
    let (repo, c1, c2) = repo_with_two_commits();
    assert_eq!(update_feature(&repo, &c1).status.code(), Some(0));

    // `<oldvalue>` as a revision: HEAD~1 is c1, which is where feature points.
    let out = run_libra_command(
        &["update-ref", "refs/heads/feature", &c2, "HEAD~1"],
        repo.path(),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "a revision oldvalue that matches must swap: {}",
        stderr_of(&out)
    );
    assert_eq!(feature_tip(&repo), c2);

    // And one that does not match is an ordinary CAS failure.
    let stale = run_libra_command(
        &["update-ref", "refs/heads/feature", &c1, "HEAD~1"],
        repo.path(),
    );
    assert_eq!(stale.status.code(), Some(128), "stale oldvalue must fail");
    assert_eq!(feature_tip(&repo), c2, "the failed CAS left the ref alone");
}

#[test]
fn update_ref_oldvalue_annotated_tag_is_cas_mismatch() {
    let (repo, c1, c2) = repo_with_two_commits();
    assert_eq!(update_feature(&repo, &c2).status.code(), Some(0));
    let tag = run_libra_command(&["tag", "-m", "release", "annotated"], repo.path());
    assert_eq!(tag.status.code(), Some(0), "tag: {}", stderr_of(&tag));

    // The tag object id is not the ref's current value, so this is a CAS
    // mismatch — NOT a "not a commit" refusal. `<oldvalue>` asserts state; a
    // wrong assertion is a mismatch, exactly as Git reports it.
    let out = run_libra_command(
        &["update-ref", "refs/heads/feature", &c1, "annotated"],
        repo.path(),
    );
    assert_eq!(out.status.code(), Some(128), "mismatch exits 128");
    let stderr = stderr_of(&out);
    assert!(
        !stderr.contains("not a commit"),
        "oldvalue must not be type-checked: {stderr}"
    );
    assert!(
        !stderr.contains("LBR-CLI-003"),
        "a state mismatch is not an invalid target: {stderr}"
    );
    assert_eq!(feature_tip(&repo), c2, "the mismatch left the ref alone");
}

#[test]
fn update_ref_oldvalue_delete_with_revision_matches() {
    let (repo, _c1, c2) = repo_with_two_commits();
    assert_eq!(update_feature(&repo, &c2).status.code(), Some(0));

    let out = run_libra_command(
        &["update-ref", "-d", "refs/heads/feature", "HEAD"],
        repo.path(),
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "a matching revision oldvalue must allow the delete: {}",
        stderr_of(&out)
    );
    let after = run_libra_command(&["rev-parse", "refs/heads/feature"], repo.path());
    assert_ne!(after.status.code(), Some(0), "the ref is gone");
}

#[test]
fn update_ref_oldvalue_delete_with_revision_mismatch() {
    let (repo, c1, c2) = repo_with_two_commits();
    assert_eq!(update_feature(&repo, &c2).status.code(), Some(0));

    // feature is at c2; asserting HEAD~1 (c1) must refuse the delete.
    let out = run_libra_command(
        &["update-ref", "-d", "refs/heads/feature", "HEAD~1"],
        repo.path(),
    );
    assert_eq!(out.status.code(), Some(128), "mismatched delete must fail");
    assert_eq!(feature_tip(&repo), c2, "the ref survived");
    assert_ne!(c1, c2);
}

#[test]
fn update_ref_oldvalue_unresolvable_changes_nothing() {
    let (repo, c1, c2) = repo_with_two_commits();
    assert_eq!(update_feature(&repo, &c2).status.code(), Some(0));
    let before = ref_state(&repo, "feature");

    // A short/garbage oldvalue no longer fails a fixed-length hex check; it
    // fails to resolve. Either way the refusal must leave the ref and its
    // reflog exactly as they were.
    for bad in ["deadbeef", "no-such-revision"] {
        let out = run_libra_command(&["update-ref", "refs/heads/feature", &c1, bad], repo.path());
        assert_eq!(
            out.status.code(),
            Some(128),
            "{bad} as oldvalue must be refused: {}",
            stderr_of(&out)
        );
        assert!(
            stderr_of(&out).contains("LBR-CLI-003"),
            "an unresolvable oldvalue is LBR-CLI-003: {}",
            stderr_of(&out)
        );
        let after = ref_state(&repo, "feature");
        assert_eq!(after.0, before.0, "{bad} moved the ref");
        assert_eq!(after.1, before.1, "{bad} wrote a reflog entry");
    }
}

// ── CT1-03 policy invariants ─────────────────────────────────────────────────
// Widening the operand parsing must not open a bypass: protect/archive
// metadata is still enforced INSIDE the transaction, and a metadata read that
// fails still refuses the write (fail-closed) with a repository code.

/// The ref tip (or `None`) plus the reflog, captured so a refusal can be shown
/// to have changed neither.
fn ref_state(repo: &TempDir, branch: &str) -> (Option<String>, String) {
    let full = format!("refs/heads/{branch}");
    let tip = run_libra_command(&["rev-parse", &full], repo.path());
    let tip = tip.status.success().then(|| stdout_trimmed(&tip));
    (tip, reflog_of(repo, branch))
}

/// Assert a refused operation exited 128 with `code` and changed nothing.
fn assert_refused_and_unchanged(
    repo: &TempDir,
    branch: &str,
    before: (Option<String>, String),
    out: &Output,
    code: &str,
) {
    assert_eq!(
        out.status.code(),
        Some(128),
        "the refusal must exit 128: {}",
        stderr_of(out)
    );
    let stderr = stderr_of(out);
    assert!(
        stderr.contains(code),
        "expected {code} in the refusal: {stderr}"
    );
    let after = ref_state(repo, branch);
    assert_eq!(after.0, before.0, "the refusal moved {branch}");
    assert_eq!(
        after.1, before.1,
        "the refusal wrote a reflog entry for {branch}"
    );
}

/// Seed `refs/heads/policy` at HEAD and mark it with `key`.
fn repo_with_marked_branch(key: &str) -> (TempDir, String) {
    let (repo, _c1, c2) = repo_with_two_commits();
    assert_eq!(
        run_libra_command(&["update-ref", "refs/heads/policy", &c2], repo.path())
            .status
            .code(),
        Some(0)
    );
    let set = run_libra_command(
        &["metadata", "set", "--branch", "policy", key, "true"],
        repo.path(),
    );
    assert_eq!(set.status.code(), Some(0), "metadata: {}", stderr_of(&set));
    (repo, c2)
}

/// A branch NAME that carries policy metadata but has no ref: `metadata set`
/// requires the branch to exist, so mark it and then drop the ref row
/// directly. That is the only way to reach the "create a branch that is
/// already protected" path.
fn repo_with_marked_absent_branch(key: &str) -> (TempDir, String) {
    let (repo, _c1, c2) = repo_with_two_commits();
    assert_eq!(
        run_libra_command(&["update-ref", "refs/heads/fresh", &c2], repo.path())
            .status
            .code(),
        Some(0)
    );
    let set = run_libra_command(
        &["metadata", "set", "--branch", "fresh", key, "true"],
        repo.path(),
    );
    assert_eq!(set.status.code(), Some(0), "metadata: {}", stderr_of(&set));
    repo_sqlite(
        &repo,
        "DELETE FROM reference WHERE name = 'fresh' AND kind = 'Branch';",
    );
    let gone = run_libra_command(&["rev-parse", "refs/heads/fresh"], repo.path());
    assert!(!gone.status.success(), "the ref row should be gone");
    (repo, c2)
}

#[test]
fn update_ref_policy_update_protected() {
    let (repo, c2) = repo_with_marked_branch("protect");
    let before = ref_state(&repo, "policy");
    // A value that really would move the ref, so the refusal cannot be
    // mistaken for a no-op.
    let out = run_libra_command(&["update-ref", "refs/heads/policy", "HEAD~1"], repo.path());
    assert_refused_and_unchanged(&repo, "policy", before, &out, "LBR-POLICY-001");
    assert_eq!(
        ref_state(&repo, "policy").0,
        Some(c2),
        "the protected branch is still at its original tip"
    );
}

#[test]
fn update_ref_policy_create_protected() {
    let (repo, c2) = repo_with_marked_absent_branch("protect");
    let before = ref_state(&repo, "fresh");
    let out = run_libra_command(&["update-ref", "refs/heads/fresh", &c2], repo.path());
    assert_refused_and_unchanged(&repo, "fresh", before, &out, "LBR-POLICY-001");
    let after = run_libra_command(&["rev-parse", "refs/heads/fresh"], repo.path());
    assert!(!after.status.success(), "the ref was not created");
}

#[test]
fn update_ref_policy_delete_protected() {
    let (repo, _c2) = repo_with_marked_branch("protect");
    let before = ref_state(&repo, "policy");
    let out = run_libra_command(&["update-ref", "-d", "refs/heads/policy"], repo.path());
    assert_refused_and_unchanged(&repo, "policy", before, &out, "LBR-POLICY-001");
}

#[test]
fn update_ref_policy_update_archived() {
    let (repo, c2) = repo_with_marked_branch("archive");
    let before = ref_state(&repo, "policy");
    let out = run_libra_command(&["update-ref", "refs/heads/policy", &c2], repo.path());
    assert_refused_and_unchanged(&repo, "policy", before, &out, "LBR-POLICY-001");
}

#[test]
fn update_ref_policy_create_archived() {
    let (repo, c2) = repo_with_marked_absent_branch("archive");
    let before = ref_state(&repo, "fresh");
    let out = run_libra_command(&["update-ref", "refs/heads/fresh", &c2], repo.path());
    assert_refused_and_unchanged(&repo, "fresh", before, &out, "LBR-POLICY-001");
    let after = run_libra_command(&["rev-parse", "refs/heads/fresh"], repo.path());
    assert!(!after.status.success(), "the ref was not created");
}

#[test]
fn update_ref_policy_delete_archived() {
    let (repo, _c2) = repo_with_marked_branch("archive");
    let before = ref_state(&repo, "policy");
    let out = run_libra_command(&["update-ref", "-d", "refs/heads/policy"], repo.path());
    assert_refused_and_unchanged(&repo, "policy", before, &out, "LBR-POLICY-001");
}

/// A repository whose branch-policy metadata cannot be read at all. The write
/// must be refused (fail-closed), with a repository code — never silently
/// treated as "no policy set".
fn repo_with_unreadable_policy_metadata() -> (TempDir, String) {
    let (repo, _c1, c2) = repo_with_two_commits();
    assert_eq!(
        run_libra_command(&["update-ref", "refs/heads/policy", &c2], repo.path())
            .status
            .code(),
        Some(0)
    );
    repo_sqlite(&repo, "DROP TABLE metadata_kv;");
    (repo, c2)
}

#[test]
fn update_ref_policy_update_storage_error() {
    let (repo, c2) = repo_with_unreadable_policy_metadata();
    let before = ref_state(&repo, "policy");
    let out = run_libra_command(&["update-ref", "refs/heads/policy", &c2], repo.path());
    assert_refused_and_unchanged(&repo, "policy", before, &out, "LBR-REPO-003");
}

#[test]
fn update_ref_policy_create_storage_error() {
    let (repo, c2) = repo_with_unreadable_policy_metadata();
    let before = ref_state(&repo, "fresh");
    let out = run_libra_command(&["update-ref", "refs/heads/fresh", &c2], repo.path());
    assert_refused_and_unchanged(&repo, "fresh", before, &out, "LBR-REPO-003");
    let after = run_libra_command(&["rev-parse", "refs/heads/fresh"], repo.path());
    assert!(!after.status.success(), "the ref was not created");
}

#[test]
fn update_ref_policy_delete_storage_error() {
    let (repo, _c2) = repo_with_unreadable_policy_metadata();
    let before = ref_state(&repo, "policy");
    let out = run_libra_command(&["update-ref", "-d", "refs/heads/policy"], repo.path());
    assert_refused_and_unchanged(&repo, "policy", before, &out, "LBR-REPO-003");
    let after = run_libra_command(&["rev-parse", "refs/heads/policy"], repo.path());
    assert!(after.status.success(), "the ref survived");
}
