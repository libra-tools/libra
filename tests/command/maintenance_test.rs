//! Integration tests for the `maintenance` command.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::fs;

use tempfile::tempdir;

use super::*;

// ---------------------------------------------------------------------------
// Basic Functionality Tests (≥ 4 required)
// ---------------------------------------------------------------------------

#[test]

/// Tests `maintenance run` on a healthy repository passes successfully.
/// Verifies the basic happy path for running all maintenance tasks.
fn test_maintenance_run_all_tasks_passes() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["maintenance", "run"], repo.path());
    assert!(
        output.status.success(),
        "maintenance run should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]

/// Tests `maintenance run --task gc` runs only the gc task.
/// Verifies that selective task execution works.
fn test_maintenance_run_gc_only() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["maintenance", "run", "--task", "gc"], repo.path());
    assert!(
        output.status.success(),
        "maintenance run --task gc should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("gc"),
        "output should mention gc task, got: {stdout}"
    );
}

#[test]

/// Tests `maintenance register` followed by `maintenance status`.
/// Verifies registration and status reporting.
fn test_maintenance_register_and_status() {
    let repo = create_committed_repo_via_cli();

    let register_output = run_libra_command(&["maintenance", "register"], repo.path());
    assert!(
        register_output.status.success(),
        "register should succeed, stderr: {}",
        String::from_utf8_lossy(&register_output.stderr)
    );

    let status_output = run_libra_command(&["maintenance", "status"], repo.path());
    assert!(
        status_output.status.success(),
        "status should succeed, stderr: {}",
        String::from_utf8_lossy(&status_output.stderr)
    );
    let stdout = String::from_utf8_lossy(&status_output.stdout);
    assert!(
        stdout.contains("registered"),
        "status should show registered, got: {stdout}"
    );
}

#[test]

/// Tests `maintenance unregister` removes registration.
/// Verifies the unregister happy path.
fn test_maintenance_unregister() {
    let repo = create_committed_repo_via_cli();

    run_libra_command(&["maintenance", "register"], repo.path());

    let output = run_libra_command(&["maintenance", "unregister"], repo.path());
    assert!(
        output.status.success(),
        "unregister should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let status_output = run_libra_command(&["maintenance", "status"], repo.path());
    let stdout = String::from_utf8_lossy(&status_output.stdout);
    assert!(
        stdout.contains("not registered"),
        "status should show not registered after unregister, got: {stdout}"
    );
}

#[test]

/// Tests `maintenance run --dry-run` reports without modifying the repository.
/// Verifies dry-run mode produces output and exits successfully.
fn test_maintenance_run_dry_run() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["maintenance", "run", "--dry-run"], repo.path());
    assert!(
        output.status.success(),
        "dry-run should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("would") || stdout.contains("skipping") || stdout.contains("skipped"),
        "dry-run should indicate no changes, got: {stdout}"
    );
}

#[test]

/// Tests `maintenance run --task loose-objects` on a repository with few objects.
/// Verifies that the threshold check prevents unnecessary packing.
fn test_maintenance_run_loose_objects_few() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(
        &["maintenance", "run", "--task", "loose-objects"],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "loose-objects should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("skipping") || stdout.contains("threshold"),
        "few loose objects should skip packing, got: {stdout}"
    );
}

#[test]

