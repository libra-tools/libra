//! Integration tests for the diff plumbing trio: `diff-tree` / `diff-index` /
//! `diff-files`.
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network).

use std::{fs, process::Output};

use tempfile::TempDir;

use super::{assert_cli_success, create_committed_repo_via_cli, run_libra_command};

fn out_str(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn rev_parse(repo: &TempDir, rev: &str) -> String {
    String::from_utf8_lossy(&run_libra_command(&["rev-parse", rev], repo.path()).stdout)
        .trim()
        .to_string()
}

/// A repo with two commits that differ in `tracked.txt`; returns `(repo, c1, c2)`.
fn repo_with_two_commits() -> (TempDir, String, String) {
    let repo = create_committed_repo_via_cli();
    let c1 = rev_parse(&repo, "HEAD");
    fs::write(repo.path().join("tracked.txt"), "second-content\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], repo.path()),
        "stage second change",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "second", "--no-verify"], repo.path()),
        "second commit",
    );
    let c2 = rev_parse(&repo, "HEAD");
    (repo, c1, c2)
}

fn configure_invalid_diff_renames(repo: &TempDir) {
    assert_cli_success(
        &run_libra_command(&["config", "diff.renames", "sideways"], repo.path()),
        "set invalid porcelain-only diff.renames",
    );
}

#[test]
fn diff_plumbing_tree_diffs_two_trees() {
    let (repo, c1, c2) = repo_with_two_commits();
    let out = run_libra_command(&["diff-tree", &c1, &c2], repo.path());
    // Plumbing diff exits 0 by default even with differences (ADR-PL-09).
    assert_eq!(
        out.status.code(),
        Some(0),
        "diff-tree should exit 0 on differences: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out_str(&out).contains("tracked.txt"),
        "diff-tree should show the changed file: {}",
        out_str(&out)
    );
}

#[test]
fn diff_plumbing_tree_ignores_porcelain_rename_config() {
    let (repo, c1, c2) = repo_with_two_commits();
    configure_invalid_diff_renames(&repo);
    let out = run_libra_command(&["diff-tree", &c1, &c2], repo.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out_str(&out).contains("tracked.txt"));
}

#[test]
fn diff_plumbing_files_shows_unstaged_changes() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    // Modify the working tree without staging.
    fs::write(repo.path().join("tracked.txt"), "worktree-edit\n").unwrap();
    let out = run_libra_command(&["diff-files"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "diff-files should exit 0 on differences: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out_str(&out).contains("tracked.txt"),
        "diff-files should show the unstaged change: {}",
        out_str(&out)
    );
}

#[test]
fn diff_plumbing_files_ignores_porcelain_rename_config() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    configure_invalid_diff_renames(&repo);
    fs::write(repo.path().join("tracked.txt"), "worktree-edit\n").unwrap();
    let out = run_libra_command(&["diff-files"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out_str(&out).contains("tracked.txt"));
}

#[test]
fn diff_plumbing_index_diffs_tree_against_worktree() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    fs::write(repo.path().join("tracked.txt"), "worktree-edit\n").unwrap();
    let out = run_libra_command(&["diff-index", "HEAD"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "diff-index should exit 0 on differences: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out_str(&out).contains("tracked.txt"),
        "diff-index should show the change vs the working tree: {}",
        out_str(&out)
    );
}

#[test]
fn diff_plumbing_index_ignores_porcelain_rename_config() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    configure_invalid_diff_renames(&repo);
    fs::write(repo.path().join("tracked.txt"), "worktree-edit\n").unwrap();
    let out = run_libra_command(&["diff-index", "HEAD"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out_str(&out).contains("tracked.txt"));
}

