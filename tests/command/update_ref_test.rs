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
fn deleting_a_missing_ref_is_a_noop() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    // Git exits 0 for `update-ref -d <missing>`: nothing to delete, no CAS value.
    let del = run_libra_command(&["update-ref", "-d", "refs/heads/ghost"], repo.path());
    assert_eq!(
        del.status.code(),
        Some(0),
        "deleting an absent ref is a no-op: {}",
        String::from_utf8_lossy(&del.stderr)
    );

    // A non-zero CAS value on a missing ref is still a mismatch.
    let cas = run_libra_command(
        &[
            "update-ref",
            "-d",
            "refs/heads/ghost",
            "1111111111111111111111111111111111111111",
        ],
        repo.path(),
    );
    assert_ne!(
        cas.status.code(),
        Some(0),
        "a CAS delete of an absent ref reports a mismatch: {}",
        String::from_utf8_lossy(&cas.stderr)
    );
    assert!(
        String::from_utf8_lossy(&cas.stderr).contains("cannot lock ref"),
        "stderr: {}",
        String::from_utf8_lossy(&cas.stderr)
    );
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

// ---------------------------------------------------------------------------
// issues/479 UR-01: `--stdin [-z]` batch transactions and `--no-deref`.
// ---------------------------------------------------------------------------

fn ref_exists(repo: &TempDir, full_ref: &str) -> bool {
    rev_parse(repo, full_ref).status.success()
}

fn stdin_batch(repo: &TempDir, extra_args: &[&str], body: &str) -> Output {
    let mut args: Vec<&str> = vec!["update-ref"];
    args.extend_from_slice(extra_args);
    args.push("--stdin");
    super::run_libra_command_with_stdin(&args, repo.path(), body)
}

