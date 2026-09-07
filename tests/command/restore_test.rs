//! Tests restore command paths for worktree and index targets along with pathspec handling.
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use libra::{
    internal::{branch::TRACES_BRANCH, db::get_db_conn_instance, head::Head, model::reference},
    utils::test::ChangeDirGuard,
};
use sea_orm::{ActiveModelTrait, Set};

use super::*;

#[test]
fn test_restore_cli_outside_repository_returns_fatal_128() {
    let temp = tempdir().unwrap();
    let output = run_libra_command(&["restore", "."], temp.path());
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("fatal: not a libra repository"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn test_restore_source_head_unborn_returns_error_without_falling_back() {
    let repo = tempdir().expect("failed to create repository root");
    init_repo_via_cli(repo.path());
    std::fs::write(repo.path().join("tracked.txt"), "modified\n")
        .expect("failed to write tracked file");

    let output = run_libra_command(&["restore", "--source", "HEAD", "tracked.txt"], repo.path());
    assert_eq!(output.status.code(), Some(128));

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        human.contains("fatal: failed to resolve checkout source"),
        "unexpected stderr: {human}"
    );
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert_eq!(report.exit_code, 128);

    let content = std::fs::read_to_string(repo.path().join("tracked.txt"))
        .expect("failed to read tracked file");
    assert_eq!(content, "modified\n");
}

#[test]
fn test_restore_missing_pathspec_returns_cli_invalid_target() {
    let repo = create_committed_repo_via_cli();

    let output = run_libra_command(&["restore", "missing.txt"], repo.path());
    assert_eq!(output.status.code(), Some(129));

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        human.contains("fatal: pathspec 'missing.txt' did not match any files"),
        "unexpected stderr: {human}"
    );
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert_eq!(report.exit_code, 129);
}

#[test]
fn test_restore_pathspec_from_file_restores_listed_paths() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();

    std::fs::write(p.join("a.txt"), "committed-a\n").unwrap();
    std::fs::write(p.join("b.txt"), "committed-b\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "a.txt", "b.txt"], p), "add a/b");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "add a/b", "--no-verify"], p),
        "commit a/b",
    );

    // Dirty both files, then list them in a pathspec file.
    std::fs::write(p.join("a.txt"), "dirty-a\n").unwrap();
    std::fs::write(p.join("b.txt"), "dirty-b\n").unwrap();
    std::fs::write(p.join("specs.txt"), "a.txt\nb.txt\n").unwrap();

    let output = run_libra_command(&["restore", "--pathspec-from-file", "specs.txt"], p);
    assert_cli_success(&output, "restore --pathspec-from-file");

    assert_eq!(
        std::fs::read_to_string(p.join("a.txt")).unwrap(),
        "committed-a\n",
        "a.txt should be restored from the pathspec file"
    );
    assert_eq!(
        std::fs::read_to_string(p.join("b.txt")).unwrap(),
        "committed-b\n",
        "b.txt should be restored from the pathspec file"
    );
}

