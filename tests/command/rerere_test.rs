//! Integration tests for `libra rerere`.
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network).

use std::{fs, process::Output};

use tempfile::{TempDir, tempdir};

use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

const CONFLICT: &str = "line1\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> other\nline3\n";
const RESOLVED: &str = "line1\nRESOLVED\nline3\n";

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// A committed repo with `tracked.txt` overwritten in the working tree with a
/// conflict (the file stays tracked, which is what `rerere` keys on).
fn repo_with_conflict() -> TempDir {
    let repo = create_committed_repo_via_cli();
    fs::write(repo.path().join("tracked.txt"), CONFLICT).unwrap();
    repo
}

#[test]
fn rerere_records_resolves_and_replays() {
    let repo = repo_with_conflict();
    let file = repo.path().join("tracked.txt");

    // 1. Record the preimage.
    assert_cli_success(&run_libra_command(&["rerere"], repo.path()), "record");
    let status = run_libra_command(&["rerere", "status"], repo.path());
    assert!(
        out(&status).contains("tracked.txt"),
        "status should list the tracked conflict: {}",
        out(&status)
    );

    // 2. Resolve it and let rerere record the postimage.
    fs::write(&file, RESOLVED).unwrap();
    assert_cli_success(
        &run_libra_command(&["rerere"], repo.path()),
        "record resolution",
    );

    // 3. The same conflict reappears; rerere must replay the resolution.
    fs::write(&file, CONFLICT).unwrap();
    assert_cli_success(&run_libra_command(&["rerere"], repo.path()), "replay");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        RESOLVED,
        "rerere should have replayed the recorded resolution"
    );
}

#[test]
fn rerere_forget_drops_the_recording() {
    let repo = repo_with_conflict();
    run_libra_command(&["rerere"], repo.path());
    let forget = run_libra_command(&["rerere", "forget", "tracked.txt"], repo.path());
    assert_eq!(forget.status.code(), Some(0));
    let status = run_libra_command(&["rerere", "status"], repo.path());
    assert!(
        !out(&status).contains("tracked.txt"),
        "forget should remove the tracked conflict: {}",
        out(&status)
    );
}

#[test]
fn rerere_forget_unknown_path_is_an_error() {
    let repo = repo_with_conflict();
    run_libra_command(&["rerere"], repo.path());
    let forget = run_libra_command(&["rerere", "forget", "nope.txt"], repo.path());
    assert_eq!(forget.status.code(), Some(128));
}

#[test]
fn rerere_clear_stops_tracking() {
    let repo = repo_with_conflict();
    run_libra_command(&["rerere"], repo.path());
    let clear = run_libra_command(&["rerere", "clear"], repo.path());
    assert_eq!(clear.status.code(), Some(0));
    let status = run_libra_command(&["rerere", "status"], repo.path());
    assert!(out(&status).trim().is_empty(), "clear should empty status");
}

#[test]
fn rerere_diff_shows_changes_since_preimage() {
    let repo = repo_with_conflict();
    run_libra_command(&["rerere"], repo.path());
    // Edit the conflicted file, then diff against the recorded preimage.
    fs::write(repo.path().join("tracked.txt"), RESOLVED).unwrap();
    let diff = run_libra_command(&["rerere", "diff"], repo.path());
    assert_eq!(diff.status.code(), Some(0));
    assert!(
        out(&diff).contains("RESOLVED") || out(&diff).contains("tracked.txt"),
        "diff should show the change: {}",
        out(&diff)
    );
}

#[test]
fn rerere_gc_is_a_noop_for_fresh_entries() {
    let repo = repo_with_conflict();
    run_libra_command(&["rerere"], repo.path());
    let gc = run_libra_command(&["rerere", "gc"], repo.path());
    assert_eq!(gc.status.code(), Some(0));
    // The fresh (unresolved) entry is well under the TTL, so it survives.
    let status = run_libra_command(&["rerere", "status"], repo.path());
    assert!(out(&status).contains("tracked.txt"));
}

#[test]
fn rerere_outside_repository_is_an_error() {
    let dir = tempdir().unwrap();
    let out = run_libra_command(&["rerere", "status"], dir.path());
    assert_eq!(out.status.code(), Some(128));
}

// ---------------------------------------------------------------------------
// GGT-12 Phase B: automatic merge/rebase/cherry-pick integration.
// ---------------------------------------------------------------------------

fn rev_parse(repo: &std::path::Path, refname: &str) -> String {
    let output = run_libra_command(&["rev-parse", refname], repo);
    assert_cli_success(&output, "rev-parse");
    out(&output).trim().to_string()
}

/// Build a repo whose `feature` branch conflicts with `main` on the single line
/// of `f.txt`, and return (repo, feature-ref, mainline-commit-hash).
fn repo_with_cherry_pick_conflict() -> TempDir {
    let repo = create_committed_repo_via_cli();
    let path = repo.path();

    fs::write(path.join("f.txt"), "base\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], path), "add base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base f", "--no-verify"], path),
        "commit base",
    );

    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], path),
        "branch",
    );
    fs::write(path.join("f.txt"), "feature\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], path), "add feature");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "feature f", "--no-verify"], path),
        "commit feature",
    );

    assert_cli_success(&run_libra_command(&["switch", "main"], path), "switch main");
    fs::write(path.join("f.txt"), "mainline\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], path), "add main");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "mainline f", "--no-verify"], path),
        "commit main",
    );

    repo
}