#[test]
fn diff_plumbing_index_cached_diffs_tree_against_index() {
    let (repo, _c1, _c2) = repo_with_two_commits();
    // Stage a change; the index then differs from HEAD.
    fs::write(repo.path().join("tracked.txt"), "staged-edit\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt"], repo.path()),
        "stage edit",
    );
    let out = run_libra_command(&["diff-index", "--cached", "HEAD"], repo.path());
    assert_eq!(
        out.status.code(),
        Some(0),
        "diff-index --cached exits 0 on differences: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out_str(&out).contains("tracked.txt"));
}

#[test]
fn diff_plumbing_tree_respects_pathspec() {
    let (repo, c1, c2) = repo_with_two_commits();
    // Limit to an unrelated path: no diff for it.
    let out = run_libra_command(&["diff-tree", &c1, &c2, "--", "nonexistent"], repo.path());
    assert_eq!(out.status.code(), Some(0));
    assert!(
        !out_str(&out).contains("tracked.txt"),
        "a path limiter should exclude tracked.txt: {}",
        out_str(&out)
    );
}

#[test]
fn diff_plumbing_outside_repository_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_libra_command(&["diff-files"], dir.path());
    assert_eq!(out.status.code(), Some(128));
}

/// WT-06 / issue #476 M-ITA-VIEW V4: `diff-files --name-status` reports an
/// intent-to-add entry as `A` (the index holds only the empty-blob placeholder).
#[test]
fn diff_files_reports_intent_to_add() {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    fs::write(root.join("new.txt"), "new\n").expect("write new");
    assert_cli_success(
        &run_libra_command(&["add", "-N", "new.txt"], root),
        "add -N",
    );

    let output = run_libra_command(&["diff-files", "--name-status"], root);
    assert_eq!(
        output.status.code(),
        Some(0),
        "plumbing differences exit 0 by default: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(out_str(&output).trim(), "A\tnew.txt");
}

/// DF-01 M-DF: `--exit-code` turns differences into exit 1 (0 when clean);
/// `--quiet` suppresses all output and also exits 1 on differences.
#[test]
fn diff_plumbing_exit_code_and_quiet_matrix() {
    let (repo, c1, c2) = repo_with_two_commits();

    // Clean tree: --exit-code and --quiet both exit 0.
    let clean = run_libra_command(&["diff-files", "--exit-code"], repo.path());
    assert_eq!(
        clean.status.code(),
        Some(0),
        "clean --exit-code should exit 0: {}",
        String::from_utf8_lossy(&clean.stderr)
    );
    let quiet_clean = run_libra_command(&["diff-files", "--quiet"], repo.path());
    assert_eq!(quiet_clean.status.code(), Some(0));
    assert!(out_str(&quiet_clean).is_empty(), "--quiet prints nothing");

    // Explicit `--raw` must be passed once. A duplicate makes clap reject the
    // synthetic argv.
    let raw_clean = run_libra_command(&["diff-files", "--raw"], repo.path());
    assert_eq!(
        raw_clean.status.code(),
        Some(0),
        "clean --raw: {}",
        String::from_utf8_lossy(&raw_clean.stderr)
    );
    let raw_tree = run_libra_command(&["diff-tree", "--raw", &c1, &c2], repo.path());
    assert_eq!(
        raw_tree.status.code(),
        Some(0),
        "diff-tree --raw: {}",
        String::from_utf8_lossy(&raw_tree.stderr)
    );
    assert!(
        out_str(&raw_tree).contains("tracked.txt"),
        "diff-tree --raw should print the raw record"
    );

    // Dirty tree: --exit-code exits 1, --quiet exits 1 with no output.
    fs::write(repo.path().join("tracked.txt"), "dirty-edit\n").unwrap();
    let dirty = run_libra_command(&["diff-files", "--exit-code"], repo.path());
    assert_eq!(
        dirty.status.code(),
        Some(1),
        "dirty --exit-code should exit 1: {}",
        String::from_utf8_lossy(&dirty.stderr)
    );
    let quiet_dirty = run_libra_command(&["diff-files", "--quiet"], repo.path());
    assert_eq!(quiet_dirty.status.code(), Some(1));
    assert!(out_str(&quiet_dirty).is_empty(), "--quiet prints nothing");
    let raw_dirty = run_libra_command(&["diff-files", "--raw"], repo.path());
    assert_eq!(raw_dirty.status.code(), Some(0), "dirty --raw stays exit 0");
    assert!(out_str(&raw_dirty).contains("tracked.txt"));
    let raw_index = run_libra_command(&["diff-index", "--raw", "HEAD"], repo.path());
    assert_eq!(
        raw_index.status.code(),
        Some(0),
        "diff-index --raw: {}",
        String::from_utf8_lossy(&raw_index.stderr)
    );

    // diff-tree / diff-index accept both flags too.
    let tree_ec = run_libra_command(&["diff-tree", "--exit-code", &c1, &c2], repo.path());
    assert_eq!(tree_ec.status.code(), Some(1));
    let index_q = run_libra_command(&["diff-index", "--quiet", "HEAD"], repo.path());
    assert_eq!(index_q.status.code(), Some(1));
    assert!(out_str(&index_q).is_empty());

    // `--json` still has to stay silent: plumbing `--quiet` is not the global
    // quiet flag, and `--machine` keeps emitting JSON.
    for args in [
        vec!["--json", "diff-files", "--quiet"],
        vec!["--json", "diff-index", "--quiet", "HEAD"],
        vec!["--json", "diff-tree", "--quiet", &c1, &c2],
    ] {
        let quiet_json = run_libra_command(&args, repo.path());
        assert_eq!(
            quiet_json.status.code(),
            Some(1),
            "quiet json {args:?}: {}",
            String::from_utf8_lossy(&quiet_json.stderr)
        );
        assert!(
            quiet_json.stdout.is_empty(),
            "--json --quiet must not print a diff envelope: {args:?} stdout={}",
            String::from_utf8_lossy(&quiet_json.stdout)
        );
    }

    let bad_flag = run_libra_command(&["diff-files", "--not-a-flag"], repo.path());
    assert_eq!(bad_flag.status.code(), Some(129));
    let bad_rev = run_libra_command(&["diff-index", "does-not-exist"], repo.path());
    assert_eq!(bad_rev.status.code(), Some(129));
    let bad_tree = run_libra_command(&["diff-tree", "does-not-exist", "HEAD"], repo.path());
    assert_eq!(bad_tree.status.code(), Some(129));
}

/// issues/479 TC-0044 — the stat-raciness contract behind `t/t3700-add.sh#24`:
/// `read-tree` leaves index entries with stat data the worktree cannot match,
/// and the plumbing `diff-index` (worktree comparison, default raw) must then
/// report the unchanged file as modified with an all-zero worktree sha —
/// WITHOUT hashing. `add --refresh` clears it again.
#[test]
fn diff_index_reports_read_tree_stat_raciness_with_zero_sha() {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();

    let read = run_libra_command(&["read-tree", "HEAD"], root);
    assert_cli_success(&read, "read-tree HEAD");

    let out = run_libra_command(&["diff-index", "HEAD", "--", "tracked.txt"], root);
    assert_eq!(
        out.status.code(),
        Some(0),
        "diff-index exits 0 by default: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let record = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        record.starts_with(":100644 100644 ") && record.ends_with(" M\ttracked.txt"),
        "raw record must report the stat-dirty entry as M: {record}"
    );
    assert!(
        record.contains(" 0000000 M"),
        "the worktree sha must be all zeros without hashing: {record}"
    );

    // `add --refresh` clears the racy entry; diff-index is then empty.
    let refresh = run_libra_command(&["add", "--refresh", "--", "tracked.txt"], root);
    assert_cli_success(&refresh, "add --refresh");
    let after = run_libra_command(&["diff-index", "HEAD", "--", "tracked.txt"], root);
    assert_eq!(
        after.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&after.stderr)
    );
    assert!(
        after.stdout.is_empty(),
        "the refreshed entry must produce no record: {}",
        String::from_utf8_lossy(&after.stdout)
    );
}