#[tokio::test]
#[serial(cwd)]
async fn test_restore_source_does_not_fall_back_from_unborn_branch_to_hash_prefix() {
    let repo = create_committed_repo_via_cli();
    let _guard = ChangeDirGuard::new(repo.path());

    let head_commit = Head::current_commit()
        .await
        .expect("expected committed repository");
    let branch_name = head_commit.to_string()[..7].to_string();

    let db = get_db_conn_instance().await;
    reference::ActiveModel {
        name: Set(Some(branch_name.clone())),
        kind: Set(reference::ConfigKind::Branch),
        commit: Set(None),
        remote: Set(None),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("failed to insert unborn branch row");

    std::fs::write(repo.path().join("tracked.txt"), "modified\n")
        .expect("failed to modify tracked file");

    let output = run_libra_command(
        &["restore", "--source", &branch_name, "tracked.txt"],
        repo.path(),
    );
    assert!(
        !output.status.success(),
        "restore unexpectedly succeeded: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let content = std::fs::read_to_string(repo.path().join("tracked.txt"))
        .expect("failed to read tracked file");
    assert_eq!(
        content, "modified\n",
        "restore should not overwrite from hash fallback"
    );
}

// ── Positive paths: worktree / staged / JSON / confirmation ─────────────

#[test]
fn test_restore_worktree_overwrites_modification_with_committed_blob() {
    let repo = create_committed_repo_via_cli();
    std::fs::write(repo.path().join("tracked.txt"), "modified\n")
        .expect("failed to modify tracked file");

    let output = run_libra_command(&["restore", "tracked.txt"], repo.path());
    assert_cli_success(&output, "restore from index should succeed");

    let restored = std::fs::read_to_string(repo.path().join("tracked.txt"))
        .expect("failed to read restored file");
    assert_eq!(
        restored, "tracked\n",
        "worktree restore should reset content to the indexed blob"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Updated 1 path(s) from the index"),
        "expected confirmation message, got stdout: {stdout}"
    );
}

#[test]
fn test_restore_staged_resets_index_entry_to_head() {
    let repo = create_committed_repo_via_cli();

    std::fs::write(repo.path().join("tracked.txt"), "staged change\n")
        .expect("failed to update tracked file");
    let add = run_libra_command(&["add", "tracked.txt"], repo.path());
    assert_cli_success(&add, "add should stage the tracked change");

    let restore = run_libra_command(&["restore", "--staged", "tracked.txt"], repo.path());
    assert_cli_success(&restore, "restore --staged should succeed");

    let stdout = String::from_utf8_lossy(&restore.stdout);
    assert!(
        stdout.contains("Updated 1 path(s) from HEAD"),
        "expected confirmation message naming HEAD source, got stdout: {stdout}"
    );

    let status = run_libra_command(&["status", "--json"], repo.path());
    assert_cli_success(&status, "status --json should succeed after staged restore");
    let report = parse_json_stdout(&status);
    let staged = report["data"]["staged"]
        .as_object()
        .expect("status data should expose staged");
    let staged_total = ["new", "modified", "deleted"]
        .iter()
        .map(|key| {
            staged
                .get(*key)
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0)
        })
        .sum::<usize>();
    assert_eq!(
        staged_total, 0,
        "after restore --staged, no staged entries should remain (got {staged:?})"
    );

    let worktree = std::fs::read_to_string(repo.path().join("tracked.txt"))
        .expect("failed to read worktree file");
    assert_eq!(
        worktree, "staged change\n",
        "restore --staged must not touch the worktree copy"
    );
}

#[test]
fn test_restore_json_envelope_reports_restored_files() {
    let repo = create_committed_repo_via_cli();
    std::fs::write(repo.path().join("tracked.txt"), "modified\n")
        .expect("failed to modify tracked file");

    let output = run_libra_command(&["restore", "--json", "tracked.txt"], repo.path());
    assert_cli_success(&output, "restore --json should succeed");

    let envelope = parse_json_stdout(&output);
    assert_eq!(envelope["ok"], Value::Bool(true));
    assert_eq!(envelope["command"], Value::String("restore".to_string()));

    let data = &envelope["data"];
    assert_eq!(data["worktree"], Value::Bool(true));
    assert_eq!(data["staged"], Value::Bool(false));
    assert!(
        data["source"].is_null(),
        "default restore (no --source) should leave source as null, got: {}",
        data["source"]
    );

    let restored = data["restored_files"]
        .as_array()
        .expect("restored_files should be an array");
    assert_eq!(
        restored.len(),
        1,
        "expected exactly one restored file, got: {restored:?}"
    );
    assert_eq!(
        restored[0],
        Value::String("tracked.txt".to_string()),
        "expected tracked.txt in restored_files"
    );

    let deleted = data["deleted_files"]
        .as_array()
        .expect("deleted_files should be an array");
    assert!(
        deleted.is_empty(),
        "no deletions expected, got: {deleted:?}"
    );

    let restored_content = std::fs::read_to_string(repo.path().join("tracked.txt"))
        .expect("failed to read restored file");
    assert_eq!(restored_content, "tracked\n");
}

#[test]
fn test_restore_quiet_suppresses_confirmation_but_still_restores() {
    let repo = create_committed_repo_via_cli();
    std::fs::write(repo.path().join("tracked.txt"), "modified\n")
        .expect("failed to modify tracked file");

    let output = run_libra_command(&["--quiet", "restore", "tracked.txt"], repo.path());
    assert_cli_success(&output, "quiet restore should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.is_empty(),
        "quiet mode should produce no stdout, got: {stdout}"
    );

    let restored = std::fs::read_to_string(repo.path().join("tracked.txt"))
        .expect("failed to read restored file");
    assert_eq!(
        restored, "tracked\n",
        "quiet mode must still perform the restore"
    );
}

// ── Locked-branch guard ─────────────────────────────────────────────────

#[test]
fn test_restore_source_refuses_locked_intent_branch() {
    let repo = create_committed_repo_via_cli();
    std::fs::write(repo.path().join("tracked.txt"), "modified\n")
        .expect("failed to modify tracked file");

    let output = run_libra_command(
        &["restore", "--source", "intent", "tracked.txt"],
        repo.path(),
    );
    assert_eq!(
        output.status.code(),
        Some(128),
        "locked-branch restore should exit 128 (fatal)"
    );

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        human.contains("refusing to restore from locked branch 'intent'"),
        "unexpected stderr: {human}"
    );
    assert_eq!(report.error_code, "LBR-CLI-003");
    assert_eq!(report.exit_code, 128);

    let content = std::fs::read_to_string(repo.path().join("tracked.txt"))
        .expect("failed to read tracked file");
    assert_eq!(
        content, "modified\n",
        "locked-source guard must not modify the worktree"
    );
}

#[test]
fn test_restore_source_refuses_locked_branch_with_revision_suffix() {
    // is_locked_revision strips `~1` / `^` / `@{0}` so users cannot
    // end-run the guard with `traces~1` or similar.
    let repo = create_committed_repo_via_cli();
    std::fs::write(repo.path().join("tracked.txt"), "modified\n")
        .expect("failed to modify tracked file");

    let output = run_libra_command(
        &["restore", "--source", "traces~1", "tracked.txt"],
        repo.path(),
    );
    assert_eq!(
        output.status.code(),
        Some(128),
        "locked-branch restore with revision suffix should still exit 128"
    );

    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert!(
        human.contains("refusing to restore from locked branch 'traces~1'"),
        "unexpected stderr: {human}"
    );
    assert_eq!(report.error_code, "LBR-CLI-003");
}

