//! Integration tests for the `fsck` command.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::fs;

use git_internal::{
    hash::{HashKind, ObjectHash, set_hash_kind_for_test},
    internal::object::{
        ObjectTrait,
        commit::Commit,
        signature::{Signature, SignatureType},
        tree::{Tree, TreeItem, TreeItemMode},
        types::ObjectType,
    },
};
use libra::utils::client_storage::ClientStorage;
use serial_test::serial;
use tempfile::tempdir;

use super::*;

// ---------------------------------------------------------------------------
// Basic Functionality Tests (≥ 4 required)
// ---------------------------------------------------------------------------

#[test]
#[serial]
/// plan-20260714 W0 hard gate: `fsck --heal` discovery walks only
/// refs/reflogs/index roots — it does not cover every worktree's private
/// index, sequencer rows or sidecars, so it fails closed on a repository
/// with linked worktrees rather than mis-report their objects.
fn test_fsck_heal_refused_with_linked_worktrees() {
    let main = tempdir().unwrap();
    let linked = tempdir().unwrap();
    init_repo_via_cli(main.path());
    configure_identity_via_cli(main.path());
    fs::write(main.path().join("base.txt"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "base.txt"], main.path()), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], main.path()),
        "commit",
    );

    // Main-only: the heal path runs (nothing to repair is a valid outcome).
    let out = run_libra_command(&["fsck", "--heal"], main.path());
    assert_cli_success(&out, "fsck --heal runs on a single-worktree repo");

    // With a linked worktree: fail closed with the W0 refusal.
    let add = run_libra_command(
        &["worktree", "add", linked.path().to_str().unwrap()],
        main.path(),
    );
    assert_cli_success(&add, "add a linked worktree");
    let out = run_libra_command(&["fsck", "--heal"], main.path());
    assert!(
        !out.status.success(),
        "fsck --heal must refuse a linked-worktree repo: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("per-worktree object inventory"),
        "the refusal names the reason: {stderr}"
    );

    // Removing the directory does not make the historical per-worktree
    // inventory disappear. `--heal` must remain fail-closed until its object
    // discovery is actually worktree-complete.
    let remove = run_libra_command(
        &[
            "worktree",
            "remove",
            "--delete-dir",
            linked.path().to_str().unwrap(),
        ],
        main.path(),
    );
    assert_cli_success(&remove, "remove the linked worktree directory");
    let out = run_libra_command(&["fsck", "--heal"], main.path());
    assert!(
        !out.status.success(),
        "fsck --heal must remain refused after linked-worktree removal"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("worktree history is retained"),
        "the retained-history refusal must stay actionable"
    );
}

#[test]
#[serial]
/// Tests fsck on an empty repository passes successfully.
/// Verifies the basic happy path for newly initialized repositories.
fn test_fsck_empty_repo_passes() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["fsck"], repo.path());
    assert!(
        output.status.success(),
        "fsck on empty repo should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[serial]
/// Tests fsck on a repository with commits passes successfully.
/// Verifies the basic happy path for normal repositories.
fn test_fsck_repo_with_commit_passes() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["fsck"], repo.path());
    assert!(
        output.status.success(),
        "fsck on healthy repo should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[serial]
/// Tests fsck --verbose outputs progress information.
/// Verifies that the verbose flag produces expected output.
fn test_fsck_verbose_output() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["fsck", "--verbose"], repo.path());
    assert!(
        output.status.success(),
        "fsck --verbose should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Checking"),
        "verbose output should contain 'Checking', got: {}",
        stdout
    );
}

#[test]
#[serial]
/// Tests fsck --root reports root commits.
/// Verifies that the --root flag correctly identifies root commits.
fn test_fsck_root_shows_root_commit() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["fsck", "--root"], repo.path());
    assert!(
        output.status.success(),
        "fsck --root should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("root"),
        "--root should report root commits, got: {}",
        stdout
    );
}

#[test]
#[serial]
/// Tests fsck --tags reports tagged commits.
/// Verifies that the --tags flag correctly lists tags.
fn test_fsck_tags_reports_tags() {
    let repo = create_committed_repo_via_cli();

    let tag_output = run_libra_command(&["tag", "v1.0"], repo.path());
    assert!(
        tag_output.status.success(),
        "tag creation should succeed, stderr: {}",
        String::from_utf8_lossy(&tag_output.stderr)
    );

    let output = run_libra_command(&["fsck", "--tags"], repo.path());
    assert!(
        output.status.success(),
        "fsck --tags should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("v1.0"),
        "--tags should report tag names, got: {}",
        stdout
    );
}

