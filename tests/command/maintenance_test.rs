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

/// W2 §C.4.3: with the typed `GcObjectSource` inventory complete, gc prune
/// RUNS in a multi-worktree repository (the W0 skip is lifted) and keeps
/// every root class alive: a blob staged ONLY in a linked worktree's
/// private index, and a note blob anchored ONLY by the `notes` table.
#[test]
fn gc_runs_multi_worktree_and_keeps_private_and_registered_roots() {
    let repo = super::create_committed_repo_via_cli();
    let main = repo.path();
    let wt_root = tempfile::tempdir().expect("wt root");
    let wt = wt_root.path().join("gc-wt");
    super::assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // A blob reachable ONLY from the linked worktree's private index.
    std::fs::write(wt.join("staged-only.txt"), "linked staged blob\n").unwrap();
    super::assert_cli_success(
        &run_libra_command(&["add", "staged-only.txt"], &wt),
        "wt add",
    );
    // A note blob anchored ONLY by the notes table.
    super::assert_cli_success(
        &run_libra_command(&["notes", "add", "-m", "keep-this-note", "HEAD"], main),
        "notes add",
    );

    // Age loose objects past the prune grace window so survival proves the
    // ROOTS below, not the freshness belt.
    super::worktree_isolation_test::backdate_loose_objects(main);
    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], main);
    super::assert_cli_success(&gc, "gc in a multi-worktree repository");
    let stdout = String::from_utf8_lossy(&gc.stdout);
    assert!(
        !stdout.contains("skipped loose-object prune"),
        "the W0 multi-worktree skip is lifted: {stdout}"
    );

    // Both roots survived: the note still renders, and the staged-only blob
    // still commits cleanly from the linked worktree.
    let note = run_libra_command(&["notes", "show", "HEAD"], main);
    super::assert_cli_success(&note, "notes show after gc");
    assert!(
        String::from_utf8_lossy(&note.stdout).contains("keep-this-note"),
        "the notes-table blob survived the prune"
    );
    // `diff --cached` must read the staged blob's CONTENT back from the
    // object store — it errors (or shows nothing) if the prune dropped it.
    let cached = run_libra_command(&["diff", "--cached"], &wt);
    super::assert_cli_success(&cached, "diff --cached after gc");
    assert!(
        String::from_utf8_lossy(&cached.stdout).contains("linked staged blob"),
        "the linked worktree's staged-only blob content survived the prune: {}",
        String::from_utf8_lossy(&cached.stdout)
    );
}

// ── PD-04: repo-level findings-blob reachability GC ─────────────────────────

/// Resolve the repo id exactly like the object_index writer/delete predicate.
async fn object_index_repo_id(conn: &sea_orm::DatabaseConnection) -> String {
    use sea_orm::{ConnectionTrait, Statement};
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT value FROM config_kv WHERE key = 'libra.repoid' ORDER BY id DESC LIMIT 1"
                .to_string(),
        ))
        .await
        .expect("query libra.repoid");
    match row {
        Some(row) => {
            let value: String = row.try_get_by("value").expect("decode repoid");
            if value.trim().is_empty() {
                "unknown-repo".to_string()
            } else {
                value
            }
        }
        None => "unknown-repo".to_string(),
    }
}

async fn insert_object_index_row(conn: &sea_orm::DatabaseConnection, repo_id: &str, oid: &str) {
    use sea_orm::{ConnectionTrait, Statement};
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT OR IGNORE INTO object_index (o_id, o_type, o_size, repo_id, created_at, is_synced) \
         VALUES (?, 'agent_findings', 1, ?, 0, 0)",
        [oid.into(), repo_id.into()],
    ))
    .await
    .expect("insert object_index row");
}

async fn object_index_row_count(
    conn: &sea_orm::DatabaseConnection,
    repo_id: &str,
    oid: &str,
) -> i64 {
    use sea_orm::{ConnectionTrait, Statement};
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS n FROM object_index WHERE repo_id = ? AND o_id = ?",
            [repo_id.into(), oid.into()],
        ))
        .await
        .expect("count object_index rows")
        .expect("count row present");
    row.try_get_by("n").expect("decode count")
}

fn loose_object_file(repo: &std::path::Path, oid: &str) -> std::path::PathBuf {
    repo.join(".libra")
        .join("objects")
        .join(&oid[..2])
        .join(&oid[2..])
}