#[tokio::test]
#[serial(cwd)]
async fn test_restore_worktree_refuses_ai_managed_current_branch() {
    let repo = create_committed_repo_via_cli();
    {
        let _guard = ChangeDirGuard::new(repo.path());
        Head::update_result(Head::Branch(TRACES_BRANCH.to_string()), None)
            .await
            .expect("point HEAD at traces");
    }
    std::fs::write(repo.path().join("tracked.txt"), "modified\n")
        .expect("failed to modify tracked file");

    let output = run_libra_command(&["restore", "tracked.txt"], repo.path());

    assert_eq!(output.status.code(), Some(128));
    let (human, report) = parse_cli_error_stderr(&output.stderr);
    assert_eq!(report.error_code, "LBR-CONFLICT-002");
    assert!(
        human.contains("refusing to restore worktree while on locked branch 'traces'"),
        "unexpected stderr: {human}"
    );
    let content = std::fs::read_to_string(repo.path().join("tracked.txt"))
        .expect("failed to read tracked file");
    assert_eq!(
        content, "modified\n",
        "locked-current-branch guard must not modify the worktree"
    );
}

#[test]
fn restore_no_progress_flag_is_accepted_noop() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("tracked.txt"), "modified\n").unwrap();
    // `--no-progress` is accepted and a no-op: Libra's restore renders no
    // progress meter, so the restore proceeds and reverts the file.
    let out = run_libra_command(&["restore", "--no-progress", "tracked.txt"], p);
    assert_cli_success(&out, "restore --no-progress tracked.txt");
    assert_eq!(
        std::fs::read_to_string(p.join("tracked.txt")).unwrap(),
        "tracked\n"
    );
}

#[test]
fn restore_no_overlay_flag_is_accepted() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::write(p.join("tracked.txt"), "modified\n").unwrap();
    // `--no-overlay` selects the default (non-overlay) mode explicitly; restore
    // proceeds normally. (Its real toggle counterpart `--overlay` — which
    // preserves source-absent tracked paths — is covered by the overlay tests
    // below.)
    let out = run_libra_command(&["restore", "--no-overlay", "tracked.txt"], p);
    assert_cli_success(&out, "restore --no-overlay tracked.txt");
    assert_eq!(
        std::fs::read_to_string(p.join("tracked.txt")).unwrap(),
        "tracked\n"
    );
}

// ---------------------------------------------------------------------------
// Conflict-stage restore: --ours / --theirs / -2 / -3 / --ignore-unmerged and
// the unmerged guard. Each test builds a real merge conflict on `tracked.txt`
// so the index carries stage 1 (base) / 2 (ours = main) / 3 (theirs = feature).
// ---------------------------------------------------------------------------

fn commit_file_cli(repo: &std::path::Path, file: &str, content: &str, message: &str) {
    std::fs::write(repo.join(file), content).expect("write file");
    assert_cli_success(&run_libra_command(&["add", file], repo), "add file");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", message, "--no-verify"], repo),
        "commit file",
    );
}

/// Build a repo with an unresolved merge conflict on `tracked.txt`:
/// stage 2 (ours) = "main change\n", stage 3 (theirs) = "feature change\n".
fn create_conflicted_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let path = repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], path),
        "checkout feature",
    );
    commit_file_cli(path, "tracked.txt", "feature change\n", "feature change");
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], path),
        "checkout main",
    );
    commit_file_cli(path, "tracked.txt", "main change\n", "main change");
    // Conflicting merge leaves tracked.txt unmerged (index stages 1/2/3) and a
    // conflict-marked working tree. The non-zero exit is expected.
    let _ = run_libra_command(&["merge", "feature"], path);
    repo
}

fn create_two_path_conflicted_repo() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let path = repo.path();
    for file in ["a-first.txt", "z-blocked.txt"] {
        std::fs::write(path.join(file), "base\n").expect("write conflict base");
    }
    assert_cli_success(
        &run_libra_command(&["add", "a-first.txt", "z-blocked.txt"], path),
        "stage conflict bases",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "conflict bases", "--no-verify"], path),
        "commit conflict bases",
    );
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], path),
        "checkout feature",
    );
    for file in ["a-first.txt", "z-blocked.txt"] {
        std::fs::write(path.join(file), "feature change\n").expect("write feature change");
    }
    assert_cli_success(
        &run_libra_command(&["add", "a-first.txt", "z-blocked.txt"], path),
        "stage feature changes",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "feature changes", "--no-verify"], path),
        "commit feature changes",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], path),
        "checkout main",
    );
    for file in ["a-first.txt", "z-blocked.txt"] {
        std::fs::write(path.join(file), "main change\n").expect("write main change");
    }
    assert_cli_success(
        &run_libra_command(&["add", "a-first.txt", "z-blocked.txt"], path),
        "stage main changes",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main changes", "--no-verify"], path),
        "commit main changes",
    );
    let merged = run_libra_command(&["merge", "feature"], path);
    assert!(!merged.status.success(), "merge must leave two conflicts");
    repo
}