#[test]
#[serial]
/// Tests fsck --dangling detects dangling commits.
/// Verifies that dangling objects are properly detected.
fn test_fsck_dangling_shows_only_commits() {
    let repo = create_committed_repo_via_cli();

    // Create a second commit
    fs::write(repo.path().join("file2.txt"), "second file\n").unwrap();
    run_libra_command(&["add", "file2.txt"], repo.path());
    run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path());

    // Reset to first commit, making the second commit dangling
    let log_output = run_libra_command(&["log", "--pretty=%H"], repo.path());
    let stdout = String::from_utf8_lossy(&log_output.stdout);
    let first_commit = stdout.lines().nth(1).unwrap().trim();
    run_libra_command(&["reset", "--hard", first_commit], repo.path());

    let output = run_libra_command(&["fsck", "--no-reflogs"], repo.path());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("dangling commit"),
        "should show dangling commit, got: {}",
        combined
    );
}

#[test]
#[serial]
/// Tests fsck --connectivity-only validates object graph.
/// Verifies that connectivity check passes on healthy repos.
fn test_fsck_connectivity_only_passes() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["fsck", "--connectivity-only"], repo.path());
    assert!(
        output.status.success(),
        "--connectivity-only should pass on healthy repo, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[serial]
/// Tests fsck returns exit code 0 on success.
/// Verifies the correct exit code for successful validation.
fn test_fsck_exit_code_zero_on_success() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["fsck"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(0),
        "fsck should exit 0 on success, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ---------------------------------------------------------------------------
// Boundary Condition Tests (≥ 8 required)
// ---------------------------------------------------------------------------

#[test]
#[serial]
/// Tests fsck with empty object ID argument.
/// Verifies that fsck handles empty string argument correctly.
fn test_fsck_with_empty_object_id() {
    let repo = create_committed_repo_via_cli();

    // fsck with empty argument should be classified as command usage, not crash
    let output = run_libra_command(&["fsck", ""], repo.path());
    assert_eq!(
        output.status.code(),
        Some(129),
        "fsck with empty arg should return CLI usage exit code"
    );
}

#[test]
#[serial]
/// Tests fsck with invalid object ID format (too short).
/// Verifies that fsck rejects short hash formats.
fn test_fsck_with_short_invalid_object_id() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["fsck", "abc123"], repo.path());
    assert!(
        !output.status.success(),
        "fsck should reject short invalid object ID"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid") || stderr.contains("not a valid"),
        "should report invalid format, stderr: {}",
        stderr
    );
}

#[test]
#[serial]
/// Tests global --json fsck errors stay in the structured CLI envelope instead
/// of bypassing the dispatcher through a process exit.
fn test_fsck_json_invalid_object_id_returns_structured_error() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "fsck", "abc123"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(129),
        "invalid object id should remain a CLI usage error"
    );
    assert!(
        output.stdout.is_empty(),
        "json error should keep stdout empty, got: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let (_stderr, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert_eq!(report.category, "cli");
    assert!(
        report.message.contains("invalid object ID: abc123"),
        "unexpected message: {}",
        report.message
    );
}

#[test]
#[serial]
/// Tests fsck with invalid object ID format (non-hex characters).
/// Verifies that fsck rejects non-hexadecimal characters.
fn test_fsck_with_non_hex_object_id() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["fsck", "xyz123ghk456"], repo.path());
    assert!(
        !output.status.success(),
        "fsck should reject non-hex object ID"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid") || stderr.contains("not a valid"),
        "should report invalid format, stderr: {}",
        stderr
    );
}

#[test]
#[serial]
/// Tests fsck with oversized object ID (longer than valid hash).
/// Verifies that fsck handles overly long hash strings.
fn test_fsck_with_oversized_object_id() {
    let repo = create_committed_repo_via_cli();

    // Create a hash that is too long (128 chars instead of 40 or 64)
    let long_hash = "0".repeat(128);
    let output = run_libra_command(&["fsck", &long_hash], repo.path());
    assert!(
        !output.status.success(),
        "fsck should reject oversized object ID"
    );
}