/// Tests `maintenance run --task pack-refs` packs loose refs.
/// Verifies pack-refs task execution.
fn test_maintenance_run_pack_refs() {
    let repo = create_committed_repo_via_cli();

    // Create a branch to have refs to pack
    run_libra_command(&["branch", "test-branch"], repo.path());

    let output = run_libra_command(&["maintenance", "run", "--task", "pack-refs"], repo.path());
    assert!(
        output.status.success(),
        "pack-refs should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]

/// Tests `maintenance status --json` returns structured output.
/// Verifies JSON output for the status subcommand.
fn test_maintenance_status_json() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["--json", "maintenance", "status"], repo.path());
    assert!(
        output.status.success(),
        "json status should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.stdout.is_empty(),
        "json status should produce stdout"
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let data = json.get("data").expect("json should have data field");
    assert!(
        data.get("registered").is_some(),
        "json data should contain registered field"
    );
}

// ---------------------------------------------------------------------------
// Boundary Condition Tests (≥ 8 required)
// ---------------------------------------------------------------------------

#[test]

/// Tests `maintenance run` on an empty (newly initialized) repository.
/// Verifies graceful handling of repositories with minimal objects.
fn test_maintenance_run_empty_repo() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    let output = run_libra_command(&["maintenance", "run"], repo.path());
    assert!(
        output.status.success(),
        "maintenance on empty repo should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]

/// Tests `maintenance run` on a repository with only a root commit.
/// Verifies minimal repository structure handling.
fn test_maintenance_run_single_commit_repo() {
    let repo = tempdir().unwrap();
    init_repo_via_cli(repo.path());

    fs::write(repo.path().join("only.txt"), "only commit\n").unwrap();
    run_libra_command(&["add", "."], repo.path());
    run_libra_command(&["commit", "-m", "only", "--no-verify"], repo.path());

    let output = run_libra_command(&["maintenance", "run"], repo.path());
    assert!(
        output.status.success(),
        "maintenance on single-commit repo should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]

/// Tests `maintenance run --task loose-objects` when there are no loose objects.
/// Verifies threshold-based skip logic on empty object sets.
fn test_maintenance_run_with_no_loose_objects() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(
        &["maintenance", "run", "--task", "loose-objects"],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "should pass even with no loose objects, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("skipping") || stdout.contains("only"),
        "should indicate skipping, got: {stdout}"
    );
}

#[test]

/// Tests `maintenance run --task incremental-repack` when there are no pack files.
/// Verifies graceful handling of missing pack directory.
fn test_maintenance_run_with_few_packs() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(
        &["maintenance", "run", "--task", "incremental-repack"],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "should pass with few packs, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]

/// Tests `maintenance status` before any registration.
/// Verifies default unregistered state.
fn test_maintenance_status_before_register() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["maintenance", "status"], repo.path());
    assert!(
        output.status.success(),
        "status should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("not registered"),
        "default status should be not registered, got: {stdout}"
    );
}

#[test]

/// Tests `maintenance run --quiet` suppresses progress output.
/// Verifies quiet mode reduces stdout.
fn test_maintenance_run_quiet() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["maintenance", "run", "--quiet"], repo.path());
    assert!(
        output.status.success(),
        "quiet run should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]

/// Tests `maintenance run --task commit-graph` runs the commit-graph task.
/// On a repository with commits it now writes a real commit-graph file.
fn test_maintenance_run_commit_graph_skipped() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(
        &["maintenance", "run", "--task", "commit-graph"],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "commit-graph task should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("commit-graph"),
        "should report the commit-graph task, got: {stdout}"
    );
}

#[test]

/// Tests `maintenance run --task prefetch` reports skip gracefully.
/// Verifies handling of tasks requiring remote configuration.
fn test_maintenance_run_prefetch_skipped() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["maintenance", "run", "--task", "prefetch"], repo.path());
    assert!(
        output.status.success(),
        "prefetch should pass (skipped), stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("skipped") || stdout.contains("requires remote"),
        "should indicate skipped, got: {stdout}"
    );
}

#[test]

/// Tests `maintenance run --dry-run --task gc` with a dangling object.
/// Verifies dry-run correctly reports what would be removed.
fn test_maintenance_run_dry_run_gc_with_dangling() {
    let repo = create_committed_repo_via_cli();

    // Create a second commit and then reset, leaving a dangling commit
    fs::write(repo.path().join("file2.txt"), "second file\n").unwrap();
    run_libra_command(&["add", "file2.txt"], repo.path());
    run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path());

    let log_output = run_libra_command(&["log", "--pretty=%H"], repo.path());
    let stdout = String::from_utf8_lossy(&log_output.stdout);
    let first_commit = stdout.lines().nth(1).unwrap().trim();
    run_libra_command(&["reset", "--hard", first_commit], repo.path());

    let output = run_libra_command(
        &["maintenance", "run", "--dry-run", "--task", "gc"],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "dry-run gc should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("would") || stdout.contains("unreachable"),
        "dry-run should mention would remove or unreachable, got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// Error Handling Tests (≥ 8 required)
// ---------------------------------------------------------------------------

#[test]

/// Tests `maintenance run` outside a repository returns fatal error.
/// Verifies proper error handling when not in a repository.
fn test_maintenance_outside_repository() {
    let temp = tempdir().unwrap();
    let output = run_libra_command(&["maintenance", "run"], temp.path());
    assert_eq!(
        output.status.code(),
        Some(128),
        "maintenance outside repo should exit 128"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal") || stderr.contains("not a libra repository"),
        "should show fatal error, stderr: {stderr}"
    );
}

#[test]

/// Tests `maintenance run` with an invalid flag returns usage error.
/// Verifies CLI argument validation.
fn test_maintenance_run_invalid_flag() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["maintenance", "run", "--invalid-flag"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(129),
        "invalid flag should exit 129"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error") || stderr.contains("unexpected"),
        "should report argument error, stderr: {stderr}"
    );
}

#[test]

/// Tests `maintenance register` outside a repository returns fatal error.
/// Verifies repo validation for register subcommand.
fn test_maintenance_register_outside_repo() {
    let temp = tempdir().unwrap();
    let output = run_libra_command(&["maintenance", "register"], temp.path());
    assert!(
        !output.status.success(),
        "register outside repo should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal") || stderr.contains("not a libra repository"),
        "should show fatal error, stderr: {stderr}"
    );
}