/// Build a modify/delete conflict on `tracked.txt` by merging `feature` into
/// `main` (ours = main, theirs = feature). When `theirs_deletes` is true the
/// theirs side (feature) deletes the file and ours (main) modifies it, so the
/// merged index has stage 2 but NO stage 3; otherwise ours (main) deletes and
/// theirs modifies, so stage 2 is absent. Used to exercise the missing-stage
/// (delete-on-restore) behavior of `--ours`/`--theirs`.
fn create_modify_delete_conflict(theirs_deletes: bool) -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    let path = repo.path();
    assert_cli_success(
        &run_libra_command(&["branch", "feature"], path),
        "create feature",
    );
    assert_cli_success(
        &run_libra_command(&["checkout", "feature"], path),
        "checkout feature",
    );
    if theirs_deletes {
        assert_cli_success(
            &run_libra_command(&["rm", "tracked.txt"], path),
            "rm on feature",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "feature deletes", "--no-verify"], path),
            "commit feature deletion",
        );
    } else {
        commit_file_cli(path, "tracked.txt", "feature change\n", "feature modifies");
    }
    assert_cli_success(
        &run_libra_command(&["checkout", "main"], path),
        "checkout main",
    );
    if theirs_deletes {
        commit_file_cli(path, "tracked.txt", "main change\n", "main modifies");
    } else {
        assert_cli_success(
            &run_libra_command(&["rm", "tracked.txt"], path),
            "rm on main",
        );
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "main deletes", "--no-verify"], path),
            "commit main deletion",
        );
    }
    // Modify/delete conflict: merge leaves one of stage 2/3 absent. Non-zero exit expected.
    let _ = run_libra_command(&["merge", "feature"], path);
    repo
}

#[test]
fn test_restore_ours_writes_stage2_blob() {
    let repo = create_conflicted_repo();
    let out = run_libra_command(&["restore", "--ours", "tracked.txt"], repo.path());
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
        "main change\n",
    );
}

#[test]
fn test_restore_theirs_writes_stage3_blob() {
    let repo = create_conflicted_repo();
    let out = run_libra_command(&["restore", "--theirs", "tracked.txt"], repo.path());
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
        "feature change\n",
    );
}

// Missing-stage (modify/delete) restore: the requested side deleted the file,
// so restoring it means removing it from the worktree in the default
// (no-overlay) mode, exactly like `git restore` (lore.md 1.2 Git-fidelity fix).

#[test]
fn test_restore_theirs_modify_delete_removes_worktree_file() {
    // theirs (feature) deleted tracked.txt → stage 3 absent.
    let repo = create_modify_delete_conflict(true);
    let out = run_libra_command(&["restore", "--theirs", "tracked.txt"], repo.path());
    assert!(
        out.status.success(),
        "restore --theirs on a modify/delete should succeed (exit 0); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !repo.path().join("tracked.txt").exists(),
        "restoring the deleted side must remove the worktree file"
    );
}

#[test]
fn test_restore_theirs_modify_delete_removes_empty_worktree_directory() {
    let repo = create_modify_delete_conflict(true);
    let path = repo.path().join("tracked.txt");
    std::fs::remove_file(&path).expect("remove conflicted worktree file");
    std::fs::create_dir(&path).expect("materialize gitlink-like directory");

    let out = run_libra_command(&["restore", "--theirs", "tracked.txt"], repo.path());
    assert_cli_success(&out, "restore deleted conflict side over empty directory");
    assert!(
        !path.exists(),
        "restoring a deletion must remove an empty materialized directory"
    );
}

#[test]
fn test_restore_theirs_modify_delete_refuses_nonempty_worktree_directory() {
    let repo = create_modify_delete_conflict(true);
    let path = repo.path().join("tracked.txt");
    std::fs::remove_file(&path).expect("remove conflicted worktree file");
    std::fs::create_dir(&path).expect("materialize gitlink-like directory");
    std::fs::write(path.join("nested.txt"), "user data\n").expect("write nested user data");

    let out = run_libra_command(&["restore", "--theirs", "tracked.txt"], repo.path());
    assert_eq!(out.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("refusing to replace non-empty worktree directory 'tracked.txt'"),
        "unexpected stderr: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("nested.txt")).expect("nested data survives"),
        "user data\n"
    );
}

#[test]
fn test_restore_ours_delete_modify_removes_worktree_file() {
    // ours (main) deleted tracked.txt → stage 2 absent.
    let repo = create_modify_delete_conflict(false);
    let out = run_libra_command(&["restore", "--ours", "tracked.txt"], repo.path());
    assert!(
        out.status.success(),
        "restore --ours on a delete/modify should succeed (exit 0); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !repo.path().join("tracked.txt").exists(),
        "restoring the deleted side must remove the worktree file"
    );
}