/// The same stat-based contract applies to plain `diff-files`: a stat-dirty
/// file with unchanged content is reported modified with a zero worktree sha.
#[test]
fn diff_files_reports_stat_dirty_entry_with_zero_sha() {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();

    // Identical content, bumped stat: only the stat differs from the index.
    fs::write(root.join("tracked.txt"), "tracked\n").expect("stat-dirty rewrite");

    let out = run_libra_command(&["diff-files", "--", "tracked.txt"], root);
    assert_eq!(out.status.code(), Some(0));
    let record = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        record.contains(" 0000000 M\ttracked.txt"),
        "stat-dirty entry must report M with all-zero worktree sha: {record}"
    );
}

// ---------------------------------------------------------------------------
// issues/479 DF-01 / TC-0044 (t3700-add.sh#24): the status letter follows ENTRY
// PRESENCE, not the emptiness of a side's content. An empty tracked file whose
// stat data was invalidated by `read-tree` is a MODIFICATION (`M` with an
// all-zero worktree sha) — Git never reports `A`/`D` for it, because the path
// exists in both the tree and the index.
// ---------------------------------------------------------------------------

/// Create `empty.txt` (a zero-byte blob) at HEAD, then invalidate the index
/// stat cache without touching the file.
fn repo_with_committed_empty_file() -> TempDir {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    fs::write(root.join("empty.txt"), "").expect("create empty file");
    assert_cli_success(
        &run_libra_command(&["add", "empty.txt"], root),
        "stage empty file",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "empty", "--no-verify"], root),
        "commit empty file",
    );
    assert_cli_success(
        &run_libra_command(&["read-tree", "HEAD"], root),
        "read-tree HEAD",
    );
    repo
}