#[test]
fn rerere_auto_replays_a_recurring_cherry_pick_conflict() {
    let repo = repo_with_cherry_pick_conflict();
    let path = repo.path();
    let mainline = rev_parse(path, "HEAD");

    // Opt in — without this the hooks are inert (see the gate test below).
    assert_cli_success(
        &run_libra_command(&["config", "rerere.enabled", "true"], path),
        "enable rerere",
    );

    // First cherry-pick conflicts; the sequencer's rerere hook must AUTO-record
    // the preimage with no manual `libra rerere`.
    let first = run_libra_command(&["cherry-pick", "feature"], path);
    assert!(!first.status.success(), "first cherry-pick should conflict");
    let status = run_libra_command(&["rerere", "status"], path);
    assert!(
        out(&status).contains("f.txt"),
        "the conflict should have been auto-recorded: {}",
        out(&status)
    );

    // Resolve, stage, and continue; the --continue hook records the postimage.
    fs::write(path.join("f.txt"), "reconciled\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "f.txt"], path),
        "stage resolution",
    );
    assert_cli_success(
        &run_libra_command(&["cherry-pick", "--continue"], path),
        "cherry-pick --continue",
    );

    // Rewind main and replay the SAME cherry-pick: the identical conflict must be
    // auto-resolved from the recorded resolution instead of left with markers.
    assert_cli_success(
        &run_libra_command(&["reset", "--hard", &mainline], path),
        "reset main",
    );
    let second = run_libra_command(&["cherry-pick", "feature"], path);
    assert!(
        !second.status.success(),
        "second cherry-pick still stops on conflict"
    );
    let replayed = fs::read_to_string(path.join("f.txt")).unwrap();
    assert_eq!(
        replayed, "reconciled\n",
        "rerere should have replayed the recorded resolution, got: {replayed:?}"
    );
    assert!(
        !replayed.contains("<<<<<<<"),
        "the replayed file must not still carry conflict markers"
    );
}

#[test]
fn rerere_disabled_by_default_does_not_auto_record() {
    let repo = repo_with_cherry_pick_conflict();
    let path = repo.path();

    // No `rerere.enabled` set → the sequencer hooks must be complete no-ops.
    let first = run_libra_command(&["cherry-pick", "feature"], path);
    assert!(!first.status.success(), "cherry-pick should still conflict");
    let status = run_libra_command(&["rerere", "status"], path);
    assert!(
        out(&status).trim().is_empty(),
        "with rerere disabled nothing should be auto-recorded: {}",
        out(&status)
    );
}