/// PD-04 designated test: repo-level reachability GC reclaims an orphaned
/// findings blob together with its `object_index` row, while a byte-shared
/// blob reachable from a commit keeps both its object and its row;
/// `--dry-run` only counts, and a second run is a no-op.
#[tokio::test]
#[serial]
async fn agent_object_gc_findings_reachability() {
    let repo = create_committed_repo_via_cli();

    // Orphan findings blob: written loose, referenced by nothing.
    fs::write(repo.path().join("orphan-findings.md"), "orphan findings\n").unwrap();
    let hashed = run_libra_command(&["hash-object", "-w", "orphan-findings.md"], repo.path());
    assert_cli_success(&hashed, "hash-object -w orphan findings");
    let orphan_oid = String::from_utf8_lossy(&hashed.stdout).trim().to_string();
    fs::remove_file(repo.path().join("orphan-findings.md")).unwrap();

    // Shared blob: the committed file's content — reachable from HEAD.
    let shared = run_libra_command(&["rev-parse", "HEAD:tracked.txt"], repo.path());
    let shared_oid = if shared.status.success() {
        String::from_utf8_lossy(&shared.stdout).trim().to_string()
    } else {
        // Fixture file name differs across helpers; fall back to ls-files.
        let ls = run_libra_command(&["ls-files", "--stage"], repo.path());
        assert_cli_success(&ls, "ls-files --stage");
        String::from_utf8_lossy(&ls.stdout)
            .split_whitespace()
            .nth(1)
            .expect("staged blob oid")
            .to_string()
    };
    assert_ne!(orphan_oid, shared_oid);

    // Register BOTH blobs in object_index as agent findings.
    let db_url = format!(
        "sqlite://{}",
        repo.path().join(".libra").join("libra.db").display()
    );
    let conn = sea_orm::Database::connect(&db_url).await.expect("open db");
    let repo_id = object_index_repo_id(&conn).await;
    insert_object_index_row(&conn, &repo_id, &orphan_oid).await;
    insert_object_index_row(&conn, &repo_id, &shared_oid).await;

    // Age the orphan past the prune grace window (backdate its mtime).
    let orphan_file = loose_object_file(repo.path(), &orphan_oid);
    assert!(orphan_file.exists(), "orphan loose object on disk");
    let touch = std::process::Command::new("touch")
        .args(["-t", "200001010000"])
        .arg(&orphan_file)
        .status()
        .expect("spawn touch");
    assert!(touch.success(), "backdate orphan object");

    // §C.4.3 writer-vs-deleter quarantine: nothing is deleted the first time
    // it is seen unreachable. The first run RECORDS the candidate; only a
    // later run, finding it still unreachable after the grace window, deletes
    // it. Drive both phases explicitly rather than waiting an hour.
    let first = run_libra_command(&["maintenance", "run", "--task", "gc"], repo.path());
    assert_cli_success(&first, "gc first pass records the candidate");
    let first_out = String::from_utf8_lossy(&first.stdout);
    assert!(
        first_out.contains("removed 0 unreachable loose objects"),
        "the first pass must delete nothing: {first_out}"
    );
    assert!(
        orphan_file.exists(),
        "the first pass records the candidate, it does not delete it"
    );

    // Backdate the ledger entry so the candidate is past the grace window.
    let ledger_path = repo.path().join(".libra").join("gc-prune-candidates.json");
    let ledger: serde_json::Value =
        serde_json::from_slice(&fs::read(&ledger_path).expect("ledger written by the first pass"))
            .expect("ledger json");
    let aged: serde_json::Map<String, serde_json::Value> = ledger
        .as_object()
        .expect("ledger object")
        .keys()
        .map(|oid| (oid.clone(), serde_json::json!(0)))
        .collect();
    assert!(
        aged.contains_key(&orphan_oid),
        "the orphan is in the ledger: {ledger}"
    );
    fs::write(
        &ledger_path,
        serde_json::to_vec(&serde_json::Value::Object(aged)).expect("serialize"),
    )
    .expect("age the ledger");

    // Dry-run: counts both sides, deletes nothing.
    let dry = run_libra_command(
        &["maintenance", "run", "--dry-run", "--task", "gc"],
        repo.path(),
    );
    assert_cli_success(&dry, "gc dry-run");
    let dry_out = String::from_utf8_lossy(&dry.stdout);
    assert!(
        dry_out.contains("would remove 1 unreachable loose objects and 1 object-index rows"),
        "dry-run counts blob + row: {dry_out}"
    );
    assert!(orphan_file.exists(), "dry-run must not delete the blob");
    assert_eq!(
        object_index_row_count(&conn, &repo_id, &orphan_oid).await,
        1
    );

    // Real run: orphan blob AND its row are reclaimed; shared survives.
    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], repo.path());
    assert_cli_success(&gc, "gc real run");
    let gc_out = String::from_utf8_lossy(&gc.stdout);
    assert!(
        gc_out.contains("removed 1 unreachable loose objects and 1 object-index rows"),
        "real run reports blob + row: {gc_out}"
    );
    assert!(!orphan_file.exists(), "orphan blob reclaimed");
    assert_eq!(
        object_index_row_count(&conn, &repo_id, &orphan_oid).await,
        0
    );
    assert!(
        loose_object_file(repo.path(), &shared_oid).exists(),
        "reachable shared blob survives"
    );
    assert_eq!(
        object_index_row_count(&conn, &repo_id, &shared_oid).await,
        1,
        "reachable blob keeps its object_index row"
    );

    // Idempotent: nothing left to reclaim.
    let again = run_libra_command(&["maintenance", "run", "--task", "gc"], repo.path());
    assert_cli_success(&again, "gc idempotent run");
    assert!(
        String::from_utf8_lossy(&again.stdout)
            .contains("removed 0 unreachable loose objects and 0 object-index rows"),
        "second run is a no-op"
    );
}