/// UR-01 B1: `create`/`update`/`delete`/`verify` each work, one line per ref.
#[test]
fn update_ref_stdin_basic_matrix() {
    let (repo, c1, c2) = repo_with_two_commits();
    // One command per ref per batch (git rejects multiple updates of one ref,
    // verify included — libra mirrors that, see the B3 test).
    let first = format!(
        "create refs/heads/b1 {c1}\nupdate refs/heads/b2 {c1}\nverify refs/heads/main {c2}\n"
    );
    let out = stdin_batch(&repo, &[], &first);
    assert!(
        out.status.success(),
        "stdin batch failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(ref_exists(&repo, "refs/heads/b1"), "b1 created");
    assert_eq!(
        stdout_trimmed(&rev_parse(&repo, "refs/heads/b2")),
        c1,
        "b2 created at c1"
    );

    let second = format!("update refs/heads/b2 {c2} {c1}\ndelete refs/heads/b1 {c1}\n");
    let out = stdin_batch(&repo, &[], &second);
    assert!(
        out.status.success(),
        "second stdin batch failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!ref_exists(&repo, "refs/heads/b1"), "b1 deleted");
    assert_eq!(
        stdout_trimmed(&rev_parse(&repo, "refs/heads/b2")),
        c2,
        "b2 CAS-updated to c2"
    );
}

/// UR-01 B2: an old-value mismatch fails the WHOLE batch atomically.
#[test]
fn update_ref_stdin_failure_is_atomic() {
    let (repo, c1, _c2) = repo_with_two_commits();
    // new2 does not exist, so expecting c1 is a CAS mismatch (the zero id
    // would be "must not exist" and pass).
    let body = format!("update refs/heads/new1 {c1}\nupdate refs/heads/new2 {c1} {c1}\n");
    let out = stdin_batch(&repo, &[], &body);
    assert_eq!(
        out.status.code(),
        Some(128),
        "CAS mismatch must fail the batch: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!ref_exists(&repo, "refs/heads/new1"), "new1 must not exist");
    assert!(!ref_exists(&repo, "refs/heads/new2"), "new2 must not exist");
}

/// UR-01 B3: two updates of the same ref are refused (git's message shape).
#[test]
fn update_ref_stdin_rejects_multiple_updates_of_one_ref() {
    let (repo, c1, _c2) = repo_with_two_commits();
    let body = format!("update refs/heads/x {c1}\nupdate refs/heads/x {c1}\n");
    let out = stdin_batch(&repo, &[], &body);
    assert_eq!(out.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("multiple updates for ref 'refs/heads/x'"),
        "multiple-updates message: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!ref_exists(&repo, "refs/heads/x"));
}

/// UR-01 B4: `-z` separates a command's ARGUMENTS with NUL (Git's wire
/// format: `create SP <ref> NUL <newvalue> NUL`), not the records.
#[test]
fn update_ref_stdin_z_nul_records() {
    let (repo, c1, _c2) = repo_with_two_commits();
    let body = format!("create refs/heads/z1\0{c1}\0");
    let out = stdin_batch(&repo, &["-z"], &body);
    assert!(
        out.status.success(),
        "-z batch failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(ref_exists(&repo, "refs/heads/z1"));
}

/// The pre-fix Libra dialect (`create <ref> <newvalue>` NUL) is NOT Git's
/// format: Git keeps `<newvalue>` in its own NUL-separated field and rejects a
/// ref containing a space as `invalid ref format`, so Libra must reject it too.
#[test]
fn update_ref_stdin_z_rejects_space_separated_ref_field() {
    let (repo, c1, _c2) = repo_with_two_commits();
    let body = format!("create refs/heads/z1 {c1}\0");
    let out = stdin_batch(&repo, &["-z"], &body);
    assert_eq!(
        out.status.code(),
        Some(128),
        "non-Git -z layout must be fatal: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("invalid ref format"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !ref_exists(&repo, "refs/heads/z1"),
        "nothing may be applied"
    );
}

/// `-z` is the only mode that can carry a value containing spaces (Git applies
/// no C-style quoting there). The revision syntax `:/fix bug` from RV-02 is the
/// concrete case, and `update` requires the <oldvalue> field in `-z` mode.
#[test]
fn update_ref_stdin_z_value_with_spaces() {
    let (repo, c1, _c2) = repo_with_two_commits();
    fs::write(repo.path().join("spaced.txt"), "spaced\n").unwrap();
    assert!(
        run_libra_command(&["add", "spaced.txt"], repo.path())
            .status
            .success()
    );
    let commit = run_libra_command(&["commit", "-m", "fix bug", "--no-verify"], repo.path());
    assert!(
        commit.status.success(),
        "commit with a spaced message failed: {}",
        String::from_utf8_lossy(&commit.stderr)
    );
    let out = stdin_batch(
        &repo,
        &["-z"],
        &format!("create refs/heads/sp1\0{c1}\0update refs/heads/sp2\0:/fix bug\0{ZERO_SHA1}\0"),
    );
    assert!(
        out.status.success(),
        "-z spaced-value batch failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(ref_exists(&repo, "refs/heads/sp1"));
    assert!(
        ref_exists(&repo, "refs/heads/sp2"),
        "`:/fix bug` should resolve in -z mode"
    );
    assert_eq!(
        stdout_trimmed(&rev_parse(&repo, "refs/heads/sp2")),
        stdout_trimmed(&rev_parse(&repo, "HEAD")),
        "the spaced message search must select the matching commit"
    );
}

/// Git distinguishes an ABSENT field (end of input: fatal) from an EMPTY field
/// (meaning "no value"): `update <ref> NUL <new> NUL NUL` is an unconditional
/// update, and an empty `<oldvalue>` must not swallow the next command.
#[test]
fn update_ref_stdin_z_empty_oldvalue_field() {
    let (repo, c1, c2) = repo_with_two_commits();
    // Empty trailing field -> no CAS at all. (One command per ref per batch:
    // Git rejects a second update of the same ref in one batch.)
    let created = stdin_batch(&repo, &["-z"], &format!("create refs/heads/e1\0{c1}\0"));
    assert!(
        created.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let out = stdin_batch(&repo, &["-z"], &format!("update refs/heads/e1\0{c2}\0\0"));
    assert!(
        out.status.success(),
        "empty <oldvalue> must mean \"no CAS\": {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_trimmed(&rev_parse(&repo, "refs/heads/e1")), c2);

    // An empty <oldvalue> must not consume the following command.
    let out = stdin_batch(
        &repo,
        &["-z"],
        &format!("delete refs/heads/e1\0\0create refs/heads/e2\0{c1}\0"),
    );
    assert!(
        out.status.success(),
        "empty <oldvalue> before another command: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!ref_exists(&repo, "refs/heads/e1"), "e1 must be deleted");
    assert!(ref_exists(&repo, "refs/heads/e2"), "e2 must be created");
}

/// An ABSENT field is end-of-input, which Git treats as fatal.
#[test]
fn update_ref_stdin_z_missing_field_is_fatal() {
    let (repo, c1, _c2) = repo_with_two_commits();
    let out = stdin_batch(
        &repo,
        &["-z"],
        &format!("create refs/heads/m1\0{c1}\0delete refs/heads/m1\0"),
    );
    assert_eq!(
        out.status.code(),
        Some(128),
        "absent <oldvalue> is fatal: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unexpected end of input"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !ref_exists(&repo, "refs/heads/m1"),
        "the whole batch must apply nothing"
    );

    let empty_new = stdin_batch(&repo, &["-z"], "create refs/heads/m2\0\0");
    assert_eq!(empty_new.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&empty_new.stderr).contains("missing <newvalue>"),
        "stderr: {}",
        String::from_utf8_lossy(&empty_new.stderr)
    );
    assert!(!ref_exists(&repo, "refs/heads/m2"));
}

/// Git: `verify` with NO `<oldvalue>` (LF) or an EMPTY one (`-z`) asserts the
/// ref does NOT exist — the empty value is the all-zero id, not "any value".
#[test]
fn update_ref_verify_without_value_requires_absence() {
    let (repo, c1, c2) = repo_with_two_commits();
    assert!(
        stdin_batch(&repo, &[], "verify refs/heads/main\n")
            .status
            .code()
            .is_some_and(|code| code != 0),
        "LF `verify <ref>` on an existing ref must fail"
    );
    assert!(
        stdin_batch(&repo, &[], "verify refs/heads/absent\n")
            .status
            .success(),
        "LF `verify <ref>` on an absent ref must succeed"
    );

    assert_eq!(
        stdin_batch(&repo, &["-z"], "verify refs/heads/main\0\0")
            .status
            .code(),
        Some(128),
        "-z `verify <ref> NUL NUL` on an existing ref must fail"
    );
    assert!(
        stdin_batch(&repo, &["-z"], "verify refs/heads/absent\0\0")
            .status
            .success(),
        "-z `verify <ref> NUL NUL` on an absent ref must succeed"
    );

    // A matching value still verifies the ref's current commit.
    assert!(
        stdin_batch(&repo, &["-z"], &format!("verify refs/heads/main\0{c2}\0"))
            .status
            .success(),
        "-z: an explicit matching <oldvalue> verifies"
    );
    // The stated matrix needs both modes: LF matching and mismatching values.
    assert!(
        stdin_batch(&repo, &[], &format!("verify refs/heads/main {c2}\n"))
            .status
            .success(),
        "LF: an explicit matching <oldvalue> verifies"
    );
    assert_eq!(
        stdin_batch(&repo, &[], &format!("verify refs/heads/main {c1}\n"))
            .status
            .code(),
        Some(128),
        "LF: a mismatching <oldvalue> fails"
    );
}

/// Git acknowledges commands as it reads them, so a later malformed command must
/// not swallow the earlier `<cmd>: ok` lines.
#[test]
fn update_ref_stdin_keeps_earlier_acknowledgements_on_late_error() {
    let (repo, c1, _c2) = repo_with_two_commits();
    let out = stdin_batch(
        &repo,
        &[],
        &format!("start\ncreate refs/heads/late {c1}\nbogus\n"),
    );
    assert_eq!(out.status.code(), Some(128), "malformed command is fatal");
    assert!(
        stdout_trimmed(&out).contains("start: ok"),
        "earlier acknowledgements must survive: {}",
        stdout_trimmed(&out)
    );

    let z = stdin_batch(
        &repo,
        &["-z"],
        &format!("start\0create refs/heads/late2\0{c1}\0bogus\0"),
    );
    assert_eq!(z.status.code(), Some(128));
    assert!(
        stdout_trimmed(&z).contains("start: ok"),
        "-z acknowledgements must survive: {}",
        stdout_trimmed(&z)
    );
    assert!(!ref_exists(&repo, "refs/heads/late"), "nothing is applied");
    assert!(!ref_exists(&repo, "refs/heads/late2"), "nothing is applied");

    // Both acknowledgements (`start: ok` AND `prepare: ok`) must survive a
    // malformed command that follows the prepare, in BOTH input modes.
    let lf_prepared = stdin_batch(
        &repo,
        &[],
        &format!("start\ncreate refs/heads/late3 {c1}\nprepare\nbogus\n"),
    );
    assert_eq!(lf_prepared.status.code(), Some(128));
    assert_eq!(
        stdout_trimmed(&lf_prepared),
        "start: ok\nprepare: ok",
        "LF acknowledgements: {}",
        stdout_trimmed(&lf_prepared)
    );

    let z_prepared = stdin_batch(
        &repo,
        &["-z"],
        &format!("start\0create refs/heads/late4\0{c1}\0prepare\0bogus\0"),
    );
    assert_eq!(z_prepared.status.code(), Some(128));
    assert_eq!(
        stdout_trimmed(&z_prepared),
        "start: ok\nprepare: ok",
        "-z acknowledgements: {}",
        stdout_trimmed(&z_prepared)
    );
    assert!(!ref_exists(&repo, "refs/heads/late3"), "nothing is applied");
    assert!(!ref_exists(&repo, "refs/heads/late4"), "nothing is applied");
}

/// Git validates the queued updates during `prepare` (it locks the refs there),
/// so a `create` of an existing ref must fail at `prepare` without `prepare: ok`.
#[test]
fn update_ref_stdin_prepare_validates_preconditions() {
    let (repo, c1, _c2) = repo_with_two_commits();
    let out = stdin_batch(
        &repo,
        &[],
        &format!("start\ncreate refs/heads/main {c1}\nprepare\ncommit\n"),
    );
    assert_eq!(
        out.status.code(),
        Some(128),
        "prepare must validate the create"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("prepare: cannot lock ref"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        stdout_trimmed(&out),
        "start: ok",
        "`prepare: ok` must not be printed for a failing prepare"
    );

    // A CAS mismatch is also reported at prepare.
    let cas = stdin_batch(
        &repo,
        &[],
        &format!("start\nupdate refs/heads/main {c1} {ZERO_SHA1}\nprepare\ncommit\n"),
    );
    assert_eq!(cas.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&cas.stderr).contains("prepare: cannot lock ref"),
        "stderr: {}",
        String::from_utf8_lossy(&cas.stderr)
    );
    assert!(!stdout_trimmed(&cas).contains("prepare: ok"));
}

/// Git allows a fresh `start` after a finished transaction; per-transaction
/// state must be reset. Commands after `commit`/`abort` (including
/// `option no-deref`) are refused.
#[test]
fn update_ref_stdin_reopens_transaction_and_checks_closed_phase() {
    let (repo, c1, _c2) = repo_with_two_commits();

    let reopened = stdin_batch(
        &repo,
        &[],
        &format!(
            "start\ncreate refs/heads/r1 {c1}\ncommit\nstart\ncreate refs/heads/r2 {c1}\ncommit\n"
        ),
    );
    assert!(
        reopened.status.success(),
        "two transactions in one batch: {}",
        String::from_utf8_lossy(&reopened.stderr)
    );
    assert_eq!(
        stdout_trimmed(&reopened),
        "start: ok\ncommit: ok\nstart: ok\ncommit: ok"
    );
    assert!(ref_exists(&repo, "refs/heads/r1"));
    assert!(ref_exists(&repo, "refs/heads/r2"));

    let after_abort = stdin_batch(
        &repo,
        &[],
        &format!("start\nabort\ncreate refs/heads/r3 {c1}\n"),
    );
    assert_eq!(after_abort.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&after_abort.stderr).contains("transaction is closed"),
        "stderr: {}",
        String::from_utf8_lossy(&after_abort.stderr)
    );
    assert!(!ref_exists(&repo, "refs/heads/r3"));

    let option_after_commit = stdin_batch(&repo, &[], "start\ncommit\noption no-deref\n");
    assert_eq!(option_after_commit.status.code(), Some(128));
    assert!(String::from_utf8_lossy(&option_after_commit.stderr).contains("transaction is closed"));
}

/// The protocol is interactive: `start: ok` must reach the caller while stdin is
/// still open, or a client that waits for it would deadlock.
#[test]
fn update_ref_stdin_ack_is_streamed_before_stdin_closes() {
    use std::{
        io::{BufRead, BufReader, Read as _, Write as _},
        process::Stdio,
        time::Duration,
    };

    let (repo, _c1, _c2) = repo_with_two_commits();
    let mut child = super::base_libra_command(&["update-ref", "--stdin"], repo.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn libra update-ref --stdin");

    let mut stdin = child.child.stdin.take().expect("child stdin");
    let stdout = child.child.stdout.take().expect("child stdout");
    stdin.write_all(b"start\n").expect("write start");
    stdin.flush().expect("flush start");

    // The reader keeps stdout open until EOF: closing it after the first line
    // would make the child's next acknowledgement fail with EPIPE (which the
    // command reports as a fatal output error after committing).
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader_thread = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut first = String::new();
        let _ = reader.read_line(&mut first);
        let _ = sender.send(first);
        let mut rest = String::new();
        let _ = reader.read_to_string(&mut rest);
        rest
    });
    let line = receiver
        .recv_timeout(Duration::from_secs(30))
        .expect("`start: ok` must arrive before stdin is closed");
    assert_eq!(line.trim(), "start: ok");

    stdin.write_all(b"commit\n").expect("write commit");
    drop(stdin);
    let status = child.child.wait().expect("wait for child");
    let rest = reader_thread.join().expect("reader thread");
    let output = child
        .wait_with_output()
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(
        status.success(),
        "streamed transaction exits 0 (rest of stdout: {rest:?}, wait error: {output})"
    );
    assert!(rest.contains("commit: ok"), "rest of stdout: {rest:?}");
}

/// Git lets `prepare` be closed by `abort` (UR-01 T2), and a `delete` of an
/// absent ref is a no-op even inside a prepared transaction.
#[test]
fn update_ref_stdin_abort_after_prepare_and_delete_missing() {
    let (repo, c1, _c2) = repo_with_two_commits();

    let out = stdin_batch(
        &repo,
        &[],
        &format!("start\ncreate refs/heads/pa1 {c1}\nprepare\nabort\n"),
    );
    assert!(
        out.status.success(),
        "abort must close a prepared transaction: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_trimmed(&out), "start: ok\nprepare: ok\nabort: ok");
    assert!(
        !ref_exists(&repo, "refs/heads/pa1"),
        "abort applies nothing"
    );

    let missing = stdin_batch(
        &repo,
        &[],
        "start\ndelete refs/heads/absent\nprepare\ncommit\n",
    );
    assert!(
        missing.status.success(),
        "deleting an absent ref is a no-op: {}",
        String::from_utf8_lossy(&missing.stderr)
    );
    assert_eq!(
        stdout_trimmed(&missing),
        "start: ok\nprepare: ok\ncommit: ok"
    );
}

/// `--json` accumulates every transaction committed in the batch, even when a
/// later transaction is aborted.
#[test]
fn update_ref_stdin_json_reports_all_committed_transactions() {
    let (repo, c1, _c2) = repo_with_two_commits();
    let out = stdin_batch(
        &repo,
        &["--json"],
        &format!("start\ncreate refs/heads/js1 {c1}\ncommit\nstart\nabort\n"),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Protocol acknowledgements share stdout with the JSON envelope, so match
    // the envelope fields instead of parsing the whole stream.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"committed\": true"),
        "an earlier commit must still be reported: {stdout}"
    );
    assert!(
        stdout.contains("refs/heads/js1"),
        "the committed ref must be listed: {stdout}"
    );
}

/// A closed stdout must be detected BEFORE the commit, so the refs never move.
#[test]
fn update_ref_stdin_refuses_commit_when_stdout_is_closed() {
    use std::{io::Write as _, process::Stdio};

    let (repo, c1, _c2) = repo_with_two_commits();
    let mut child = super::base_libra_command(&["update-ref", "--stdin"], repo.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn libra update-ref --stdin");

    let mut stdin = child.child.stdin.take().expect("child stdin");
    // Close the read end before the transaction commits: a plain batch prints
    // nothing before `commit: ok`, so only the pre-commit probe can catch this.
    drop(child.child.stdout.take());
    let _ = stdin.write_all(format!("create refs/heads/closed1 {c1}\ncommit\n").as_bytes());
    let _ = stdin.flush();
    drop(stdin);

    let output = child.wait_with_output().expect("wait for child");
    assert_eq!(
        output.status.code(),
        Some(128),
        "a closed stdout must fail the batch with exit code 128: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !ref_exists(&repo, "refs/heads/closed1"),
        "no ref may be written when the acknowledgement channel is gone"
    );
}

/// `-z` handles a multi-command batch and the transaction commands.
#[test]
fn update_ref_stdin_z_multi_command_batch() {
    let (repo, c1, c2) = repo_with_two_commits();
    let body = format!(
        "create refs/heads/m1\0{c1}\0create refs/heads/m2\0{c2}\0verify refs/heads/main\0{c2}\0"
    );
    let out = stdin_batch(&repo, &["-z"], &body);
    assert!(
        out.status.success(),
        "-z multi batch failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Plain (non-transaction) batches are silent, like Git.
    assert_eq!(stdout_trimmed(&out), "");
    assert!(ref_exists(&repo, "refs/heads/m1"));
    assert!(ref_exists(&repo, "refs/heads/m2"));

    let txn = format!("start\0create refs/heads/m3\0{c1}\0prepare\0commit\0");
    let out = stdin_batch(&repo, &["-z"], &txn);
    assert!(
        out.status.success(),
        "-z transaction failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_trimmed(&out), "start: ok\nprepare: ok\ncommit: ok");
    assert!(ref_exists(&repo, "refs/heads/m3"));
}

/// UR-01 T1/T2/T3: the start/prepare/commit/abort state machine prints
/// `<cmd>: ok` and commit/abort control whether anything is applied.
#[test]
fn update_ref_stdin_transaction_matrix() {
    let (repo, c1, _c2) = repo_with_two_commits();

    // Git's order: queue the commands, prepare, then commit.
    let committed = stdin_batch(
        &repo,
        &[],
        &format!("start\ncreate refs/heads/t1 {c1}\nprepare\ncommit\n"),
    );
    assert!(
        committed.status.success(),
        "commit transaction failed: {}",
        String::from_utf8_lossy(&committed.stderr)
    );
    let out = stdout_trimmed(&committed);
    assert_eq!(out, "start: ok\nprepare: ok\ncommit: ok", "ok lines: {out}");
    assert!(ref_exists(&repo, "refs/heads/t1"));

    let aborted = stdin_batch(
        &repo,
        &[],
        &format!("start\ncreate refs/heads/t2 {c1}\nabort\n"),
    );
    assert!(
        aborted.status.success(),
        "abort transaction failed: {}",
        String::from_utf8_lossy(&aborted.stderr)
    );
    assert_eq!(
        stdout_trimmed(&aborted),
        "start: ok\nabort: ok",
        "abort lines"
    );
    assert!(!ref_exists(&repo, "refs/heads/t2"), "abort applies nothing");

    // T3: an update after commit is refused ("transaction is closed").
    let after = stdin_batch(
        &repo,
        &[],
        &format!("start\ncommit\nupdate refs/heads/t3 {c1}\n"),
    );
    assert_eq!(after.status.code(), Some(128), "closed transaction");
    assert!(
        String::from_utf8_lossy(&after.stderr).contains("transaction is closed"),
        "closed message: {}",
        String::from_utf8_lossy(&after.stderr)
    );

    // A nested start is refused.
    let nested = stdin_batch(&repo, &[], "start\nstart\n");
    assert_eq!(nested.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&nested.stderr).contains("cannot restart ongoing transaction"),
        "nested start message: {}",
        String::from_utf8_lossy(&nested.stderr)
    );

    // After `prepare` only commit/abort may follow (Git: "prepared transactions
    // can only be closed") — a queued update in that window is refused and
    // nothing is applied.
    let late = stdin_batch(
        &repo,
        &[],
        &format!("start\nprepare\ncreate refs/heads/t4 {c1}\ncommit\n"),
    );
    assert_eq!(late.status.code(), Some(128), "post-prepare update");
    assert!(
        String::from_utf8_lossy(&late.stderr).contains("prepared transactions can only be closed"),
        "prepared message: {}",
        String::from_utf8_lossy(&late.stderr)
    );
    assert!(!ref_exists(&repo, "refs/heads/t4"), "nothing is applied");

    // A second `prepare` after a successful one is refused the same way.
    let double_prepare = stdin_batch(&repo, &[], "start\nprepare\nprepare\n");
    assert_eq!(double_prepare.status.code(), Some(128));

    // An explicit transaction left uncommitted at end of input applies nothing
    // but still exits 0, like Git.
    let dangling = stdin_batch(&repo, &[], &format!("start\ncreate refs/heads/t5 {c1}\n"));
    assert!(
        dangling.status.success(),
        "an uncommitted transaction exits 0: {}",
        String::from_utf8_lossy(&dangling.stderr)
    );
    assert_eq!(stdout_trimmed(&dangling), "start: ok");
    assert!(
        !ref_exists(&repo, "refs/heads/t5"),
        "an uncommitted transaction applies nothing"
    );
    let prepared_only = stdin_batch(
        &repo,
        &[],
        &format!("start\ncreate refs/heads/t6 {c1}\nprepare\n"),
    );
    assert!(prepared_only.status.success());
    assert!(!ref_exists(&repo, "refs/heads/t6"));

    // A plain batch with no `start` still commits on its `commit` line.
    let implicit = stdin_batch(&repo, &[], &format!("create refs/heads/t7 {c1}\ncommit\n"));
    assert!(implicit.status.success());
    assert_eq!(stdout_trimmed(&implicit), "commit: ok");
    assert!(ref_exists(&repo, "refs/heads/t7"));

    // `commit: ok` must not be printed when the commit itself fails.
    let failing = stdin_batch(
        &repo,
        &[],
        &format!("start\nupdate refs/heads/main {c1} {ZERO_SHA1}\ncommit\n"),
    );
    assert_eq!(failing.status.code(), Some(128), "CAS failure is fatal");
    assert!(
        !stdout_trimmed(&failing).contains("commit: ok"),
        "a failed commit must not acknowledge success: {}",
        stdout_trimmed(&failing)
    );
}

/// UR-01: out-of-scope refs and `symref-*` commands fail the whole batch with
/// the invalid-target class (129 / `LBR-CLI-003`) and apply nothing.
#[test]
fn update_ref_stdin_out_of_scope_ref_aborts_batch() {
    let (repo, c1, _c2) = repo_with_two_commits();
    let body = format!("create refs/heads/ok {c1}\ncreate refs/tags/v1 {c1}\n");
    let out = stdin_batch(&repo, &[], &body);
    assert_eq!(
        out.status.code(),
        Some(129),
        "out-of-scope ref must be 129: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Error-Code: LBR-CLI-003"),
        "stable code: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !ref_exists(&repo, "refs/heads/ok"),
        "nothing may be applied"
    );

    let symref = stdin_batch(&repo, &[], "symref-update HEAD refs/heads/main\n");
    assert_eq!(symref.status.code(), Some(129));
    assert!(
        String::from_utf8_lossy(&symref.stderr).contains("symbolic refs are not supported"),
        "symref message: {}",
        String::from_utf8_lossy(&symref.stderr)
    );
}

/// UR-01: `--no-deref` and `option no-deref` are accepted and change nothing
/// (no symbolic refs exist inside the `refs/heads/*` scope).
#[test]
fn update_ref_no_deref_accepted() {
    let (repo, c1, _c2) = repo_with_two_commits();
    let out = stdin_batch(
        &repo,
        &["--no-deref"],
        &format!("option no-deref\ncreate refs/heads/nd1 {c1}\n"),
    );
    assert!(
        out.status.success(),
        "--no-deref batch failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(ref_exists(&repo, "refs/heads/nd1"));

    // The argv form accepts the flag as a no-op too.
    let argv = run_libra_command(
        &["update-ref", "--no-deref", "refs/heads/nd2", &c1],
        repo.path(),
    );
    assert!(
        argv.status.success(),
        "--no-deref argv form failed: {}",
        String::from_utf8_lossy(&argv.stderr)
    );
    assert!(ref_exists(&repo, "refs/heads/nd2"));
}