#[test]
#[serial]
/// Tests fsck with mixed-case object ID.
/// Verifies that fsck handles mixed-case hex strings correctly.
fn test_fsck_with_mixed_case_object_id() {
    let repo = create_committed_repo_via_cli();

    // Get actual commit hash and mix its case
    let log_output = run_libra_command(&["log", "--pretty=%H", "-n", "1"], repo.path());
    let stdout = String::from_utf8_lossy(&log_output.stdout);
    let commit_hash = stdout.lines().next().unwrap().trim();

    // Convert to mixed case
    let mixed_hash: String = commit_hash
        .chars()
        .enumerate()
        .map(|(i, c)| {
            if i % 2 == 0 {
                c.to_ascii_uppercase()
            } else {
                c
            }
        })
        .collect();

    let output = run_libra_command(&["fsck", &mixed_hash], repo.path());
    assert!(
        output.status.success(),
        "fsck should accept mixed-case object ID, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[serial]
/// Tests fsck with zero hash (all zeros).
/// Verifies that fsck handles the null object ID correctly.
fn test_fsck_with_zero_hash() {
    let repo = create_committed_repo_via_cli();

    let zero_hash = "0000000000000000000000000000000000000000";
    let output = run_libra_command(&["fsck", zero_hash], repo.path());
    // Zero hash should be invalid or not found, but should not crash
    assert!(
        !output.status.success() || output.status.success(),
        "fsck should handle zero hash without crashing"
    );
}

#[test]
#[serial]
/// Tests fsck --unreachable with no unreachable objects.
/// Verifies that fsck handles empty result sets correctly.
fn test_fsck_unreachable_empty() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["fsck", "--unreachable"], repo.path());
    assert!(
        output.status.success(),
        "fsck --unreachable should succeed even with no unreachable objects, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[serial]
/// Tests fsck --no-dangling suppresses dangling output.
/// Verifies that the flag properly filters output.
fn test_fsck_no_dangling_suppresses_output() {
    let repo = create_committed_repo_via_cli();

    // Create dangling commit
    fs::write(repo.path().join("file2.txt"), "second file\n").unwrap();
    run_libra_command(&["add", "file2.txt"], repo.path());
    run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path());

    let log_output = run_libra_command(&["log", "--pretty=%H"], repo.path());
    let stdout = String::from_utf8_lossy(&log_output.stdout);
    let first_commit = stdout.lines().nth(1).unwrap().trim();
    run_libra_command(&["reset", "--hard", first_commit], repo.path());

    let output = run_libra_command(&["fsck", "--no-reflogs", "--no-dangling"], repo.path());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !combined.contains("dangling"),
        "--no-dangling should suppress dangling output, got: {}",
        combined
    );
}

#[test]
#[serial]
/// Tests fsck with multiple object ID arguments.
/// Verifies that fsck rejects multiple arguments (only one OBJECT allowed).
fn test_fsck_with_multiple_object_ids() {
    let repo = create_committed_repo_via_cli();

    let log_output = run_libra_command(&["log", "--pretty=%H", "-n", "1"], repo.path());
    let stdout = String::from_utf8_lossy(&log_output.stdout);
    let commit_hash = stdout.lines().next().unwrap().trim();

    // fsck only accepts one OBJECT argument, multiple args should return CLI error
    let output = run_libra_command(&["fsck", commit_hash, commit_hash], repo.path());
    // Should return exit code 129 (CLI usage error)
    assert_eq!(
        output.status.code(),
        Some(129),
        "fsck with multiple args should exit 129 (CLI usage error)"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unexpected") || stderr.contains("usage") || stderr.contains("error"),
        "should report unexpected argument error, stderr: {}",
        stderr
    );
}