/// GC-08 performance floor: the gc reachability walk plus prune preview over
/// a >10k-loose-object repository completes within a generous wall-clock
/// bound (no full-tree rescans or N+1 storage round-trips).
#[tokio::test]
#[serial]
async fn gc_ten_thousand_objects_within_budget() {
    use libra::utils::test::ChangeDirGuard;

    let repo = create_committed_repo_via_cli();
    {
        let _hash_guard = set_hash_kind_for_test(HashKind::Sha1);
        let _guard = ChangeDirGuard::new(repo.path());
        for i in 0..10_500u32 {
            let blob = git_internal::internal::object::blob::Blob::from_content(&format!(
                "perf blob {i}\n"
            ));
            libra::command::save_object(&blob, &blob.id).expect("save perf blob");
        }
    }

    let started = std::time::Instant::now();
    let out = run_libra_command(
        &["maintenance", "run", "--dry-run", "--task", "gc"],
        repo.path(),
    );
    assert_cli_success(&out, "gc dry-run over 10k objects");
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(60),
        "gc over 10k loose objects must stay within the wall-clock budget, took {elapsed:?}"
    );
}

/// plan-20260714 §C.4.3 `Boundary`: GC stops at a shallow graft instead of
/// reporting the clone as corrupt.
///
/// A shallow clone deliberately does not have its boundary commits' parents.
/// The roots walk followed `parent_commit_ids` unconditionally, so the first
/// absent parent surfaced as "reachable commit <oid> cannot be read while
/// computing GC roots" and `gc`, `repack` and `prune` failed outright on
/// every shallow repository — the one class of repository where routine
/// maintenance matters most, because it was made small on purpose.
#[test]
fn gc_stops_at_a_shallow_boundary_instead_of_reporting_corruption() {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();

    // Two more commits, so there is a real parent edge to cut.
    for n in 1..=2 {
        fs::write(root.join(format!("f{n}.txt")), format!("{n}\n")).expect("write");
        assert_cli_success(
            &run_libra_command(&["add", &format!("f{n}.txt")], root),
            "add",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", &format!("c{n}"), "--no-verify"], root),
            "commit",
        );
    }

    // HEAD~1 becomes the graft point: declare it a boundary and then remove
    // its parent's object, exactly as a `--depth` clone would leave things.
    let boundary =
        String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD~1"], root).stdout)
            .trim()
            .to_string();
    let cut = String::from_utf8_lossy(&run_libra_command(&["rev-parse", "HEAD~2"], root).stdout)
        .trim()
        .to_string();
    assert!(
        !boundary.is_empty() && !cut.is_empty(),
        "resolved both commits"
    );

    // A real `--depth` clone has no reflog entries for commits it never
    // received; this synthetic one does, and they are reachability roots in
    // their own right. Expire them so the fixture models the shape under
    // test — the parent EDGE — rather than an unrelated dangling root.
    //
    // `--expire=all`, not `--expire=now`: the cutoff is `timestamp < c`, so
    // entries written in the SAME second as the expire survive `now` and the
    // fixture would keep the grafted-away commit alive depending on how the
    // second boundary fell.
    assert_cli_success(
        &run_libra_command(&["reflog", "expire", "--expire=all", "--all"], root),
        "expire reflog roots the graft would not have",
    );

    fs::write(root.join(".libra").join("shallow"), format!("{boundary}\n"))
        .expect("write shallow metadata");
    let cut_path = root
        .join(".libra")
        .join("objects")
        .join(&cut[..2])
        .join(&cut[2..]);
    fs::remove_file(&cut_path).expect("remove the grafted-away parent object");

    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], root);
    let stderr = String::from_utf8_lossy(&gc.stderr);
    assert!(
        gc.status.success(),
        "gc must stop at the boundary, not demand the parent this clone never had: {stderr}"
    );
    assert!(
        !stderr.contains(&cut),
        "and must not name the grafted-away commit at all: {stderr}"
    );

    // The boundary commit itself survives — it is reachable, only its
    // ancestry is absent.
    assert_cli_success(
        &run_libra_command(&["cat-file", "-t", &boundary], root),
        "boundary commit still readable after gc",
    );
}