#[test]
fn diff_index_read_tree_empty_blob_is_modified_not_added() {
    let repo = repo_with_committed_empty_file();
    let root = repo.path();

    let out = run_libra_command(&["diff-index", "HEAD", "--", "empty.txt"], root);
    assert_eq!(out.status.code(), Some(0));
    let record = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        record.starts_with(":100644 100644 e69de29"),
        "the empty blob stays the old-side object id: {record}"
    );
    assert!(
        record.ends_with(" M\tempty.txt"),
        "an empty tracked file is a modification, never an addition: {record}"
    );
    assert!(
        record.contains(" 0000000 M"),
        "the stat-invalid worktree sha is all zeros: {record}"
    );

    // `--name-status` must agree on the letter.
    let named = run_libra_command(
        &["diff-index", "HEAD", "--name-status", "--", "empty.txt"],
        root,
    );
    assert_eq!(
        String::from_utf8_lossy(&named.stdout).trim(),
        "M\tempty.txt",
        "name-status letter: {}",
        String::from_utf8_lossy(&named.stderr)
    );

    // `status --porcelain` must stay consistent: nothing changed on disk.
    let status = run_libra_command(&["status", "--porcelain"], root);
    assert!(
        status.stdout.is_empty(),
        "status must not claim a change: {}",
        String::from_utf8_lossy(&status.stdout)
    );

    // The pessimistic record is cleared by `add --refresh`, like Git.
    assert_cli_success(
        &run_libra_command(&["add", "--refresh", "--", "empty.txt"], root),
        "add --refresh",
    );
    let after = run_libra_command(&["diff-index", "HEAD", "--", "empty.txt"], root);
    assert!(
        after.stdout.is_empty(),
        "the refreshed empty entry must produce no record: {}",
        String::from_utf8_lossy(&after.stdout)
    );
}