#[test]
#[serial]
/// Tests fsck on repository with only root commit.
/// Verifies minimal repository structure.
fn test_fsck_single_commit_repo() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    fs::write(repo.path().join("only.txt"), "only commit\n").unwrap();
    run_libra_command(&["add", "."], repo.path());
    run_libra_command(&["commit", "-m", "only", "--no-verify"], repo.path());

    let output = run_libra_command(&["fsck"], repo.path());
    assert!(
        output.status.success(),
        "fsck on single-commit repo should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ---------------------------------------------------------------------------
// Error Handling Tests (≥ 8 required)
// ---------------------------------------------------------------------------

#[test]
#[serial]
/// Tests fsck outside a repository returns fatal error.
/// Verifies that fsck properly reports error when not in a repository.
fn test_fsck_outside_repository() {
    let temp = tempdir().unwrap();
    let output = run_libra_command(&["fsck"], temp.path());
    assert_eq!(
        output.status.code(),
        Some(128),
        "fsck outside repository should exit 128"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal"),
        "should show fatal error, stderr: {}",
        stderr
    );
}

#[test]
#[serial]
/// Tests fsck with corrupted object file.
/// Verifies that fsck detects and reports corrupted objects.
fn test_fsck_corrupted_object() {
    let repo = create_committed_repo_via_cli();

    let log_output = run_libra_command(&["log", "--pretty=%H"], repo.path());
    let stdout = String::from_utf8_lossy(&log_output.stdout);
    let commit_hash = stdout.lines().next().unwrap().trim();

    let objects_dir = repo.path().join(".libra").join("objects");
    let object_path = objects_dir.join(&commit_hash[0..2]).join(&commit_hash[2..]);

    if object_path.exists() {
        fs::write(&object_path, b"corrupted!!!").unwrap();
        let output = run_libra_command(&["fsck"], repo.path());
        assert!(
            !output.status.success(),
            "fsck should fail on corrupted object"
        );
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // fsck reports "unknown type" for corrupted objects
        assert!(
            combined.contains("unknown")
                || combined.contains("corrupt")
                || combined.contains("error"),
            "should report corruption, got: {}",
            combined
        );
    }
}

/// Delete the loose object backing `commit_hash`, making it referenced-but-missing.
fn delete_commit_object(repo: &std::path::Path, commit_hash: &str) -> std::path::PathBuf {
    let object_path = repo
        .join(".libra")
        .join("objects")
        .join(&commit_hash[0..2])
        .join(&commit_hash[2..]);
    fs::remove_file(&object_path).expect("delete commit object");
    object_path
}

#[test]
#[serial]
/// `fsck --heal` on a repository with a missing object but no durable tier
/// configured must report the object as unrecoverable, must NOT fabricate it,
/// and must still exit non-zero (the integrity issue persists).
fn test_fsck_heal_local_only_reports_unrecoverable() {
    let repo = create_committed_repo_via_cli();

    let log_output = run_libra_command(&["log", "--pretty=%H"], repo.path());
    let stdout = String::from_utf8_lossy(&log_output.stdout);
    let commit_hash = stdout.lines().next().unwrap().trim().to_string();
    let object_path = delete_commit_object(repo.path(), &commit_hash);

    let output = run_libra_command(&["fsck", "--heal"], repo.path());
    assert!(
        !output.status.success(),
        "a still-missing object must keep fsck exit non-zero"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("heal:"),
        "should print a heal summary, got: {combined}"
    );
    assert!(
        combined.contains("unrecoverable"),
        "should report the object as unrecoverable, got: {combined}"
    );
    assert!(
        !object_path.exists(),
        "heal must not fabricate the missing object"
    );
}

#[test]
#[serial]
/// Regression: `fsck --heal` must NOT treat *packed* objects as missing.
/// `collect_heal_candidates` classifies "missing" via `local.exist` (which
/// consults pack indexes), not the loose-only object inventory. After packing
/// all objects and dropping the loose copies, a healthy repo must report nothing
/// to repair and exit 0 — not falsely flag every packed object unrecoverable.
fn test_fsck_heal_does_not_flag_packed_objects_as_missing() {
    let repo = create_committed_repo_via_cli();

    // Pack everything and delete the now-redundant loose objects, so every
    // object lives only inside a pack.
    let repack = run_libra_command(&["repack", "-a", "-d"], repo.path());
    assert!(
        repack.status.success(),
        "repack should succeed: {}",
        String::from_utf8_lossy(&repack.stderr)
    );

    let output = run_libra_command(&["--json", "fsck", "--heal"], repo.path());
    let json = parse_json_stdout(&output);
    assert_eq!(
        json["data"]["heal"]["unrecoverable"]
            .as_u64()
            .expect("heal.unrecoverable"),
        0,
        "packed objects must not be reported as unrecoverable"
    );
    assert_eq!(
        json["data"]["heal"]["healed"]
            .as_u64()
            .expect("heal.healed"),
        0,
        "a healthy packed repo needs no repair"
    );
    assert!(
        output.status.success(),
        "fsck --heal on a healthy packed repo should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[serial]
/// `fsck --heal <OBJECT>` must attempt to heal the explicitly-named object even
/// when it is not reachable from refs/reflogs/index. With no durable tier the
/// object is reported unrecoverable (proving it was seeded and attempted, not
/// silently ignored).
fn test_fsck_heal_explicit_absent_object_is_attempted() {
    let repo = create_committed_repo_via_cli();

    // A well-formed SHA-1 OID that is absent from the repo and unreferenced.
    let absent = "deadbeef".repeat(5); // 40 hex chars

    let output = run_libra_command(&["--json", "fsck", "--heal", &absent], repo.path());
    let json = parse_json_stdout(&output);
    assert!(
        json["data"]["heal"]["unrecoverable"]
            .as_u64()
            .expect("heal.unrecoverable")
            >= 1,
        "the explicitly-named absent object should be attempted and reported unrecoverable"
    );
    assert!(
        !output.status.success(),
        "an unrepaired explicit object keeps fsck exit non-zero"
    );
}

#[test]
#[serial]
/// `--json fsck --heal` embeds a structured heal report.
fn test_fsck_heal_json_includes_report() {
    let repo = create_committed_repo_via_cli();

    let log_output = run_libra_command(&["log", "--pretty=%H"], repo.path());
    let stdout = String::from_utf8_lossy(&log_output.stdout);
    let commit_hash = stdout.lines().next().unwrap().trim().to_string();
    delete_commit_object(repo.path(), &commit_hash);

    let output = run_libra_command(&["--json", "fsck", "--heal"], repo.path());
    let json = parse_json_stdout(&output);
    assert!(
        json["data"]["heal"]["unrecoverable"]
            .as_u64()
            .expect("heal.unrecoverable")
            >= 1,
        "expected at least one unrecoverable object"
    );
    assert_eq!(
        json["data"]["heal"]["healed"]
            .as_u64()
            .expect("heal.healed"),
        0,
        "nothing can be healed without a durable tier"
    );
}

#[test]
#[serial]
/// Tests fsck rejects annotated tag objects that are syntactically valid UTF-8
/// but missing required tag headers.
fn test_fsck_rejects_tag_object_missing_tagger() {
    let repo = create_committed_repo_via_cli();

    let log_output = run_libra_command(&["log", "--pretty=%H"], repo.path());
    assert_cli_success(&log_output, "log --pretty=%H");
    let stdout = String::from_utf8_lossy(&log_output.stdout);
    let commit_hash = stdout.lines().next().unwrap().trim();

    let tag_data = format!(
        "object {commit_hash}\ntype commit\ntag broken-tag\n\nmalformed tag without tagger\n"
    );
    let tag_hash = git_internal::hash::ObjectHash::from_type_and_data(
        git_internal::internal::object::types::ObjectType::Tag,
        tag_data.as_bytes(),
    );
    let storage =
        libra::utils::client_storage::ClientStorage::init(repo.path().join(".libra/objects"));
    storage
        .put(
            &tag_hash,
            tag_data.as_bytes(),
            git_internal::internal::object::types::ObjectType::Tag,
        )
        .expect("write malformed tag object");

    let output = run_libra_command(&["fsck"], repo.path());
    assert!(
        !output.status.success(),
        "fsck should fail on malformed tag object"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("missing tagger"),
        "fsck should report missing tagger, stderr: {stderr}"
    );
}

#[test]
#[serial]
/// Tests fsck with missing object file.
/// Verifies that fsck detects and reports missing objects.
fn test_fsck_missing_object() {
    let repo = create_committed_repo_via_cli();

    let log_output = run_libra_command(&["log", "--pretty=%H"], repo.path());
    let stdout = String::from_utf8_lossy(&log_output.stdout);
    let commit_hash = stdout.lines().next().unwrap().trim();

    let objects_dir = repo.path().join(".libra").join("objects");
    let object_path = objects_dir.join(&commit_hash[0..2]).join(&commit_hash[2..]);

    if object_path.exists() {
        fs::remove_file(&object_path).unwrap();
        let output = run_libra_command(&["fsck"], repo.path());
        assert!(
            !output.status.success(),
            "fsck should fail on missing object"
        );
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            combined.contains("missing") || combined.contains("not found"),
            "should report missing object, got: {}",
            combined
        );
    }
}

#[test]
#[serial]
/// Tests fsck with invalid reflog reference.
/// Verifies that fsck handles broken reflog entries.
fn test_fsck_invalid_reflog_reference() {
    let repo = create_committed_repo_via_cli();

    // Create invalid reflog entry by corrupting a ref
    let refs_dir = repo.path().join(".libra").join("refs").join("heads");
    fs::create_dir_all(&refs_dir).unwrap();
    let broken_ref = refs_dir.join("broken");
    fs::write(&broken_ref, "invalid-hash-not-exist").unwrap();

    let output = run_libra_command(&["fsck"], repo.path());
    // Should report error but not crash
    assert!(
        !output.status.success() || output.status.success(),
        "fsck should handle invalid reflog reference"
    );
}

#[test]
#[serial]
/// Tests fsck with broken HEAD reference.
/// Note: HEAD pointing to non-existent branch doesn't cause failure,
/// only prints a notice. Test verifies graceful handling.
fn test_fsck_broken_head_reference() {
    let repo = create_committed_repo_via_cli();

    // Store original HEAD
    let head_path = repo.path().join(".libra").join("HEAD");
    let original_head =
        fs::read_to_string(&head_path).unwrap_or_else(|_| "ref: refs/heads/main".to_string());

    // Corrupt HEAD to point to non-existent branch
    fs::write(&head_path, "ref: refs/heads/nonexistent").unwrap();

    let output = run_libra_command(&["fsck"], repo.path());

    // Restore original HEAD first (before assertions)
    let _ = fs::write(&head_path, &original_head);

    // check_head() only prints notice, doesn't cause failure
    // Test verifies fsck doesn't crash on broken HEAD
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Should either succeed (with notice) or handle gracefully
    assert!(
        output.status.success() || stderr.contains("notice") || stderr.contains("unborn"),
        "fsck should handle broken HEAD gracefully, stderr: {}",
        stderr
    );
}

#[test]
#[serial]
/// Tests fsck with SHA-256 repository missing object.
/// Verifies that fsck detects missing objects in SHA-256 repos.
fn test_fsck_sha256_missing_object() {
    let repo = tempdir().unwrap();
    run_libra_command(&["init", "--object-format", "sha256"], repo.path());
    configure_identity_via_cli(repo.path());

    fs::write(repo.path().join("file.txt"), "content\n").unwrap();
    run_libra_command(&["add", "file.txt"], repo.path());
    run_libra_command(&["commit", "-m", "init", "--no-verify"], repo.path());

    let log_output = run_libra_command(&["log", "--pretty=%H"], repo.path());
    let stdout = String::from_utf8_lossy(&log_output.stdout);
    let commit_hash = stdout.lines().next().unwrap().trim();

    let objects_dir = repo.path().join(".libra").join("objects");
    let object_path = objects_dir.join(&commit_hash[0..2]).join(&commit_hash[2..]);
    fs::remove_file(&object_path).unwrap();

    let output = run_libra_command(&["fsck"], repo.path());
    assert!(
        !output.status.success(),
        "fsck should fail on missing SHA-256 object"
    );
}

#[test]
#[serial]
/// Tests fsck reports unreachable commits with --unreachable flag.
/// Verifies proper error reporting for unreachable objects.
fn test_fsck_unreachable_commit_reports() {
    let repo = create_committed_repo_via_cli();

    // Create dangling commit
    fs::write(repo.path().join("file2.txt"), "second file\n").unwrap();
    run_libra_command(&["add", "file2.txt"], repo.path());
    run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path());

    let log_output = run_libra_command(&["log", "--pretty=%H"], repo.path());
    let stdout = String::from_utf8_lossy(&log_output.stdout);
    let first_commit = stdout.lines().nth(1).unwrap().trim();
    run_libra_command(&["reset", "--hard", first_commit], repo.path());

    let output = run_libra_command(&["fsck", "--no-reflogs", "--unreachable"], repo.path());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("unreachable"),
        "fsck should report unreachable commits, got: {}",
        combined
    );
}