/// Malformed shallow metadata fails CLOSED: reading it as "no boundaries"
/// would put the walk straight back into the corruption report it is meant
/// to prevent, and reading it as "everything is a boundary" would stop the
/// walk early and let live objects be pruned.
#[test]
fn gc_refuses_to_prune_with_unparseable_shallow_metadata() {
    let repo = create_committed_repo_via_cli();
    fs::write(
        repo.path().join(".libra").join("shallow"),
        "not-an-object-id\n",
    )
    .expect("write shallow metadata");

    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], repo.path());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&gc.stdout),
        String::from_utf8_lossy(&gc.stderr)
    );
    assert!(
        !gc.status.success(),
        "gc must refuse rather than prune under unreadable shallow metadata: {combined}"
    );
    assert!(
        combined.contains("shallow"),
        "and must say which metadata it could not trust: {combined}"
    );
}

// ---------------------------------------------------------------------------
// plan-20260714 §C.4.3 writer-vs-deleter: the maintenance exclusion lock
// ---------------------------------------------------------------------------

/// Hold the repository maintenance lock the way a concurrent publisher does.
///
/// A separate process is the honest fixture: `flock` is advisory and
/// per-open-file-description, so a lock taken in THIS process would not be
/// seen the same way by the spawned `libra` under test on every platform.
/// Kills and reaps the helper on drop, including on an assertion unwind — a
/// leaked holder keeps the lock (and a 600-second sleep) alive for every
/// later test in this process.
struct LockHolder(std::process::Child);

