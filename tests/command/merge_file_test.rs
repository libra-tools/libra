//! Integration tests for `libra merge-file`.
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network).

use std::{fs, path::Path, process::Output};

use tempfile::{TempDir, tempdir};

use super::{
    assert_cli_success, create_committed_repo_via_cli, parse_json_stdout, run_libra_command,
};

fn stdout_str(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Write the three input files into `dir` and return their fixed names.
fn write_inputs(dir: &Path, ours: &str, base: &str, theirs: &str) {
    fs::write(dir.join("ours.txt"), ours).unwrap();
    fs::write(dir.join("base.txt"), base).unwrap();
    fs::write(dir.join("theirs.txt"), theirs).unwrap();
}

fn merge_file(dir: &Path, args: &[&str]) -> Output {
    let mut full = vec!["merge-file"];
    full.extend_from_slice(args);
    full.extend_from_slice(&["ours.txt", "base.txt", "theirs.txt"]);
    run_libra_command(&full, dir)
}

#[test]
fn clean_merge_combines_non_overlapping_changes() {
    let dir = tempdir().unwrap();
    write_inputs(dir.path(), "X\nb\nc\n", "a\nb\nc\n", "a\nb\nZ\n");
    let out = merge_file(dir.path(), &["-p"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "clean merge should exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout_str(&out), "X\nb\nZ\n");
}

#[test]
fn conflict_emits_markers_and_exits_1() {
    let dir = tempdir().unwrap();
    write_inputs(dir.path(), "a\nOURS\nc\n", "a\nb\nc\n", "a\nTHEIRS\nc\n");
    let out = merge_file(dir.path(), &["-p"]);
    assert_eq!(out.status.code(), Some(1), "conflict should exit 1");
    let merged = stdout_str(&out);
    assert!(
        merged.contains("<<<<<<< ours"),
        "missing ours marker: {merged}"
    );
    assert!(merged.contains("======="), "missing separator: {merged}");
    assert!(
        merged.contains(">>>>>>> theirs"),
        "missing theirs marker: {merged}"
    );
}

#[test]
fn diff3_includes_base_section() {
    let dir = tempdir().unwrap();
    write_inputs(dir.path(), "a\nOURS\nc\n", "a\nBASE\nc\n", "a\nTHEIRS\nc\n");
    let out = merge_file(dir.path(), &["-p", "--diff3"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stdout_str(&out).contains("|||||||"),
        "diff3 should include the base marker: {}",
        stdout_str(&out)
    );
}

#[test]
fn stdout_mode_does_not_modify_current() {
    let dir = tempdir().unwrap();
    write_inputs(dir.path(), "X\nb\nc\n", "a\nb\nc\n", "a\nb\nZ\n");
    merge_file(dir.path(), &["-p"]);
    assert_eq!(
        fs::read_to_string(dir.path().join("ours.txt")).unwrap(),
        "X\nb\nc\n",
        "-p must not modify the current file"
    );
}

#[test]
fn write_mode_overwrites_current() {
    let dir = tempdir().unwrap();
    write_inputs(dir.path(), "X\nb\nc\n", "a\nb\nc\n", "a\nb\nZ\n");
    let out = merge_file(dir.path(), &[]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(dir.path().join("ours.txt")).unwrap(),
        "X\nb\nZ\n",
        "write mode should overwrite the current file with the merge"
    );
}

fn init_repo() -> TempDir {
    let repo = tempdir().unwrap();
    assert!(run_libra_command(&["init"], repo.path()).status.success());
    repo
}

#[test]
fn write_mode_clean_leaves_no_backup() {
    let repo = init_repo();
    write_inputs(repo.path(), "X\nb\nc\n", "a\nb\nc\n", "a\nb\nZ\n");
    let out = merge_file(repo.path(), &[]);
    assert_eq!(out.status.code(), Some(0));
    let backup_dir = repo.path().join(".libra/merge-file-backup");
    let empty = !backup_dir.exists()
        || fs::read_dir(&backup_dir)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true);
    assert!(empty, "a clean merge should leave no backup");
}

#[test]
fn write_mode_conflict_keeps_backup() {
    let repo = init_repo();
    let original = "a\nOURS\nc\n";
    write_inputs(repo.path(), original, "a\nb\nc\n", "a\nTHEIRS\nc\n");
    let out = merge_file(repo.path(), &[]);
    assert_eq!(out.status.code(), Some(1));
    let backup = repo.path().join(".libra/merge-file-backup/ours.txt");
    assert!(backup.exists(), "conflict should keep the backup");
    assert_eq!(
        fs::read_to_string(&backup).unwrap(),
        original,
        "backup should hold the original current content"
    );
}

#[test]
fn binary_input_is_rejected() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("ours.txt"), b"a\0b").unwrap();
    fs::write(dir.path().join("base.txt"), "a\nb\n").unwrap();
    fs::write(dir.path().join("theirs.txt"), "a\nc\n").unwrap();
    let out = merge_file(dir.path(), &["-p"]);
    assert_eq!(
        out.status.code(),
        Some(128),
        "binary input must be rejected"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot merge binary files"),
        "stderr should explain the binary rejection: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn empty_inputs_are_allowed() {
    let dir = tempdir().unwrap();
    write_inputs(dir.path(), "", "", "");
    let out = merge_file(dir.path(), &["-p"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout_str(&out), "");
}

#[test]
fn missing_input_file_is_an_error() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("base.txt"), "a\n").unwrap();
    fs::write(dir.path().join("theirs.txt"), "b\n").unwrap();
    // ours.txt does not exist.
    let out = run_libra_command(
        &["merge-file", "-p", "ours.txt", "base.txt", "theirs.txt"],
        dir.path(),
    );
    assert_eq!(out.status.code(), Some(128));
}

#[test]
fn json_reports_conflict_and_written() {
    let dir = tempdir().unwrap();
    write_inputs(dir.path(), "a\nOURS\nc\n", "a\nb\nc\n", "a\nTHEIRS\nc\n");
    let out = run_libra_command(
        &[
            "--json",
            "merge-file",
            "-p",
            "ours.txt",
            "base.txt",
            "theirs.txt",
        ],
        dir.path(),
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "conflict exits 1 even with --json"
    );
    let json = parse_json_stdout(&out);
    assert_eq!(json["data"]["conflict"].as_bool(), Some(true));
    assert_eq!(json["data"]["written"].as_bool(), Some(false));
}

/// W2 §C.4.3: the in-place backup lives in the ACTING worktree's local
/// gitdir — a linked worktree's conflicted merge backs up under its own
/// `.libra/merge-file-backup/`, and same-named files in two worktrees never
/// share (or clean up) each other's backups.
#[test]
fn backup_is_worktree_local() {
    let repo = create_committed_repo_via_cli();
    let main = repo.path();
    let wt_root = tempfile::tempdir().expect("wt root");
    let wt = wt_root.path().join("mf-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Identical conflicting three-way inputs in BOTH worktrees.
    for dir in [main, wt.as_path()] {
        std::fs::write(dir.join("base.txt"), "base\n").unwrap();
        std::fs::write(dir.join("ours.txt"), "ours\n").unwrap();
        std::fs::write(dir.join("theirs.txt"), "theirs\n").unwrap();
    }
    let wt_merge = run_libra_command(&["merge-file", "ours.txt", "base.txt", "theirs.txt"], &wt);
    assert_eq!(wt_merge.status.code(), Some(1), "conflict exits 1");
    assert!(
        wt.join(".libra/merge-file-backup/ours.txt").exists(),
        "the linked worktree's backup lives in ITS local gitdir"
    );
    assert!(
        !main.join(".libra/merge-file-backup/ours.txt").exists(),
        "nothing leaked into the shared/common storage"
    );

    // Main's own conflicted merge lands in main's gitdir; both coexist.
    let main_merge = run_libra_command(&["merge-file", "ours.txt", "base.txt", "theirs.txt"], main);
    assert_eq!(main_merge.status.code(), Some(1), "conflict exits 1");
    assert!(main.join(".libra/merge-file-backup/ours.txt").exists());
    assert!(
        wt.join(".libra/merge-file-backup/ours.txt").exists(),
        "main's merge did not clean up the linked worktree's backup"
    );

    // A CLEAN merge in main removes MAIN's own backup (the cleanup branch)
    // while the linked worktree's backup for the same filename survives.
    std::fs::write(main.join("base.txt"), "b\n").unwrap();
    std::fs::write(main.join("ours.txt"), "x\n").unwrap();
    std::fs::write(main.join("theirs.txt"), "b\n").unwrap();
    let clean_merge =
        run_libra_command(&["merge-file", "ours.txt", "base.txt", "theirs.txt"], main);
    assert_eq!(clean_merge.status.code(), Some(0), "clean merge exits 0");
    assert!(
        !main.join(".libra/merge-file-backup/ours.txt").exists(),
        "main's clean merge removed its OWN backup"
    );
    assert!(
        wt.join(".libra/merge-file-backup/ours.txt").exists(),
        "the linked worktree's same-named backup survives main's cleanup"
    );
}