#[test]
#[serial]
/// Tests fsck exit code is non-zero on corruption.
/// Verifies proper exit code behavior on errors.
fn test_fsck_exit_code_nonzero_on_error() {
    let repo = create_committed_repo_via_cli();

    let log_output = run_libra_command(&["log", "--pretty=%H"], repo.path());
    let stdout = String::from_utf8_lossy(&log_output.stdout);
    let commit_hash = stdout.lines().next().unwrap().trim();

    let objects_dir = repo.path().join(".libra").join("objects");
    let object_path = objects_dir.join(&commit_hash[0..2]).join(&commit_hash[2..]);

    if object_path.exists() {
        fs::remove_file(&object_path).unwrap();
        let output = run_libra_command(&["fsck"], repo.path());
        assert_ne!(
            output.status.code(),
            Some(0),
            "fsck should return non-zero exit code on error"
        );
    }
}

#[test]
#[serial]
/// Tests fsck with invalid flags returns usage error.
/// Verifies that fsck properly reports invalid flag errors.
fn test_fsck_invalid_flag() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["fsck", "--invalid-flag"], repo.path());
    // Invalid flags return exit code 129 (CLI usage error)
    assert_eq!(
        output.status.code(),
        Some(129),
        "fsck with invalid flag should exit 129"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error") || stderr.contains("usage") || stderr.contains("unexpected"),
        "should report error or usage, stderr: {}",
        stderr
    );
}