impl Drop for LockHolder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn hold_shared_maintenance_lock(repo: &std::path::Path) -> LockHolder {
    let lock_path = repo.join(".libra").join("maintenance.lock");
    let script = format!(
        "import fcntl, sys, time\n\
         f = open({path:?}, 'a+')\n\
         fcntl.flock(f, fcntl.LOCK_SH)\n\
         sys.stdout.write('locked\\n')\n\
         sys.stdout.flush()\n\
         time.sleep(600)\n",
        path = lock_path.to_string_lossy().to_string()
    );
    let mut child = std::process::Command::new("python3")
        .args(["-c", &script])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the publisher holding the maintenance lock");
    // Wait for the lock to actually be held before returning.
    use std::io::{BufRead, BufReader};
    let stdout = child.stdout.take().expect("publisher stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("publisher ready line");
    assert_eq!(line.trim(), "locked", "the publisher must hold the lock");
    LockHolder(child)
}

/// §C.4.3: a deletion phase never unlinks while a publisher may be
/// publishing — it DEFERS.
///
/// The two-scan quarantine proves an object was unreachable at two separated
/// moments; it cannot prove nothing referenced it in between, and neither can
/// a database transaction, because a worktree index, a sidecar and an
/// agent-run manifest are files. This is the exclusion that closes that
/// interval, and the test drives it end to end: with a publisher holding the
/// lock the aged candidate SURVIVES and gc says so; once the publisher exits
/// the same candidate is deleted.
#[test]
fn gc_defers_deletion_while_a_publisher_holds_the_maintenance_lock() {
    let repo = create_committed_repo_via_cli();

    fs::write(repo.path().join("orphan.md"), "orphan for the lock test\n").unwrap();
    let hashed = run_libra_command(&["hash-object", "-w", "orphan.md"], repo.path());
    assert_cli_success(&hashed, "hash-object -w");
    let orphan_oid = String::from_utf8_lossy(&hashed.stdout).trim().to_string();
    fs::remove_file(repo.path().join("orphan.md")).unwrap();
    let orphan_file = loose_object_file(repo.path(), &orphan_oid);
    assert!(orphan_file.exists(), "orphan loose object on disk");
    // Age it past the loose-object grace window, as the PD-04 fixture does.
    let touch = std::process::Command::new("touch")
        .args(["-t", "200001010000"])
        .arg(&orphan_file)
        .status()
        .expect("spawn touch");
    assert!(touch.success(), "backdate the orphan object");

    // Quarantine phase 1: record the candidate.
    let first = run_libra_command(&["maintenance", "run", "--task", "gc"], repo.path());
    assert_cli_success(&first, "gc records the candidate");
    let ledger_path = repo.path().join(".libra").join("gc-prune-candidates.json");
    let ledger: serde_json::Value =
        serde_json::from_slice(&fs::read(&ledger_path).expect("ledger")).expect("ledger json");
    let aged: serde_json::Map<String, serde_json::Value> = ledger
        .as_object()
        .expect("ledger object")
        .keys()
        .map(|oid| (oid.clone(), serde_json::json!(0)))
        .collect();
    assert!(aged.contains_key(&orphan_oid), "candidate recorded");
    fs::write(&ledger_path, serde_json::to_vec(&aged).expect("serialize")).expect("age the ledger");

    // A publisher is running: deletion must be deferred, not performed.
    let publisher = hold_shared_maintenance_lock(repo.path());
    let blocked = run_libra_command(&["maintenance", "run", "--task", "gc"], repo.path());
    let blocked_out = format!(
        "{}{}",
        String::from_utf8_lossy(&blocked.stdout),
        String::from_utf8_lossy(&blocked.stderr)
    );
    assert!(
        blocked_out.contains("deferred the deletion"),
        "gc must report the deferral rather than deleting: {blocked_out}"
    );
    assert!(
        orphan_file.exists(),
        "the candidate must survive while a publisher holds the lock: {blocked_out}"
    );

    // Publisher gone: the same candidate is now deleted.
    drop(publisher);
    let after = run_libra_command(&["maintenance", "run", "--task", "gc"], repo.path());
    assert_cli_success(&after, "gc after the publisher exits");
    let after_out = String::from_utf8_lossy(&after.stdout);
    assert!(
        !orphan_file.exists(),
        "once nothing is publishing, the aged candidate is reclaimed: {after_out}"
    );
    // And it is reclaimed by the VERY NEXT run: a deferral must not reset the
    // quarantine clock. If the deferred pass had dropped the candidate from
    // the ledger, this run would re-quarantine it instead — which, in a
    // repository with a long-running session, would mean pruning never
    // happens at all.
    assert!(
        !after_out.contains("newly unreachable"),
        "the deferral must preserve the candidate's first-seen timestamp: {after_out}"
    );
}

/// §C.4.3: an agent-run directory whose manifest is missing fails the walk
/// CLOSED, at any age.
///
/// The blobs an interrupted run owns are exactly as unlisted after a day as
/// after a minute, so an age-bounded "skip it" turned "I cannot enumerate
/// this run's roots" into "this run has none" — and pruned what it owned.
#[test]
fn gc_refuses_to_prune_with_an_unenumerable_agent_run() {
    let repo = create_committed_repo_via_cli();
    let run_dir = repo
        .path()
        .join(".libra")
        .join("sessions")
        .join("agent-runs")
        .join("20260101T000000Z-abcdef01");
    fs::create_dir_all(&run_dir).expect("create run dir");
    fs::write(run_dir.join("findings.md"), "findings the run owns\n").expect("write findings");
    // Backdate it well past any grace window: age must not buy permission.
    let touch = std::process::Command::new("touch")
        .args(["-t", "200001010000"])
        .arg(&run_dir)
        .status()
        .expect("spawn touch");
    assert!(touch.success(), "backdate the run directory");

    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], repo.path());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&gc.stdout),
        String::from_utf8_lossy(&gc.stderr)
    );
    assert!(
        !gc.status.success(),
        "an unenumerable mandatory root must fail the walk closed: {combined}"
    );
    assert!(
        combined.contains("no manifest") && combined.contains("agent clean"),
        "and must name the explicit route out: {combined}"
    );
}

/// A manifest that is valid JSON but not a JSON OBJECT (`[]`, `null`, a
/// string) fails closed too: every field lookup on it returns `None`, which
/// reads as "this run declares no roots" and prunes the blob it owned.
#[test]
fn gc_refuses_a_manifest_that_is_not_a_json_object() {
    let repo = create_committed_repo_via_cli();
    let run_dir = repo
        .path()
        .join(".libra")
        .join("sessions")
        .join("agent-runs")
        .join("20260101T000000Z-abcdef02");
    fs::create_dir_all(&run_dir).expect("create run dir");
    fs::write(run_dir.join("manifest.json"), "[]").expect("write manifest");

    let gc = run_libra_command(&["maintenance", "run", "--task", "gc"], repo.path());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&gc.stdout),
        String::from_utf8_lossy(&gc.stderr)
    );
    assert!(
        !gc.status.success(),
        "a non-object manifest must fail closed: {combined}"
    );
    assert!(
        combined.contains("not a JSON object"),
        "and must say why: {combined}"
    );
}
