//! Integration tests for the diff plumbing trio: `diff-tree` / `diff-index` /
//! `diff-files`.
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network).

use std::{fs, process::Output};

use serial_test::serial;
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

/// A committed, tracked `foo` whose worktree copy is rewritten with the SAME
/// bytes but an older mtime: stat-dirty, content-clean. This is the state the
/// upstream t3700#24 sequence (TC-0044) hits — `foo` is already tracked from
/// earlier tests when `>foo && git add foo && git commit -a` runs.
fn repo_with_stat_dirty_tracked_file() -> TempDir {
    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    fs::write(root.join("foo"), "").expect("create foo");
    assert_cli_success(&run_libra_command(&["add", "foo"], root), "stage foo");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "foo", "--no-verify"], root),
        "commit foo",
    );
    fs::write(root.join("foo"), "").expect("rewrite foo");
    let file = fs::File::options()
        .write(true)
        .open(root.join("foo"))
        .expect("open foo");
    // A day back, not UNIX_EPOCH: status treats a zero time as untrusted.
    let backdated = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400);
    file.set_times(fs::FileTimes::new().set_modified(backdated))
        .expect("backdate foo");
    let dirty = run_libra_command(&["diff-files", "--", "foo"], root);
    assert!(
        String::from_utf8_lossy(&dirty.stdout).ends_with(" M\tfoo\n"),
        "fixture must start stat-dirty: {}",
        String::from_utf8_lossy(&dirty.stdout)
    );
    repo
}