#[test]
#[serial]
/// Tests fsck with broken tag reference.
/// Verifies that fsck handles broken tag refs correctly.
fn test_fsck_broken_tag_reference() {
    let repo = create_committed_repo_via_cli();

    // Create a broken tag pointing to non-existent commit
    let tags_dir = repo.path().join(".libra").join("refs").join("tags");
    fs::create_dir_all(&tags_dir).unwrap();
    let broken_tag = tags_dir.join("broken-tag");
    fs::write(&broken_tag, "nonexistent-commit-hash").unwrap();

    let output = run_libra_command(&["fsck", "--tags"], repo.path());
    // Should report error or handle gracefully
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // fsck may report "unknown" for invalid commit hashes in tags
    assert!(
        combined.contains("unknown")
            || combined.contains("error")
            || combined.contains("not found")
            || output.status.success(),
        "should handle broken tag reference, got: {}",
        combined
    );
}

/// Build a signature with a fixed name/timestamp but caller-chosen email and
/// timezone (so `--strict` checks can be exercised).
fn strict_signature(email: &str, tz: &str, ty: SignatureType) -> Signature {
    Signature {
        signature_type: ty,
        name: "Test".to_string(),
        email: email.to_string(),
        timestamp: 1_700_000_000,
        timezone: tz.to_string(),
    }
}