/// Git emits NO `-p` body for a stat-invalid entry whose content is unchanged,
/// even though `--raw`/`--name-status` still report `M`. Printing a
/// `--- /dev/null` header here would contradict the letter (issues/479 DF-01).
#[test]
fn diff_patch_omits_unchanged_stat_invalid_entry() {
    let repo = repo_with_committed_empty_file();
    let root = repo.path();

    let patch = run_libra_command(&["diff-index", "-p", "HEAD", "--", "empty.txt"], root);
    assert_eq!(patch.status.code(), Some(0));
    assert!(
        patch.stdout.is_empty(),
        "an unchanged entry has no patch body: {}",
        String::from_utf8_lossy(&patch.stdout)
    );

    let files_patch = run_libra_command(&["diff-files", "-p", "--", "empty.txt"], root);
    assert!(
        files_patch.stdout.is_empty(),
        "diff-files -p must also be empty: {}",
        String::from_utf8_lossy(&files_patch.stdout)
    );

    // The raw record still reports the pessimistic `M` with the zero worktree id.
    let raw = run_libra_command(&["diff-index", "HEAD", "--", "empty.txt"], root);
    assert!(
        String::from_utf8_lossy(&raw.stdout).contains(" 0000000 M"),
        "raw must keep the pessimistic record: {}",
        String::from_utf8_lossy(&raw.stdout)
    );

    // A real content change still renders a patch.
    fs::write(root.join("tracked.txt"), "changed\n").expect("modify tracked file");
    assert_cli_success(
        &run_libra_command(&["read-tree", "HEAD"], root),
        "read-tree HEAD",
    );
    let changed = run_libra_command(&["diff-index", "-p", "HEAD", "--", "tracked.txt"], root);
    let changed_text = String::from_utf8_lossy(&changed.stdout);
    assert!(
        changed_text.contains("\n@@ ") && changed_text.contains("-tracked"),
        "a real change keeps its hunk: {changed_text}"
    );
}

#[test]
fn diff_files_read_tree_empty_blob_is_modified_not_added() {
    let repo = repo_with_committed_empty_file();
    let record = String::from_utf8_lossy(
        &run_libra_command(&["diff-files", "--", "empty.txt"], repo.path()).stdout,
    )
    .trim()
    .to_string();
    assert!(
        record.ends_with(" M\tempty.txt"),
        "diff-files must report M for an empty tracked file: {record}"
    );
}

#[test]
fn diff_index_read_tree_worktree_truncated_to_empty_is_modified_not_deleted() {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    // Truncate the tracked file to zero bytes, then invalidate the stat cache.
    fs::write(root.join("tracked.txt"), "").expect("truncate tracked file");
    assert_cli_success(
        &run_libra_command(&["read-tree", "HEAD"], root),
        "read-tree HEAD",
    );

    let record = String::from_utf8_lossy(
        &run_libra_command(&["diff-index", "HEAD", "--", "tracked.txt"], root).stdout,
    )
    .trim()
    .to_string();
    assert!(
        record.ends_with(" M\ttracked.txt"),
        "an emptied worktree file is a modification, never a deletion: {record}"
    );
}

/// Guard the other direction: real additions and deletions still report `A`/`D`.
#[test]
fn diff_status_keeps_real_additions_and_deletions() {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    fs::write(root.join("added.txt"), "added\n").expect("create added file");
    assert_cli_success(
        &run_libra_command(&["add", "added.txt"], root),
        "stage added",
    );
    assert_cli_success(
        &run_libra_command(&["rm", "--cached", "tracked.txt"], root),
        "drop tracked.txt from the index",
    );

    let named = String::from_utf8_lossy(
        &run_libra_command(&["diff-index", "--name-status", "HEAD"], root).stdout,
    )
    .to_string();
    assert!(
        named.contains("A\tadded.txt"),
        "a path absent from the tree is added: {named}"
    );
    assert!(
        named.contains("D\ttracked.txt"),
        "a path absent from the index is deleted: {named}"
    );
}