fn assert_plumbing_clean(root: &std::path::Path, after: &str) {
    for cmd in [
        vec!["diff-index", "HEAD", "--", "foo"],
        vec!["diff-files", "--", "foo"],
    ] {
        let out = run_libra_command(&cmd, root);
        assert_eq!(out.status.code(), Some(0), "{cmd:?} after {after}");
        assert!(
            out.stdout.is_empty(),
            "{cmd:?} must be empty after {after} (stat re-recorded like Git): {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

/// Git's `add <path>`, `add -u` and `add .` re-record the stat of a matched
/// tracked path whose content is unchanged; Libra's staging loop only saw
/// content changes, so the entry stayed stat-dirty (issues/479 TC-0044).
#[test]
fn add_rerecords_stat_of_unchanged_tracked_path() {
    for args in [vec!["add", "foo"], vec!["add", "-u"], vec!["add", "."]] {
        let repo = repo_with_stat_dirty_tracked_file();
        let root = repo.path();
        let out = run_libra_command(&args, root);
        assert_cli_success(&out, &format!("{args:?}"));
        assert!(
            out.stdout.is_empty(),
            "{args:?} on a pipe stays silent: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert_plumbing_clean(root, &format!("{args:?}"));
    }
}

/// A pathspec that does not match the stat-dirty entry must leave it alone.
#[test]
fn add_leaves_unmatched_stat_dirty_entry_untouched() {
    let repo = repo_with_stat_dirty_tracked_file();
    let root = repo.path();
    fs::write(root.join("other.txt"), "other\n").expect("create other");
    assert_cli_success(
        &run_libra_command(&["add", "other.txt"], root),
        "add other.txt",
    );
    let out = run_libra_command(&["diff-files", "--", "foo"], root);
    assert!(
        String::from_utf8_lossy(&out.stdout).ends_with(" M\tfoo\n"),
        "an unmatched entry keeps its stale stat: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// Git's `commit -a` (`add_files_to_cache`) refreshes the stat of unchanged
/// tracked paths as a side effect of a successful commit (first half: Git
/// parity). The second half pins an intentional Libra difference: Git
/// prepares `-a` in a temporary index and rolls the refresh back when the
/// commit is refused, while Libra's `-a` writes the index before the commit
/// runs (as it already did for auto-staged content), so the stat refresh is
/// kept even when the commit is then refused as "nothing to commit".
#[test]
fn commit_all_rerecords_stat_of_unchanged_tracked_path() {
    let repo = repo_with_stat_dirty_tracked_file();
    let root = repo.path();
    fs::write(root.join("changed.txt"), "v1\n").expect("create changed");
    assert_cli_success(
        &run_libra_command(&["add", "changed.txt"], root),
        "add changed",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "c", "--no-verify"], root),
        "commit changed",
    );
    fs::write(root.join("changed.txt"), "v2\n").expect("modify changed");
    assert_cli_success(
        &run_libra_command(&["commit", "-a", "-m", "all", "--no-verify"], root),
        "commit -a",
    );
    assert_plumbing_clean(root, "commit -a");

    // A stat-only refresh is not staged content: with nothing else changed,
    // `commit -a` must still refuse as "nothing to commit". Intentional
    // difference: unlike Git, the refreshed stat is kept after the refusal.
    let repo = repo_with_stat_dirty_tracked_file();
    let root = repo.path();
    let out = run_libra_command(&["commit", "-a", "-m", "none", "--no-verify"], root);
    assert_ne!(
        out.status.code(),
        Some(0),
        "nothing to commit must still fail"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("nothing to commit"),
        "refused as nothing to commit, not another error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_plumbing_clean(root, "commit -a with nothing to commit");
}

/// Make `name` racily clean but modified: its index entry keeps the
/// worktree file's exact stat while pointing at the blob of different,
/// same-size content, and the index file is backdated so the entry is not
/// older than the index snapshot. Only a content compare can see the edit.
fn make_racy_modified_entry(root: &std::path::Path, name: &str) {
    use git_internal::{
        hash::HashKind,
        internal::index::{Index, IndexEntry},
    };

    fs::write(root.join(name), "old\n").expect("create racy file");
    assert_cli_success(&run_libra_command(&["add", name], root), "add racy file");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "racy", "--no-verify"], root),
        "commit racy file",
    );
    fs::write(root.join(name), "new\n").expect("same-size edit");
    let index_path = root.join(".libra/index");
    let mut index = Index::load_with_hash_kind(HashKind::Sha1, &index_path).expect("load index");
    let (old_hash, mode) = {
        let entry = index.get(name, 0).expect("racy file tracked");
        (entry.hash, entry.mode)
    };
    let metadata = fs::symlink_metadata(root.join(name)).expect("stat racy file");
    let mut entry = IndexEntry::new(&metadata, old_hash, name.to_string());
    entry.mode = mode;
    index.update(entry);
    index
        .save_with_hash_kind(HashKind::Sha1, &index_path)
        .expect("save index");
    let file_mtime = metadata.modified().expect("racy mtime");
    fs::File::options()
        .write(true)
        .open(&index_path)
        .expect("open index")
        .set_times(
            fs::FileTimes::new().set_modified(file_mtime - std::time::Duration::from_secs(2)),
        )
        .expect("backdate index");
}

/// R1-P1-1 (FX-479-03 review): a stat-only refresh makes `commit -a` save the
/// index even when there is nothing to commit, which advances the index
/// mtime. A racily clean entry whose content differs must be smudged first,
/// so a later `status` still content-compares it instead of trusting its stat
/// against the newer index (Git's `ce_smudge_racily_clean_entry`).
#[test]
fn commit_all_stat_refresh_keeps_racy_modified_entry_visible() {
    let repo = repo_with_stat_dirty_tracked_file();
    let root = repo.path();
    make_racy_modified_entry(root, "racy.txt");
    let before = run_libra_command(&["status", "--porcelain"], root);
    assert!(
        out_str(&before).contains(" M racy.txt"),
        "fixture: the racy guard must report the edit before commit -a: {}",
        out_str(&before)
    );

    // foo needs a stat-only refresh, so commit -a rewrites the index.
    let _ = run_libra_command(&["commit", "-a", "-m", "all", "--no-verify"], root);

    let committed_new = out_str(&run_libra_command(&["show", "HEAD:racy.txt"], root)) == "new\n";
    let after = run_libra_command(&["status", "--porcelain"], root);
    assert!(
        committed_new || out_str(&after).contains(" M racy.txt"),
        "the same-size edit must stay visible (or be committed), never hidden: \
         status={:?}",
        out_str(&after)
    );
    assert_plumbing_clean(root, "commit -a with a racy sibling");
}

/// R2-P1-1 (FX-479-03 review): `commit -a` with a racily clean LFS-tracked
/// file that cannot be read must fail closed without a panic and without
/// writing the index. The operation boundary's working-copy scan refuses it
/// before staging (`LBR-AUTH-002`, 128); the smudge pass itself treats an
/// unreadable racy entry as content-unknown (unit tests in
/// `src/command/refresh.rs`), and `calc_file_blob_hash` now returns the LFS
/// read error instead of panicking.
#[cfg(unix)]
#[test]
fn commit_all_with_unreadable_racy_lfs_file_fails_closed_without_panic() {
    use std::os::unix::fs::PermissionsExt;

    let repo = repo_with_stat_dirty_tracked_file();
    let root = repo.path();
    assert_cli_success(
        &run_libra_command(&["lfs", "track", "*.bin"], root),
        "lfs track",
    );
    make_racy_modified_entry(root, "big.bin");
    let file = root.join("big.bin");
    let index_before = fs::read(root.join(".libra/index")).expect("read index");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).expect("make unreadable");
    let readable = fs::read(&file).is_ok();

    let out = run_libra_command(&["commit", "-a", "-m", "all", "--no-verify"], root);
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("restore mode");
    if readable {
        // Running as root: permissions do not block the read.
        return;
    }

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked"),
        "an unreadable LFS file must not panic commit -a: {stderr}"
    );
    assert_eq!(out.status.code(), Some(128), "fails closed: {stderr}");
    assert_eq!(
        fs::read(root.join(".libra/index")).expect("reread index"),
        index_before,
        "a refused commit -a must not write the index"
    );
}