#[test]
fn test_restore_theirs_modify_delete_json_reports_deleted() {
    // The deletion is agent-facing: it must appear in deleted_files, not restored_files.
    let repo = create_modify_delete_conflict(true);
    let out = run_libra_command(
        &["--json", "restore", "--theirs", "tracked.txt"],
        repo.path(),
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json = parse_json_stdout(&out);
    let deleted = json["data"]["deleted_files"]
        .as_array()
        .expect("deleted_files array");
    assert!(
        deleted.iter().any(|v| v.as_str() == Some("tracked.txt")),
        "deleted_files should list the removed path, got {json}"
    );
    assert!(
        json["data"]["restored_files"]
            .as_array()
            .is_some_and(|a| a.is_empty()),
        "a pure deletion must not report restored_files"
    );
}

#[test]
fn test_restore_overlay_missing_stage_errors() {
    // Overlay mode never removes paths, so a missing stage is an error (exit 128),
    // matching Git's overlay-mode `does not have their version`.
    let repo = create_modify_delete_conflict(true); // stage 3 absent
    let out = run_libra_command(
        &["restore", "--overlay", "--theirs", "tracked.txt"],
        repo.path(),
    );
    assert_eq!(
        out.status.code(),
        Some(128),
        "overlay mode must error on a missing stage; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("does not have"),
        "overlay error should mention the missing side, got: {stderr}"
    );
}

#[test]
fn test_restore_short_aliases_2_3() {
    let repo = create_conflicted_repo();
    assert_cli_success(
        &run_libra_command(&["restore", "-2", "tracked.txt"], repo.path()),
        "-2 restores ours",
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
        "main change\n",
    );
    assert_cli_success(
        &run_libra_command(&["restore", "-3", "tracked.txt"], repo.path()),
        "-3 restores theirs",
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
        "feature change\n",
    );
}

#[test]
fn test_restore_unmerged_path_blocks_with_exit_128() {
    let repo = create_conflicted_repo();
    let out = run_libra_command(&["--json", "restore", "tracked.txt"], repo.path());
    assert_eq!(out.status.code(), Some(128));
    let report: serde_json::Value =
        serde_json::from_slice(&out.stderr).expect("expected stderr JSON in --json mode");
    assert_eq!(report["error_code"], "LBR-CONFLICT-001");
    assert!(
        report["message"]
            .as_str()
            .unwrap_or_default()
            .contains("is unmerged"),
        "unexpected message: {}",
        report["message"]
    );
}

#[test]
fn test_restore_ignore_unmerged_skips_block() {
    let repo = create_conflicted_repo();
    let before = std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap();
    // The only matched path is unmerged; --ignore-unmerged skips it and the
    // command succeeds, leaving the conflicted working tree untouched.
    let out = run_libra_command(
        &[
            "restore",
            "--ignore-unmerged",
            "--source",
            "HEAD",
            "tracked.txt",
        ],
        repo.path(),
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = std::fs::read_to_string(repo.path().join("tracked.txt")).unwrap();
    assert_eq!(before, after, "unmerged file must be left untouched");
}

#[test]
fn test_restore_ignore_unmerged_exact_deleted_path_is_noop() {
    // Regression: when the only matched pathspec is an unmerged path whose
    // worktree file has been deleted, `--ignore-unmerged` must skip it cleanly
    // (a no-op) rather than failing the `PathspecNotMatched` precheck.
    let repo = create_conflicted_repo();
    std::fs::remove_file(repo.path().join("tracked.txt")).expect("delete conflicted file");
    // Both the canonical and the `./`-prefixed spelling must skip cleanly; the
    // skip decision reuses the pathspec matcher, so it is spelling-robust.
    for spelling in ["tracked.txt", "./tracked.txt"] {
        let out = run_libra_command(
            &["restore", "--ignore-unmerged", "--source", "HEAD", spelling],
            repo.path(),
        );
        assert!(
            out.status.success(),
            "exact unmerged pathspec {spelling:?} must skip cleanly, stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The unmerged path was skipped, so it is NOT resurrected from HEAD.
        assert!(
            !repo.path().join("tracked.txt").exists(),
            "skipped unmerged path must not be restored ({spelling:?})",
        );
    }
}

#[test]
fn test_restore_ours_keeps_index_unmerged() {
    let repo = create_conflicted_repo();
    assert_cli_success(
        &run_libra_command(&["restore", "--ours", "tracked.txt"], repo.path()),
        "restore --ours",
    );
    // The index is intentionally left unmerged, so a plain restore still blocks.
    let out = run_libra_command(&["restore", "tracked.txt"], repo.path());
    assert_eq!(out.status.code(), Some(128));
}

#[test]
fn test_restore_ours_staged_rejected_by_clap() {
    let repo = create_committed_repo_via_cli();
    let out = run_libra_command(
        &["restore", "--ours", "--staged", "tracked.txt"],
        repo.path(),
    );
    // libra remaps clap conflict errors to CliInvalidArguments (exit 129).
    assert_eq!(out.status.code(), Some(129));
}

#[test]
fn test_restore_conflict_flags_mutually_exclusive() {
    let repo = create_committed_repo_via_cli();
    let path = repo.path();
    // libra remaps clap conflict errors to CliInvalidArguments (exit 129).
    assert_eq!(
        run_libra_command(&["restore", "--ours", "--theirs", "tracked.txt"], path)
            .status
            .code(),
        Some(129),
        "--ours conflicts with --theirs",
    );
    assert_eq!(
        run_libra_command(
            &["restore", "--ours", "--source", "HEAD", "tracked.txt"],
            path
        )
        .status
        .code(),
        Some(129),
        "--ours conflicts with --source",
    );
    assert_eq!(
        run_libra_command(
            &["restore", "--ignore-unmerged", "--ours", "tracked.txt"],
            path
        )
        .status
        .code(),
        Some(129),
        "--ignore-unmerged conflicts with --ours",
    );
}

// ---------------------------------------------------------------------------
// Overlay mode: `--overlay` only creates/updates source paths and never removes
// tracked paths absent from the source; the default (`--no-overlay`) removes
// them. The two form a last-one-wins toggle.
// ---------------------------------------------------------------------------

/// A repo whose HEAD has `extra.txt` but whose parent commit (`HEAD~1`) does
/// not, so restoring from `HEAD~1` makes `extra.txt` a path absent from source.
fn repo_with_extra_over_parent() -> tempfile::TempDir {
    let repo = create_committed_repo_via_cli();
    commit_file_cli(repo.path(), "extra.txt", "extra\n", "add extra");
    repo
}

#[test]
fn test_restore_overlay_keeps_files_absent_from_source() {
    let repo = repo_with_extra_over_parent();
    let p = repo.path();
    assert!(p.join("extra.txt").exists());
    let out = run_libra_command(&["restore", "--overlay", "--source", "HEAD~1", "."], p);
    assert_cli_success(&out, "restore --overlay");
    assert!(
        p.join("extra.txt").exists(),
        "overlay mode must not remove a file absent from the source",
    );
}

#[test]
fn test_restore_default_removes_files_absent_from_source() {
    let repo = repo_with_extra_over_parent();
    let p = repo.path();
    assert!(p.join("extra.txt").exists());
    // Default (no-overlay): files absent from the source are removed.
    let out = run_libra_command(&["restore", "--source", "HEAD~1", "."], p);
    assert_cli_success(&out, "restore default (no-overlay)");
    assert!(
        !p.join("extra.txt").exists(),
        "default no-overlay must remove a file absent from the source",
    );
}

const TEST_GITLINK_OID: &str = "1111111111111111111111111111111111111111";

fn commit_gitlink(repo: &Path) {
    let spec = format!("160000,{TEST_GITLINK_OID},vendor/sub");
    assert_cli_success(
        &run_libra_command(&["update-index", "--cacheinfo", &spec], repo),
        "stage gitlink",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--no-gpg-sign",
                "--no-verify",
                "-m",
                "add gitlink",
            ],
            repo,
        ),
        "commit gitlink",
    );
}

fn restore_index_and_worktree(repo: &Path, source: &str) -> std::process::Output {
    run_libra_command(
        &["restore", "--source", source, "--staged", "--worktree", "."],
        repo,
    )
}

#[test]
fn test_restore_removes_empty_materialized_gitlink_directory() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_gitlink(p);
    assert_cli_success(
        &run_libra_command(&["update-index", "--remove", "vendor/sub"], p),
        "remove gitlink from index",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--no-gpg-sign",
                "--no-verify",
                "-m",
                "remove gitlink",
            ],
            p,
        ),
        "commit gitlink removal",
    );

    assert_cli_success(
        &restore_index_and_worktree(p, "HEAD~1"),
        "restore gitlink commit",
    );
    assert!(p.join("vendor/sub").is_dir());

    let removed = restore_index_and_worktree(p, "HEAD");
    assert_cli_success(&removed, "restore commit without gitlink");
    assert!(
        !p.join("vendor/sub").exists(),
        "an empty materialized gitlink directory must be removed"
    );
}