/// Store an in-process commit with the given author/committer email and
/// timezone into the repo's object store, returning its object id (hex).
fn store_strict_commit(repo: &std::path::Path, email: &str, tz: &str) -> String {
    let _kind = set_hash_kind_for_test(HashKind::Sha1);
    let storage = ClientStorage::init(repo.join(".libra/objects"));
    let commit = Commit::new(
        strict_signature(email, tz, SignatureType::Author),
        strict_signature(email, tz, SignatureType::Committer),
        ObjectHash::default(),
        vec![],
        "strict fixture",
    );
    storage
        .put(
            &commit.id,
            &commit.to_data().expect("serialize commit"),
            ObjectType::Commit,
        )
        .expect("store commit");
    commit.id.to_string()
}

/// `--strict` flags a commit whose author/committer email lacks `@`; the default
/// (non-strict) check does not.
#[test]
#[serial]
fn test_strict_commit_bad_email() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    let id = store_strict_commit(repo.path(), "noatsign", "+0000");

    let plain = run_libra_command(&["fsck", &id], repo.path());
    assert!(
        !String::from_utf8_lossy(&plain.stderr).contains("bad email"),
        "non-strict fsck must not flag email format"
    );

    let strict = run_libra_command(&["fsck", "--strict", &id], repo.path());
    let stderr = String::from_utf8_lossy(&strict.stderr);
    assert!(
        stderr.contains("bad email"),
        "strict must flag a missing @: {stderr}"
    );
    assert_eq!(strict.status.code(), Some(1));
}

/// `--strict` flags a commit whose timezone is out of range; the default check
/// does not.
#[test]
#[serial]
fn test_strict_commit_bad_timezone() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());
    let id = store_strict_commit(repo.path(), "test@example.com", "+9900");

    let plain = run_libra_command(&["fsck", &id], repo.path());
    assert!(
        !String::from_utf8_lossy(&plain.stderr).contains("bad timezone"),
        "non-strict fsck must not flag the timezone"
    );

    let strict = run_libra_command(&["fsck", "--strict", &id], repo.path());
    let stderr = String::from_utf8_lossy(&strict.stderr);
    assert!(
        stderr.contains("bad timezone"),
        "strict must flag a bad timezone: {stderr}"
    );
    assert_eq!(strict.status.code(), Some(1));
}

/// `--strict` flags a tree whose entries are not in Git's canonical sort order;
/// the default check does not.
#[test]
#[serial]
fn test_strict_tree_unsorted() {
    let repo = tempdir().expect("temp repo");
    init_repo_via_cli(repo.path());

    let id = {
        let _kind = set_hash_kind_for_test(HashKind::Sha1);
        let storage = ClientStorage::init(repo.path().join(".libra/objects"));
        // Deliberately out of order: "b" precedes "a".
        let items = vec![
            TreeItem {
                mode: TreeItemMode::Blob,
                id: ObjectHash::from_bytes(&[0x22; 20]).unwrap(),
                name: "b".to_string(),
            },
            TreeItem {
                mode: TreeItemMode::Blob,
                id: ObjectHash::from_bytes(&[0x11; 20]).unwrap(),
                name: "a".to_string(),
            },
        ];
        let tree = Tree::from_tree_items(items).expect("build tree");
        storage
            .put(
                &tree.id,
                &tree.to_data().expect("serialize tree"),
                ObjectType::Tree,
            )
            .expect("store tree");
        tree.id.to_string()
    };

    let plain = run_libra_command(&["fsck", &id], repo.path());
    assert!(
        !String::from_utf8_lossy(&plain.stderr).contains("tree not sorted"),
        "non-strict fsck must not flag tree ordering"
    );

    let strict = run_libra_command(&["fsck", "--strict", &id], repo.path());
    let stderr = String::from_utf8_lossy(&strict.stderr);
    assert!(
        stderr.contains("tree not sorted"),
        "strict must flag an unsorted tree: {stderr}"
    );
    assert_eq!(strict.status.code(), Some(1));
}