/// R1-P2-1 (FX-479-03 review): the stat-refresh pass clears a stale legacy
/// `CE_VALID` bit (issues/637) even on a stat-clean entry; `commit -a` must
/// persist that index change instead of depending on an unrelated stat-dirty
/// file to trigger the save. Nothing to commit still refuses as before.
#[test]
fn commit_all_persists_cleared_assume_valid_bit() {
    use git_internal::{
        hash::HashKind,
        internal::index::{Index, IndexEntry},
    };

    let repo = create_committed_repo_via_cli();
    let root = repo.path();
    fs::write(root.join("kept.txt"), "kept\n").expect("create kept");
    assert_cli_success(&run_libra_command(&["add", "kept.txt"], root), "add kept");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "kept", "--no-verify"], root),
        "commit kept",
    );
    let index_path = root.join(".libra/index");
    let mut index = Index::load_with_hash_kind(HashKind::Sha1, &index_path).expect("load index");
    let (hash, mode) = {
        let entry = index.get("kept.txt", 0).expect("kept tracked");
        (entry.hash, entry.mode)
    };
    let metadata = fs::symlink_metadata(root.join("kept.txt")).expect("stat kept");
    let mut entry = IndexEntry::new(&metadata, hash, "kept.txt".to_string());
    entry.mode = mode;
    entry.flags.assume_valid = true;
    index.update(entry);
    index
        .save_with_hash_kind(HashKind::Sha1, &index_path)
        .expect("save index");

    let out = run_libra_command(&["commit", "-a", "-m", "none", "--no-verify"], root);
    assert_ne!(
        out.status.code(),
        Some(0),
        "nothing to commit must still fail"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("nothing to commit"),
        "refused as nothing to commit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let index = Index::load_with_hash_kind(HashKind::Sha1, &index_path).expect("reload index");
    assert!(
        !index
            .get("kept.txt", 0)
            .expect("kept still tracked")
            .flags
            .assume_valid,
        "the cleared stale CE_VALID bit must be saved"
    );
}