#[test]
fn test_restore_replaces_empty_materialized_gitlink_with_blob() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_gitlink(p);

    std::fs::write(p.join("replacement-source.txt"), "replacement\n")
        .expect("write replacement source");
    let hashed = run_libra_command(&["hash-object", "-w", "replacement-source.txt"], p);
    assert_cli_success(&hashed, "hash replacement blob");
    let oid = String::from_utf8_lossy(&hashed.stdout).trim().to_string();
    let spec = format!("100644,{oid},vendor/sub");
    assert_cli_success(
        &run_libra_command(&["update-index", "--cacheinfo", &spec], p),
        "replace gitlink index entry with blob",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--no-gpg-sign",
                "--no-verify",
                "-m",
                "replace gitlink",
            ],
            p,
        ),
        "commit gitlink replacement",
    );

    assert_cli_success(
        &restore_index_and_worktree(p, "HEAD~1"),
        "restore gitlink commit",
    );
    assert!(p.join("vendor/sub").is_dir());

    let replaced = restore_index_and_worktree(p, "HEAD");
    assert_cli_success(&replaced, "restore blob replacing gitlink");
    assert_eq!(
        std::fs::read_to_string(p.join("vendor/sub")).expect("read replacement blob"),
        "replacement\n"
    );
}

#[test]
fn test_restore_refuses_nonempty_gitlink_before_mutating_other_paths() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_gitlink(p);
    std::fs::write(p.join("tracked.txt"), "next\n").expect("update tracked file");
    assert_cli_success(
        &run_libra_command(&["update-index", "--add", "tracked.txt"], p),
        "stage tracked update",
    );
    assert_cli_success(
        &run_libra_command(&["update-index", "--remove", "vendor/sub"], p),
        "remove gitlink from index",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "commit",
                "--no-gpg-sign",
                "--no-verify",
                "-m",
                "remove gitlink and update tracked file",
            ],
            p,
        ),
        "commit transition away from gitlink",
    );

    assert_cli_success(
        &restore_index_and_worktree(p, "HEAD~1"),
        "restore gitlink commit",
    );
    std::fs::write(p.join("vendor/sub/local.txt"), "nested user data\n")
        .expect("write nested gitlink content");
    assert_eq!(
        std::fs::read_to_string(p.join("tracked.txt")).expect("read old tracked content"),
        "tracked\n"
    );

    let rejected = restore_index_and_worktree(p, "HEAD");
    assert_eq!(rejected.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(
        stderr.contains("refusing to replace non-empty worktree directory 'vendor/sub'"),
        "{stderr}"
    );
    assert!(p.join("vendor/sub/local.txt").exists());
    assert_eq!(
        std::fs::read_to_string(p.join("tracked.txt")).expect("read untouched tracked content"),
        "tracked\n",
        "preflight must reject before restoring an earlier path"
    );
}