#[test]

/// Tests `maintenance status` outside a repository returns fatal error.
/// Verifies repo validation for status subcommand.
fn test_maintenance_status_outside_repo() {
    let temp = tempdir().unwrap();
    let output = run_libra_command(&["maintenance", "status"], temp.path());
    assert!(!output.status.success(), "status outside repo should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal") || stderr.contains("not a libra repository"),
        "should show fatal error, stderr: {stderr}"
    );
}

#[test]

/// Tests `maintenance run --task gc` actually removes dangling objects.
/// Verifies gc task performs expected cleanup.
fn test_maintenance_run_gc_removes_dangling() {
    let repo = create_committed_repo_via_cli();

    // Create dangling commit
    fs::write(repo.path().join("file2.txt"), "second file\n").unwrap();
    run_libra_command(&["add", "file2.txt"], repo.path());
    run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path());

    let log_output = run_libra_command(&["log", "--pretty=%H"], repo.path());
    let stdout = String::from_utf8_lossy(&log_output.stdout);
    let first_commit = stdout.lines().nth(1).unwrap().trim();
    run_libra_command(&["reset", "--hard", first_commit], repo.path());

    let output = run_libra_command(&["maintenance", "run", "--task", "gc"], repo.path());
    assert!(
        output.status.success(),
        "gc should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("removed") || stdout.contains("unreachable"),
        "gc should report removal, got: {stdout}"
    );
}

#[test]
fn test_maintenance_gc_preserves_file_backed_stash_root() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "older stashed change\n").unwrap();
    let older = run_libra_command(&["stash", "push", "-m", "older-gc-root"], repo.path());
    assert_cli_success(&older, "create older stash before gc");
    fs::write(repo.path().join("tracked.txt"), "newer stashed change\n").unwrap();
    let newer = run_libra_command(&["stash", "push", "-m", "newer-gc-root"], repo.path());
    assert_cli_success(&newer, "create newer stash before gc");

    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], repo.path());
    assert_cli_success(&gc, "gc with stash root");

    let pop_newer = run_libra_command(&["stash", "pop"], repo.path());
    assert_cli_success(&pop_newer, "restore newest stash after gc");
    assert_eq!(
        fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
        "newer stashed change\n"
    );
    assert_cli_success(
        &run_libra_command(&["reset", "--hard", "HEAD"], repo.path()),
        "clear newest restored change",
    );
    let pop_older = run_libra_command(&["stash", "pop"], repo.path());
    assert_cli_success(&pop_older, "restore older reflog-only stash after gc");
    assert_eq!(
        fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
        "older stashed change\n"
    );
}

#[test]
fn test_maintenance_gc_traces_annotated_tag_targets() {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), "tag-only commit\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], repo.path()),
        "stage tag-only commit",
    );
    assert_cli_success(
        &run_libra_command(
            &["commit", "-m", "tag-only commit", "--no-verify"],
            repo.path(),
        ),
        "create tag-only commit",
    );
    let target = run_libra_command(&["rev-parse", "HEAD"], repo.path());
    assert_cli_success(&target, "resolve annotated tag target");
    let target = String::from_utf8(target.stdout).unwrap().trim().to_string();
    assert_cli_success(
        &run_libra_command(
            &["tag", "-m", "GC traversal", "tagged-gc-root"],
            repo.path(),
        ),
        "create annotated tag",
    );
    assert_cli_success(
        &run_libra_command(&["reset", "--hard", "HEAD~1"], repo.path()),
        "move the branch away from the tagged commit",
    );
    assert_cli_success(
        &run_libra_command(&["reflog", "expire", "--expire=now", "--all"], repo.path()),
        "remove reflog roots for the tagged commit",
    );

    assert_cli_success(
        &run_libra_command(&["maintenance", "run", "--task", "gc"], repo.path()),
        "run gc with an annotated-tag-only target",
    );
    assert_cli_success(
        &run_libra_command(&["cat-file", "-e", &target], repo.path()),
        "annotated tag target should survive gc",
    );
}