#[test]
fn rerere_auto_replays_a_recurring_merge_conflict() {
    // Same divergence, exercised through `merge` + `merge --continue` (whose
    // resolution is finalized without going through `commit`, so it carries its
    // own postimage hook).
    let repo = repo_with_cherry_pick_conflict();
    let path = repo.path();
    let mainline = rev_parse(path, "HEAD");
    assert_cli_success(
        &run_libra_command(&["config", "rerere.enabled", "true"], path),
        "enable rerere",
    );

    let first = run_libra_command(&["merge", "feature"], path);
    assert!(!first.status.success(), "first merge should conflict");

    fs::write(path.join("f.txt"), "merged-by-hand\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], path), "stage merge");
    assert_cli_success(
        &run_libra_command(&["merge", "--continue"], path),
        "merge --continue",
    );

    // Replay the identical merge conflict after rewinding.
    assert_cli_success(
        &run_libra_command(&["reset", "--hard", &mainline], path),
        "reset main",
    );
    let second = run_libra_command(&["merge", "feature"], path);
    assert!(
        !second.status.success(),
        "second merge still stops on conflict"
    );
    let replayed = fs::read_to_string(path.join("f.txt")).unwrap();
    assert_eq!(
        replayed, "merged-by-hand\n",
        "rerere should have replayed the recorded merge resolution, got: {replayed:?}"
    );
}

/// W2 §C.4.3: MERGE_RR is per-worktree — each worktree tracks its own
/// current conflicts in `<local_gitdir>/MERGE_RR`, one scope's clear never
/// drops another's tracking, while the resolution CACHE stays shared (a
/// resolution recorded in one worktree replays in another).
#[test]
fn merge_rr_is_worktree_scoped_and_cache_is_shared() {
    let repo = repo_with_conflict();
    let main = repo.path();
    let wt_root = tempdir().expect("wt root");
    let wt = wt_root.path().join("rr-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );

    // Main records its conflict; the canonical file is main's local gitdir
    // (`.libra/MERGE_RR`), NOT the legacy shared cache location.
    assert_cli_success(&run_libra_command(&["rerere"], main), "main record");
    assert!(main.join(".libra/MERGE_RR").exists(), "canonical main file");
    assert!(
        !main.join(".libra/rerere/MERGE_RR").exists(),
        "no legacy shared MERGE_RR is written"
    );

    // The linked worktree sees NO tracked conflicts (its own list is empty).
    let wt_status = run_libra_command(&["rerere", "status"], &wt);
    assert_cli_success(&wt_status, "wt status");
    assert_eq!(out(&wt_status).trim(), "", "linked list starts empty");

    // The linked worktree gets the same conflict; tracking lands in ITS
    // local gitdir and clear-in-main must not drop it.
    fs::write(wt.join("tracked.txt"), CONFLICT).unwrap();
    assert_cli_success(&run_libra_command(&["rerere"], &wt), "wt record");
    assert!(wt.join(".libra/MERGE_RR").exists(), "canonical wt file");
    assert_cli_success(&run_libra_command(&["rerere", "clear"], main), "main clear");
    let wt_status = run_libra_command(&["rerere", "status"], &wt);
    assert!(
        out(&wt_status).contains("tracked.txt"),
        "main's clear does not drop the linked worktree's tracking: {}",
        out(&wt_status)
    );

    // Shared cache: resolve in the LINKED worktree, then the same conflict
    // in MAIN replays from the shared resolution cache.
    fs::write(wt.join("tracked.txt"), RESOLVED).unwrap();
    assert_cli_success(&run_libra_command(&["rerere"], &wt), "wt resolve");
    fs::write(main.join("tracked.txt"), CONFLICT).unwrap();
    assert_cli_success(&run_libra_command(&["rerere"], main), "main replay");
    assert_eq!(
        fs::read_to_string(main.join("tracked.txt")).unwrap(),
        RESOLVED,
        "the resolution cache is repository-shared across worktrees"
    );
}

/// W2 §C.4.3 legacy migration: a single-worktree main READS a pre-W2
/// `.libra/rerere/MERGE_RR` and migrates it to the canonical location on
/// first write; with linked worktrees present the legacy file is ambiguous —
/// ignored with a notice and left untouched, and a linked scope never reads
/// it at all.
#[test]
fn legacy_merge_rr_migrates_for_single_main_and_stays_ambiguous_with_linked() {
    // Single-worktree: legacy content is read and migrated on first write.
    let repo = repo_with_conflict();
    let main = repo.path();
    assert_cli_success(&run_libra_command(&["rerere"], main), "record");
    let canonical = main.join(".libra/MERGE_RR");
    let legacy = main.join(".libra/rerere/MERGE_RR");
    // Recreate the pre-W2 layout: move the tracking file to the legacy spot.
    fs::rename(&canonical, &legacy).unwrap();
    let status = run_libra_command(&["rerere", "status"], main);
    assert!(
        out(&status).contains("tracked.txt"),
        "single-worktree main reads the legacy file: {}",
        out(&status)
    );
    // A write pass migrates: canonical exists, legacy is gone.
    assert_cli_success(&run_libra_command(&["rerere"], main), "migrating write");
    assert!(canonical.exists(), "canonical file after migration");
    assert!(!legacy.exists(), "legacy file removed after migration");

    // With a linked worktree, a legacy file is ambiguous: ignored (with a
    // notice) and left untouched; the linked scope never reads it either.
    fs::rename(&canonical, &legacy).unwrap();
    let legacy_bytes = fs::read(&legacy).unwrap();
    let wt_root = tempdir().expect("wt root");
    let wt = wt_root.path().join("legacy-wt");
    assert_cli_success(
        &run_libra_command(&["worktree", "add", wt.to_str().unwrap()], main),
        "worktree add",
    );
    let status = run_libra_command(&["rerere", "status"], main);
    assert_cli_success(&status, "ambiguous status still succeeds");
    assert_eq!(
        out(&status).trim(),
        "",
        "ambiguous legacy list is not consumed by main"
    );
    assert!(
        String::from_utf8_lossy(&status.stderr).contains("ambiguous"),
        "a one-line notice points at the ambiguity"
    );
    // `clear` in main surfaces the same notice and leaves the file too.
    let cleared = run_libra_command(&["rerere", "clear"], main);
    assert_cli_success(&cleared, "ambiguous clear still succeeds");
    assert!(
        String::from_utf8_lossy(&cleared.stderr).contains("ambiguous"),
        "clear surfaces the ambiguity notice"
    );
    assert!(legacy.exists(), "clear leaves the ambiguous legacy file");
    assert_eq!(
        fs::read(&legacy).unwrap(),
        legacy_bytes,
        "the ambiguous legacy file is byte-for-byte untouched for the W3 doctor"
    );
    let wt_status = run_libra_command(&["rerere", "status"], &wt);
    assert_cli_success(&wt_status, "wt status");
    assert_eq!(
        out(&wt_status).trim(),
        "",
        "a linked scope never reads the legacy common file"
    );
    assert!(
        legacy.exists(),
        "the ambiguous legacy file is left untouched"
    );
}