#[test]
fn test_restore_overlay_no_overlay_toggle_last_wins() {
    // `--no-overlay --overlay` → overlay wins → keep.
    let repo = repo_with_extra_over_parent();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(
            &[
                "restore",
                "--no-overlay",
                "--overlay",
                "--source",
                "HEAD~1",
                ".",
            ],
            p,
        ),
        "toggle: overlay last",
    );
    assert!(
        p.join("extra.txt").exists(),
        "last --overlay wins → file kept",
    );

    // `--overlay --no-overlay` → no-overlay wins → delete.
    let repo = repo_with_extra_over_parent();
    let p = repo.path();
    assert_cli_success(
        &run_libra_command(
            &[
                "restore",
                "--overlay",
                "--no-overlay",
                "--source",
                "HEAD~1",
                ".",
            ],
            p,
        ),
        "toggle: no-overlay last",
    );
    assert!(
        !p.join("extra.txt").exists(),
        "last --no-overlay wins → file removed",
    );
}

#[test]
fn test_restore_staged_overlay_keeps_index_entry_absent_from_source() {
    // Index overlay: `--staged --overlay` must not unstage/remove an index entry
    // that is absent from the source.
    let repo = repo_with_extra_over_parent();
    let p = repo.path();
    let out = run_libra_command(
        &[
            "restore",
            "--staged",
            "--overlay",
            "--source",
            "HEAD~1",
            ".",
        ],
        p,
    );
    assert_cli_success(&out, "restore --staged --overlay");
    // extra.txt is still tracked in the index (its blob is unchanged on disk).
    let status = run_libra_command(&["status", "--short"], p);
    let s = String::from_utf8_lossy(&status.stdout);
    assert!(
        !s.contains("D  extra.txt") && !s.contains("D extra.txt"),
        "overlay --staged must not stage a deletion of extra.txt; status: {s}"
    );
}

#[test]
fn test_restore_overlay_recreates_source_path_missing_from_worktree() {
    // Overlay must still CREATE a source path that is missing from the worktree;
    // it only suppresses removal of paths absent from the source.
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    std::fs::remove_file(p.join("tracked.txt")).expect("delete tracked.txt from worktree");
    assert!(!p.join("tracked.txt").exists());
    let out = run_libra_command(&["restore", "--overlay", "tracked.txt"], p);
    assert_cli_success(&out, "restore --overlay recreate");
    assert!(
        p.join("tracked.txt").exists(),
        "overlay must recreate a source path missing from the worktree",
    );
}

#[test]
fn test_restore_staged_overlay_adds_source_path_missing_from_index() {
    // Overlay --staged must still ADD a source path that is missing from the
    // index (it only suppresses removal of index entries absent from source).
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    commit_file_cli(p, "extra.txt", "extra\n", "add extra"); // HEAD has extra.txt
    // Drop extra.txt from the index (keeps the worktree file).
    assert_cli_success(
        &run_libra_command(&["rm", "--cached", "extra.txt"], p),
        "rm --cached extra.txt",
    );
    let listed = String::from_utf8_lossy(&run_libra_command(&["ls-files"], p).stdout).to_string();
    assert!(
        !listed.contains("extra.txt"),
        "precondition: extra.txt should be untracked after rm --cached; ls-files: {listed}"
    );
    // Overlay staged restore from HEAD must re-add extra.txt to the index.
    let out = run_libra_command(
        &[
            "restore",
            "--staged",
            "--overlay",
            "--source",
            "HEAD",
            "extra.txt",
        ],
        p,
    );
    assert_cli_success(&out, "restore --staged --overlay add");
    let listed = String::from_utf8_lossy(&run_libra_command(&["ls-files"], p).stdout).to_string();
    assert!(
        listed.contains("extra.txt"),
        "overlay --staged must add a source path missing from the index; ls-files: {listed}"
    );
}