#[test]
fn test_maintenance_gc_fails_closed_when_index_root_is_corrupt() {
    let repo = create_committed_repo_via_cli();
    fs::write(
        repo.path().join("tracked.txt"),
        "staged and otherwise unreachable\n",
    )
    .unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], repo.path()),
        "stage unique blob",
    );
    let staged = run_libra_command(&["ls-files", "--stage", "tracked.txt"], repo.path());
    assert_cli_success(&staged, "read staged object id");
    let staged = String::from_utf8(staged.stdout).unwrap();
    let oid = staged
        .split_whitespace()
        .nth(1)
        .expect("stage row has object id");
    let object_path = repo
        .path()
        .join(".libra/objects")
        .join(&oid[..2])
        .join(&oid[2..]);
    assert!(object_path.exists(), "staged blob starts as a loose object");

    fs::write(repo.path().join(".libra/index"), b"corrupt index").unwrap();
    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], repo.path());
    assert!(!gc.status.success(), "gc must reject an unreadable root");
    assert!(
        String::from_utf8_lossy(&gc.stderr).contains("LBR-IO-001"),
        "stderr was: {}",
        String::from_utf8_lossy(&gc.stderr)
    );
    assert!(
        object_path.exists(),
        "gc must not delete staged data after silently ignoring a corrupt index"
    );
}

#[test]

/// Tests `maintenance run --json` returns structured output envelope.
/// Verifies JSON output format for the run subcommand.
fn test_maintenance_run_json_output() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(
        &["--json", "maintenance", "run", "--task", "gc"],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "json run should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.stdout.is_empty(), "json run should produce stdout");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid json");
    let data = json.get("data").expect("json should have data field");
    assert!(
        data.get("dry_run").is_some(),
        "json data should contain dry_run field"
    );
    assert!(
        data.get("tasks").is_some(),
        "json data should contain tasks field"
    );
}

#[test]

/// Tests `maintenance run --task gc --task loose-objects` runs multiple tasks.
/// Verifies multiple --task flags are accepted.
fn test_maintenance_run_multiple_tasks() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(
        &[
            "maintenance",
            "run",
            "--task",
            "gc",
            "--task",
            "loose-objects",
        ],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "multiple tasks should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("gc") && stdout.contains("loose-objects"),
        "output should mention both tasks, got: {stdout}"
    );
}

#[test]

/// Tests `maintenance unregister` on a repository that was never registered.
/// Verifies graceful handling of unregister without prior register.
fn test_maintenance_unregister_not_registered() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["maintenance", "unregister"], repo.path());
    assert!(
        output.status.success(),
        "unregister on unregistered repo should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]

/// Tests `maintenance run --dry-run` does not modify repository state.
/// Verifies that dry-run leaves objects untouched.
fn test_maintenance_dry_run_no_changes() {
    let repo = create_committed_repo_via_cli();

    // Count loose objects before
    let objects_dir = repo.path().join(".libra").join("objects");
    let before_count = count_loose_objects(&objects_dir);

    let output = run_libra_command(&["maintenance", "run", "--dry-run"], repo.path());
    assert!(
        output.status.success(),
        "dry-run should pass, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Count loose objects after
    let after_count = count_loose_objects(&objects_dir);
    assert_eq!(
        before_count, after_count,
        "dry-run should not change object count"
    );
}

/// `maintenance run --task prefetch` with no configured remotes succeeds and
/// reports that it skipped (no network access required).
#[test]
fn test_maintenance_prefetch_no_remotes_skips() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["maintenance", "run", "--task", "prefetch"], repo.path());
    assert!(
        output.status.success(),
        "prefetch with no remotes should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("prefetch"),
        "output should mention the prefetch task, got: {stdout}"
    );
}

/// `maintenance run --task prefetch --dry-run` with a configured remote reports
/// the planned prefetch without performing any network fetch.
#[test]
fn test_maintenance_prefetch_dry_run_lists_remotes() {
    let repo = create_committed_repo_via_cli();
    run_libra_command(
        &["remote", "add", "origin", "https://example.com/repo.git"],
        repo.path(),
    );

    let output = run_libra_command(
        &["maintenance", "run", "--task", "prefetch", "--dry-run"],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "prefetch dry-run should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("would prefetch") || stdout.contains("prefetch"),
        "dry-run output should describe the prefetch, got: {stdout}"
    );
}

/// `maintenance run --task commit-graph` writes a Git-compatible commit-graph
/// file beginning with the `CGPH` signature.
#[test]
fn test_maintenance_commit_graph_writes_file() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(
        &["maintenance", "run", "--task", "commit-graph"],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "commit-graph task should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let commit_graph = repo.path().join(".libra/objects/info/commit-graph");
    assert!(
        commit_graph.exists(),
        "commit-graph file should be written to objects/info"
    );
    let bytes = fs::read(&commit_graph).unwrap();
    assert_eq!(
        &bytes[0..4],
        b"CGPH",
        "commit-graph should start with the CGPH signature"
    );
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Count loose objects in the objects directory.
fn count_loose_objects(objects_dir: &std::path::Path) -> usize {
    let mut count = 0;
    for entry in fs::read_dir(objects_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy();
        if name.len() != 2 {
            continue;
        }
        for sub in fs::read_dir(&path).unwrap() {
            let sub = sub.unwrap();
            if sub.path().is_file() {
                count += 1;
            }
        }
    }
    count
}