/// The full upstream t3700#24 sequence (TC-0044) on an ALREADY-TRACKED foo:
/// `>foo && add foo && commit -a` must leave `diff-index` empty BEFORE
/// `read-tree` makes it racy; `add --refresh` then clears the racy `M`.
#[test]
fn t3700_24_add_refresh_sequence_on_tracked_file() {
    let repo = repo_with_stat_dirty_tracked_file();
    let root = repo.path();
    fs::write(root.join("bar"), "bar\n").expect("create bar");
    assert_cli_success(&run_libra_command(&["add", "bar"], root), "add bar");
    assert_cli_success(&run_libra_command(&["add", "foo"], root), "add foo");
    assert_cli_success(
        &run_libra_command(&["commit", "-a", "-m", "commit all", "--no-verify"], root),
        "commit all",
    );
    assert_plumbing_clean(root, "add foo && commit -a");

    assert_cli_success(
        &run_libra_command(&["read-tree", "HEAD"], root),
        "read-tree",
    );
    let racy = run_libra_command(&["diff-index", "HEAD", "--", "foo"], root);
    let record = String::from_utf8_lossy(&racy.stdout);
    assert!(
        record.starts_with(":100644 100644 ") && record.ends_with(" M\tfoo\n"),
        "read-tree leaves a racy M record: {record}"
    );
    let refresh = run_libra_command(&["add", "--refresh", "--", "foo"], root);
    assert_cli_success(&refresh, "add --refresh");
    assert!(
        refresh.stdout.is_empty(),
        "add --refresh on a pipe is silent like Git (#659): {}",
        String::from_utf8_lossy(&refresh.stdout)
    );
    let after = run_libra_command(&["diff-index", "HEAD", "--", "foo"], root);
    assert!(
        after.stdout.is_empty(),
        "add --refresh clears the racy entry: {}",
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

#[test]
fn i1062_plumbing_explicit_conversion_matches_git_modes() {
    use super::add_test::{i1062_command, i1062_ok, i1062_run};
    for format in ["sha1", "sha256"] {
        let temp = super::status_test::i1062_committed_scripts(format);
        let repo = temp.path().join("repo");
        let git = |args: &[&str]| {
            let template = i1062_command(&repo, &[]);
            let mut command = std::process::Command::new("git");
            command.args(args).current_dir(&repo);
            for (key, value) in template.get_envs() {
                if let Some(value) = value {
                    command.env(key, value);
                } else {
                    command.env_remove(key);
                }
            }
            command.output().unwrap()
        };
        for args in [
            vec!["init", "-b", "main", "--object-format", format],
            vec!["config", "user.name", "EOL"],
            vec!["config", "user.email", "eol@example.test"],
        ] {
            assert_cli_success(&git(&args), "Git fixture");
        }
        for name in ["probe.ps1", "probe.bat", "probe.cmd"] {
            fs::write(repo.join(name), b"one\ntwo\n").unwrap();
        }
        assert_cli_success(
            &git(&[
                "add",
                ".gitattributes",
                "probe.ps1",
                "probe.bat",
                "probe.cmd",
            ]),
            "Git add",
        );
        assert_cli_success(&git(&["commit", "-m", "scripts"]), "Git commit");
        let git_index = fs::read(repo.join(".git/index")).unwrap();
        let libra_index = fs::read(repo.join(".libra/index")).unwrap();
        for content in [
            b"one\r\ntwo\r\n".as_slice(),
            b"one\r\nedited\r\n".as_slice(),
        ] {
            fs::write(repo.join("probe.ps1"), content).unwrap();
            for command in ["diff-files", "diff-index"] {
                for flags in [
                    vec![],
                    vec!["--raw"],
                    vec!["-p"],
                    vec!["--name-only"],
                    vec!["--name-status"],
                    vec!["--quiet"],
                    vec!["--exit-code"],
                    vec!["-p", "--exit-code"],
                    vec!["-p", "--quiet"],
                    vec!["-s", "--exit-code"],
                ] {
                    fs::write(repo.join(".git/index"), &git_index).unwrap();
                    let mut args = vec![command];
                    args.extend(flags);
                    if command == "diff-index" {
                        args.push("HEAD");
                    }
                    args.extend(["--", "probe.ps1"]);
                    let mut git_args = args.clone();
                    // Libra retains its existing seven-digit raw OID display.
                    git_args.insert(1, "--abbrev=7");
                    let expected = git(&git_args);
                    let actual = i1062_run(&repo, &args);
                    assert_eq!(
                        actual.status.code(),
                        expected.status.code(),
                        "{format} {args:?}: {actual:?}"
                    );
                    assert_eq!(actual.stdout, expected.stdout, "{format} {args:?}");
                    assert_eq!(fs::read(repo.join(".libra/index")).unwrap(), libra_index);
                }
            }
        }
        // All stat-dirty candidates share the zero OID, but never share bytes.
        for (name, content) in [
            ("probe.ps1", b"one\r\nps1 edit\r\n".as_slice()),
            ("probe.bat", b"one\r\nbat edit\r\n".as_slice()),
            ("probe.cmd", b"one\r\ntwo\r\n".as_slice()),
        ] {
            fs::write(repo.join(name), content).unwrap();
        }
        for command in ["diff-files", "diff-index"] {
            fs::write(repo.join(".git/index"), &git_index).unwrap();
            let mut args = vec![command, "-p", "--exit-code"];
            if command == "diff-index" {
                args.push("HEAD");
            }
            args.extend(["--", "probe.ps1", "probe.bat", "probe.cmd"]);
            let mut git_args = args.clone();
            git_args.insert(1, "--abbrev=7");
            let expected = git(&git_args);
            let actual = i1062_run(&repo, &args);
            assert_eq!(actual.status.code(), expected.status.code(), "{args:?}");
            // Libra separates file patches with a blank line. Compare every
            // patch byte, allowing only that existing inter-file separator.
            let patches = |bytes: &[u8]| {
                String::from_utf8(bytes.to_vec())
                    .unwrap()
                    .split("diff --git ")
                    .map(|patch| patch.trim_matches('\n').to_owned())
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                patches(&actual.stdout),
                patches(&expected.stdout),
                "{args:?}"
            );
            assert_eq!(fs::read(repo.join(".libra/index")).unwrap(), libra_index);
        }
        assert!(
            i1062_ok(
                &repo,
                &["diff-index", "--cached", "HEAD", "--", "probe.ps1"]
            )
            .is_empty()
        );
        fs::write(repo.join("intent.ps1"), b"new\r\nscript\r\n").unwrap();
        assert_cli_success(&git(&["add", "-N", "intent.ps1"]), "Git intent-to-add");
        i1062_ok(&repo, &["add", "-N", "intent.ps1"]);
        let git_index = fs::read(repo.join(".git/index")).unwrap();
        let libra_index = fs::read(repo.join(".libra/index")).unwrap();
        for command in ["diff-files", "diff-index"] {
            for flag in ["-p", "--raw", "--quiet"] {
                fs::write(repo.join(".git/index"), &git_index).unwrap();
                let mut args = vec![command, flag];
                if command == "diff-index" {
                    args.push("HEAD");
                }
                args.extend(["--", "intent.ps1"]);
                let mut git_args = args.clone();
                git_args.insert(1, "--abbrev=7");
                let expected = git(&git_args);
                let actual = i1062_run(&repo, &args);
                assert_eq!(
                    actual.status.code(),
                    expected.status.code(),
                    "{args:?}: {actual:?}"
                );
                // git-internal's existing empty-side hunk starts at 1;
                // Git prints 0. Keep all OIDs, body bytes and exit checks exact.
                let actual_text = String::from_utf8(actual.stdout)
                    .unwrap()
                    .replace("@@ -1,0 +1,2 @@", "@@ -0,0 +1,2 @@");
                assert_eq!(actual_text.as_bytes(), expected.stdout, "{args:?}");
                assert_eq!(fs::read(repo.join(".libra/index")).unwrap(), libra_index);
            }
        }
    }
}

#[test]
fn i861_auto_patch_cleaning_preserves_stat_only_exit_contract() {
    use super::add_test::{i1062_ok, i1062_run};
    let temp = super::status_test::i861_committed_scripts("sha1");
    let repo = temp.path().join("repo");
    for args in [
        vec!["diff-files", "-p", "--", "probe.ps1"],
        vec!["diff-index", "-p", "HEAD", "--", "probe.ps1"],
    ] {
        assert!(i1062_ok(&repo, &args).is_empty(), "{args:?}");
    }
    for args in [
        vec!["diff-files", "--quiet", "--", "probe.ps1"],
        vec!["diff-index", "--quiet", "HEAD", "--", "probe.ps1"],
    ] {
        assert_eq!(i1062_run(&repo, &args).status.code(), Some(1), "{args:?}");
    }
    i1062_ok(&repo, &["diff", "--quiet", "--", "probe.ps1"]);
}

/// LG-09 (M-DTREE D1–D7): `diff-tree` default raw, recursion, `-t`, single
/// commit form, `--root`, and patch output match Git.
#[test]
#[serial(cwd)]
fn test_diff_tree_raw_and_patch_matrix() {
    let repo = create_committed_repo_via_cli();
    let p = repo.path();
    fs::create_dir_all(p.join("dir1/sub")).unwrap();
    fs::create_dir_all(p.join("dir2")).unwrap();
    fs::write(p.join("dir1/sub/x"), "a\n").unwrap();
    fs::write(p.join("dir2/z"), "a\nb\n").unwrap();
    fs::write(p.join("tracked.txt"), "one\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "-A"], p), "add base");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], p),
        "commit base",
    );
    fs::write(p.join("dir1/sub/x"), "a\nb\n").unwrap();
    fs::write(p.join("dir2/z"), "a\nb\nc\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "-A"], p), "add change");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "change", "--no-verify"], p),
        "commit change",
    );

    let head = rev_parse(&repo, "HEAD");
    let head_parent = rev_parse(&repo, "HEAD~1");

    // D1: default is non-recursive raw with full OIDs and tree entries.
    let out = run_libra_command(&["diff-tree", "HEAD~1", "HEAD"], p);
    assert_cli_success(&out, "diff-tree default");
    let text = out_str(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].starts_with(":040000 040000 "), "{text}");
    assert!(lines[0].ends_with("\tdir1"), "{text}");
    assert!(lines[1].ends_with("\tdir2"), "{text}");
    // Full 40-char object ids, not 7-char abbreviations.
    let oids: Vec<&str> = lines[0].split_whitespace().skip(2).take(2).collect();
    assert!(oids.iter().all(|oid| oid.len() == 40), "{text}");

    // D2: `-r` lists blobs; `--raw -r` is identical.
    let recursive = run_libra_command(&["diff-tree", "-r", "HEAD~1", "HEAD"], p);
    assert_cli_success(&recursive, "diff-tree -r");
    let rtext = out_str(&recursive);
    assert!(rtext.contains("M\tdir1/sub/x"), "{rtext}");
    assert!(rtext.contains("M\tdir2/z"), "{rtext}");
    assert!(!rtext.contains(":040000"), "{rtext}");
    let raw_r = run_libra_command(&["diff-tree", "--raw", "-r", "HEAD~1", "HEAD"], p);
    assert_eq!(out_str(&raw_r), rtext, "--raw -r matches -r");

    // D6: `-t` includes tree entries while recursing.
    let with_trees = run_libra_command(&["diff-tree", "-r", "-t", "HEAD~1", "HEAD"], p);
    assert_cli_success(&with_trees, "diff-tree -r -t");
    let ttext = out_str(&with_trees);
    assert!(ttext.contains("M\tdir1"), "{ttext}");
    assert!(ttext.contains("M\tdir1/sub"), "{ttext}");
    assert!(ttext.contains("M\tdir1/sub/x"), "{ttext}");

    // D4: single-commit form prints the commit id line, then non-recursive raw.
    let single = run_libra_command(&["diff-tree", "HEAD"], p);
    assert_cli_success(&single, "diff-tree HEAD");
    let stext = out_str(&single);
    let mut single_lines = stext.lines();
    assert_eq!(single_lines.next().unwrap().trim(), head, "{stext}");
    assert!(
        single_lines.next().unwrap().starts_with(":040000"),
        "{stext}"
    );
    let no_id = run_libra_command(&["diff-tree", "--no-commit-id", "HEAD"], p);
    assert_cli_success(&no_id, "diff-tree --no-commit-id");
    let ntext = out_str(&no_id);
    assert!(!ntext.starts_with(&head), "{ntext}");
    assert!(ntext.starts_with(":040000"), "{ntext}");

    // D3: `-p` renders a patch (single form keeps the id line).
    let patch = run_libra_command(&["diff-tree", "-p", "HEAD~1", "HEAD"], p);
    assert_cli_success(&patch, "diff-tree -p");
    let ptext = out_str(&patch);
    assert!(
        ptext.contains("diff --git a/dir1/sub/x b/dir1/sub/x"),
        "{ptext}"
    );
    assert!(ptext.contains("@@ -1 +1,2 @@"), "{ptext}");
    let single_patch = run_libra_command(&["diff-tree", "-p", "HEAD"], p);
    assert_cli_success(&single_patch, "diff-tree -p HEAD");
    assert!(
        out_str(&single_patch).starts_with(&format!("{head}\n")),
        "id line first"
    );

    // D5: `-r --root` renders the root commit against the empty tree.
    let root = rev_parse(&repo, &format!("{head_parent}~1"));
    let root_out = run_libra_command(&["diff-tree", "-r", "--root", &root], p);
    assert_cli_success(&root_out, "diff-tree --root");
    let root_text = out_str(&root_out);
    assert!(root_text.starts_with(&format!("{root}\n")), "{root_text}");
    assert!(
        root_text.contains(":000000 100644 0000000000000000000000000000000000000000 "),
        "{root_text}"
    );
    // The helper's initial commit contains only `tracked.txt`.
    assert!(root_text.contains("A\ttracked.txt"), "{root_text}");
    // Without `--root`, a root commit prints only the id line.
    let root_default = run_libra_command(&["diff-tree", &root], p);
    assert_eq!(
        out_str(&root_default).trim(),
        root,
        "root default is id only"
    );
}