#[test]
fn test_restore_overlay_recreates_deleted_tracked_directory() {
    // A directory pathspec whose tracked files were all deleted from the
    // worktree must be recreated (the pathspec expands to its source files via
    // the discovery set, not a bare directory entry). Covers both overlay and
    // default modes.
    for overlay in [true, false] {
        let repo = create_committed_repo_via_cli();
        let p = repo.path();
        std::fs::create_dir(p.join("dir")).unwrap();
        std::fs::write(p.join("dir/a.txt"), "a\n").unwrap();
        std::fs::write(p.join("dir/b.txt"), "b\n").unwrap();
        assert_cli_success(&run_libra_command(&["add", "dir"], p), "add dir");
        assert_cli_success(
            &run_libra_command(&["commit", "-m", "add dir", "--no-verify"], p),
            "commit dir",
        );
        // Delete the whole tracked directory from the worktree.
        std::fs::remove_dir_all(p.join("dir")).unwrap();
        assert!(!p.join("dir").exists());

        let mut argv = vec!["restore"];
        if overlay {
            argv.push("--overlay");
        }
        argv.push("dir");
        let out = run_libra_command(&argv, p);
        assert_cli_success(&out, "restore deleted directory");
        assert_eq!(
            std::fs::read_to_string(p.join("dir/a.txt")).unwrap_or_default(),
            "a\n",
            "dir/a.txt must be recreated (overlay={overlay})",
        );
        assert_eq!(
            std::fs::read_to_string(p.join("dir/b.txt")).unwrap_or_default(),
            "b\n",
            "dir/b.txt must be recreated (overlay={overlay})",
        );
    }
}

#[test]
fn test_restore_merge_rewrites_conflict_markers() {
    // `restore --merge` rebuilds the conflict markers in the working tree from
    // the index stages (ours = stage 2, theirs = stage 3), leaving the index
    // unmerged. `--conflict=diff3` also emits the base block; an unsupported
    // style is a usage error.
    let repo = create_conflicted_repo();
    let p = repo.path();

    // Overwrite the worktree so --merge has something to regenerate.
    std::fs::write(p.join("tracked.txt"), "hand-edited\n").unwrap();

    let out = run_libra_command(&["restore", "--merge", "tracked.txt"], p);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let content = std::fs::read_to_string(p.join("tracked.txt")).unwrap();
    assert!(
        content.contains("<<<<<<< ours")
            && content.contains("=======")
            && content.contains(">>>>>>> theirs"),
        "merge markers must be present:\n{content}"
    );
    assert!(
        content.contains("main change") && content.contains("feature change"),
        "both conflict sides must be present:\n{content}"
    );
    assert!(
        !content.contains("||||||| base"),
        "default merge style omits the base block:\n{content}"
    );

    // --conflict=diff3 includes the base block (and implies --merge). The index
    // is still unmerged after --merge, so this regenerates from the same stages.
    let out3 = run_libra_command(&["restore", "--conflict=diff3", "tracked.txt"], p);
    assert!(
        out3.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out3.stderr)
    );
    let content3 = std::fs::read_to_string(p.join("tracked.txt")).unwrap();
    assert!(
        content3.contains("||||||| base"),
        "diff3 style must include the base block:\n{content3}"
    );

    // Unsupported style → usage error (exit 129) carrying the stable CLI code.
    let bad = run_libra_command(&["restore", "--conflict=zdiff3", "tracked.txt"], p);
    assert_eq!(bad.status.code(), Some(129), "zdiff3 is unsupported");
    let (human, report) = parse_cli_error_stderr(&bad.stderr);
    assert_eq!(
        report.error_code, "LBR-CLI-002",
        "unsupported conflict style should carry the CLI-arguments stable code"
    );
    assert!(
        human.contains("unsupported conflict style"),
        "stderr should name the unsupported style: {human}"
    );
}

#[test]
fn test_restore_merge_replaces_empty_worktree_directory() {
    let repo = create_conflicted_repo();
    let path = repo.path().join("tracked.txt");
    std::fs::remove_file(&path).expect("remove conflict marker file");
    std::fs::create_dir(&path).expect("create empty materialized directory");

    let output = run_libra_command(&["restore", "--merge", "tracked.txt"], repo.path());
    assert_cli_success(&output, "restore --merge over empty directory");
    let content = std::fs::read_to_string(path).expect("read rebuilt markers");
    assert!(content.contains("<<<<<<< ours"), "{content}");
    assert!(content.contains(">>>>>>> theirs"), "{content}");
}

#[test]
fn test_restore_diff3_rejects_nonempty_directory_before_mutating_any_path() {
    let repo = create_two_path_conflicted_repo();
    let first = repo.path().join("a-first.txt");
    let blocked = repo.path().join("z-blocked.txt");
    std::fs::write(&first, "must remain untouched\n").expect("write first sentinel");
    std::fs::remove_file(&blocked).expect("remove second conflict marker file");
    std::fs::create_dir(&blocked).expect("create blocked directory");
    std::fs::write(blocked.join("nested.txt"), "nested user data\n")
        .expect("write nested user data");

    let output = run_libra_command(
        &[
            "restore",
            "--conflict=diff3",
            "a-first.txt",
            "z-blocked.txt",
        ],
        repo.path(),
    );
    assert_eq!(output.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("refusing to replace non-empty worktree directory 'z-blocked.txt'"),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(first).expect("read untouched first path"),
        "must remain untouched\n"
    );
    assert_eq!(
        std::fs::read_to_string(blocked.join("nested.txt")).expect("read nested data"),
        "nested user data\n"
    );
}