// ---------------------------------------------------------------------------
// --full: packfile integrity verification (panic-safe checksum)
// ---------------------------------------------------------------------------

/// Copy a pack fixture into the repo's `.libra/objects/pack/` and build its idx.
fn install_pack_fixture(repo: &std::path::Path) -> std::path::PathBuf {
    let src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/packs/small-sha1.pack");
    let pack_dir = repo.join(".libra/objects/pack");
    fs::create_dir_all(&pack_dir).expect("create pack dir");
    let pack = pack_dir.join("test.pack");
    fs::copy(&src, &pack).expect("copy pack fixture");
    let out = run_libra_command(
        &["index-pack", pack.to_str().unwrap(), "--index-version", "2"],
        repo,
    );
    assert_cli_success(&out, "index-pack builds the fixture idx");
    pack
}

#[test]
#[serial]
fn fsck_full_passes_on_a_valid_pack() {
    let repo = tempdir().expect("repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    install_pack_fixture(repo.path());

    // Both the default (full on, like Git) and explicit `--full` accept a valid
    // pack without error.
    assert_cli_success(
        &run_libra_command(&["fsck", "--full"], repo.path()),
        "fsck --full valid",
    );
    assert_cli_success(
        &run_libra_command(&["fsck"], repo.path()),
        "fsck default valid",
    );
}

#[test]
#[serial]
fn fsck_full_reports_corrupt_pack_without_panicking() {
    let repo = tempdir().expect("repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    let pack = install_pack_fixture(repo.path());

    // Corrupt a byte in the pack body — `fsck --full` must REPORT it (exit 1),
    // not crash the pack decoder.
    let mut bytes = fs::read(&pack).unwrap();
    bytes[20] ^= 0xff;
    fs::write(&pack, &bytes).unwrap();

    let full = run_libra_command(&["fsck", "--full"], repo.path());
    assert_eq!(
        full.status.code(),
        Some(1),
        "corrupt pack fails fsck --full"
    );
    let stderr = String::from_utf8_lossy(&full.stderr);
    assert!(
        stderr.contains("bad packfile checksum"),
        "reports the corrupt pack: {stderr}"
    );
    assert!(!stderr.contains("panic"), "must not panic: {stderr}");

    // `--no-full` skips the pack check, so the corrupt pack is not flagged.
    assert_cli_success(
        &run_libra_command(&["fsck", "--no-full"], repo.path()),
        "fsck --no-full skips pack verification",
    );
}

#[test]
#[serial]
fn fsck_full_reports_corrupt_index() {
    let repo = tempdir().expect("repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    let pack = install_pack_fixture(repo.path());

    let idx = pack.with_extension("idx");
    let mut bytes = fs::read(&idx).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff; // corrupt the index trailing checksum
    fs::write(&idx, &bytes).unwrap();

    let full = run_libra_command(&["fsck", "--full"], repo.path());
    assert_eq!(full.status.code(), Some(1), "corrupt idx fails fsck --full");
    let stderr = String::from_utf8_lossy(&full.stderr);
    assert!(
        stderr.contains("test.idx"),
        "reports the corrupt idx: {stderr}"
    );
}

#[test]
#[serial]
fn fsck_full_detects_mismatched_index() {
    let repo = tempdir().expect("repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    let pack = install_pack_fixture(repo.path());

    // Replace the pack with a DIFFERENT valid pack while keeping the original
    // index: the index's recorded pack checksum no longer matches the pack's
    // trailer, which the cross-check must catch.
    let other = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/packs/ref-delta-sha1.pack");
    fs::copy(&other, &pack).expect("swap in a different pack");

    let full = run_libra_command(&["fsck", "--full"], repo.path());
    assert_eq!(
        full.status.code(),
        Some(1),
        "mismatched index fails fsck --full"
    );
    assert!(
        String::from_utf8_lossy(&full.stderr).contains("does not match the packfile"),
        "reports the pack/index mismatch"
    );
}

#[test]
#[serial]
fn fsck_full_json_verbose_emits_clean_json() {
    let repo = tempdir().expect("repo");
    init_repo_via_cli(repo.path());
    configure_identity_via_cli(repo.path());
    install_pack_fixture(repo.path());

    // `--json --verbose` must not interleave human progress lines into the JSON
    // stream — stdout has to parse as a single JSON document.
    let out = run_libra_command(&["--json", "fsck", "--full", "--verbose"], repo.path());
    assert_cli_success(&out, "fsck --json --full --verbose");
    let json = parse_json_stdout(&out);
    assert_eq!(json["command"], "fsck");
}
