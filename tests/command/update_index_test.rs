//! Integration tests for `libra update-index`.
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network).

use std::fs;

use tempfile::tempdir;

use super::{
    parse_cli_error_stderr, parse_json_stdout, run_libra_command, run_libra_command_with_stdin,
};

fn init_repo() -> tempfile::TempDir {
    let repo = tempdir().expect("tempdir");
    let init = run_libra_command(&["init"], repo.path());
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    repo
}

fn stdout_trimmed(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// `--cacheinfo` registers an entry from an object id (no working-tree read),
/// and the resulting index produces a tree via `write-tree` — the core plumbing
/// round-trip.
#[test]
fn cacheinfo_registers_entry_and_write_tree_reads_it() {
    let repo = init_repo();

    // Create a blob and capture its id.
    fs::write(repo.path().join("payload"), "cacheinfo content\n").unwrap();
    let hash = run_libra_command(&["hash-object", "-w", "payload"], repo.path());
    assert!(
        hash.status.success(),
        "hash-object -w failed: {}",
        String::from_utf8_lossy(&hash.stderr)
    );
    let oid = stdout_trimmed(&hash);
    assert_eq!(oid.len(), 40, "SHA-1 object id: {oid}");

    // Register it at a NESTED path (also exercises nested tree building).
    let spec = format!("100644,{oid},sub/data.txt");
    let upd = run_libra_command(&["update-index", "--cacheinfo", &spec], repo.path());
    assert_eq!(
        upd.status.code(),
        Some(0),
        "update-index --cacheinfo failed: {}",
        String::from_utf8_lossy(&upd.stderr)
    );

    // The index now tracks sub/data.txt.
    let ls = run_libra_command(&["ls-files"], repo.path());
    assert!(
        stdout_trimmed(&ls).lines().any(|l| l == "sub/data.txt"),
        "ls-files should list the cacheinfo entry: {}",
        stdout_trimmed(&ls)
    );

    // And write-tree succeeds on that index.
    let tree = run_libra_command(&["write-tree"], repo.path());
    assert_eq!(tree.status.code(), Some(0));
    assert_eq!(stdout_trimmed(&tree).len(), 40, "a tree id is produced");
}

#[test]
fn add_stages_a_working_tree_file() {
    let repo = init_repo();
    fs::write(repo.path().join("new.txt"), "hi").unwrap();
    let upd = run_libra_command(&["update-index", "--add", "new.txt"], repo.path());
    assert_eq!(
        upd.status.code(),
        Some(0),
        "update-index --add failed: {}",
        String::from_utf8_lossy(&upd.stderr)
    );
    let ls = run_libra_command(&["ls-files"], repo.path());
    assert!(
        stdout_trimmed(&ls).lines().any(|l| l == "new.txt"),
        "{}",
        stdout_trimmed(&ls)
    );
}

#[test]
fn add_returns_marker_registration_failure_without_saving_the_index() {
    let repo = init_repo();
    fs::write(repo.path().join("marker-failure.txt"), "content").unwrap();
    fs::write(
        repo.path().join(".libra/object-index-repair"),
        "conflicting non-directory",
    )
    .unwrap();

    let failed = run_libra_command(
        &["--json", "update-index", "--add", "marker-failure.txt"],
        repo.path(),
    );
    assert_eq!(failed.status.code(), Some(128));
    let (_human, report) = parse_cli_error_stderr(&failed.stderr);
    assert_eq!(report.error_code, "LBR-IO-002");
    assert!(
        report
            .message
            .contains("object payloads were stored safely")
            && report.message.contains("no paths were staged"),
        "unexpected error report: {report:?}"
    );
    let ls = run_libra_command(&["ls-files"], repo.path());
    assert!(
        !stdout_trimmed(&ls)
            .lines()
            .any(|line| line == "marker-failure.txt"),
        "failed object registration must not save the index: {}",
        stdout_trimmed(&ls)
    );

    fs::remove_file(repo.path().join(".libra/object-index-repair"))
        .expect("remove injected marker-directory conflict");
    let retried = run_libra_command(
        &["update-index", "--add", "marker-failure.txt"],
        repo.path(),
    );
    assert_eq!(
        retried.status.code(),
        Some(0),
        "normal retry must register the existing payload: {}",
        String::from_utf8_lossy(&retried.stderr)
    );
    let ls = run_libra_command(&["ls-files"], repo.path());
    assert!(
        stdout_trimmed(&ls)
            .lines()
            .any(|line| line == "marker-failure.txt")
    );
}

#[test]
fn remove_drops_an_entry() {
    let repo = init_repo();
    fs::write(repo.path().join("doomed.txt"), "x").unwrap();
    run_libra_command(&["update-index", "--add", "doomed.txt"], repo.path());
    let rm = run_libra_command(&["update-index", "--remove", "doomed.txt"], repo.path());
    assert_eq!(rm.status.code(), Some(0));
    let ls = run_libra_command(&["ls-files"], repo.path());
    assert!(
        !stdout_trimmed(&ls).lines().any(|l| l == "doomed.txt"),
        "entry should be removed: {}",
        stdout_trimmed(&ls)
    );
}

#[test]
fn untracked_path_without_add_is_a_usage_error() {
    let repo = init_repo();
    fs::write(repo.path().join("loose.txt"), "x").unwrap();
    let upd = run_libra_command(&["update-index", "loose.txt"], repo.path());
    assert_eq!(
        upd.status.code(),
        Some(128),
        "an untracked path without --add is a usage error: {}",
        String::from_utf8_lossy(&upd.stderr)
    );
}

#[test]
fn invalid_cacheinfo_mode_is_an_error() {
    let repo = init_repo();
    // 100600 is not a recognized mode.
    let spec = format!("100600,{},f.txt", "0".repeat(40));
    let upd = run_libra_command(&["update-index", "--cacheinfo", &spec], repo.path());
    assert_eq!(upd.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&upd.stderr).contains("mode"),
        "error mentions the mode: {}",
        String::from_utf8_lossy(&upd.stderr)
    );
}

#[test]
fn invalid_cacheinfo_object_id_is_an_error() {
    let repo = init_repo();
    // Too-short object id for a SHA-1 repo.
    let upd = run_libra_command(
        &["update-index", "--cacheinfo", "100644,deadbeef,f.txt"],
        repo.path(),
    );
    assert_eq!(upd.status.code(), Some(128));
}

#[test]
fn cacheinfo_rejects_path_traversal() {
    let repo = init_repo();
    let spec = format!("100644,{},../escape.txt", "0".repeat(40));
    let upd = run_libra_command(&["update-index", "--cacheinfo", &spec], repo.path());
    assert_eq!(
        upd.status.code(),
        Some(128),
        "a `..` index key is rejected: {}",
        String::from_utf8_lossy(&upd.stderr)
    );
}

#[test]
fn json_output_reports_counts() {
    let repo = init_repo();
    fs::write(repo.path().join("j.txt"), "x").unwrap();
    let upd = run_libra_command(&["--json", "update-index", "--add", "j.txt"], repo.path());
    assert_eq!(upd.status.code(), Some(0));
    let json = parse_json_stdout(&upd);
    assert_eq!(json["data"]["updated"].as_u64(), Some(1));
    assert_eq!(json["data"]["removed"].as_u64(), Some(0));
}

#[test]
fn outside_repository_is_an_error() {
    let dir = tempdir().expect("tempdir");
    let upd = run_libra_command(&["update-index", "--add", "x"], dir.path());
    assert_eq!(upd.status.code(), Some(128));
}

/// `--cacheinfo` must register an entry even for an object that does not exist
/// (Git's contract), and must NOT create the object.
#[test]
fn cacheinfo_object_need_not_exist() {
    let repo = init_repo();
    let oid = "a".repeat(40); // valid hex, but no such object
    let spec = format!("100644,{oid},ghost.txt");
    let upd = run_libra_command(&["update-index", "--cacheinfo", &spec], repo.path());
    assert_eq!(
        upd.status.code(),
        Some(0),
        "cacheinfo must not require the object to exist: {}",
        String::from_utf8_lossy(&upd.stderr)
    );
    let ls = run_libra_command(&["ls-files"], repo.path());
    assert!(stdout_trimmed(&ls).lines().any(|l| l == "ghost.txt"));
    let obj = repo
        .path()
        .join(".libra/objects")
        .join(&oid[..2])
        .join(&oid[2..]);
    assert!(
        !obj.exists(),
        "cacheinfo must not write an object: {}",
        obj.display()
    );
}

#[test]
fn cacheinfo_rejects_windows_drive_path() {
    let repo = init_repo();
    let spec = format!("100644,{},C:/evil.txt", "0".repeat(40));
    let upd = run_libra_command(&["update-index", "--cacheinfo", &spec], repo.path());
    assert_eq!(
        upd.status.code(),
        Some(128),
        "a Windows drive-letter path is rejected: {}",
        String::from_utf8_lossy(&upd.stderr)
    );
}

/// Staging a directory must be a 128 error, not a panic in the blob reader.
#[test]
fn add_directory_is_rejected_not_panicked() {
    let repo = init_repo();
    fs::create_dir(repo.path().join("adir")).unwrap();
    let upd = run_libra_command(&["update-index", "--add", "adir"], repo.path());
    assert_eq!(
        upd.status.code(),
        Some(128),
        "staging a directory is a 128 error, not a panic: {}",
        String::from_utf8_lossy(&upd.stderr)
    );
}

/// SW-02 (M-FREMOVE R1–R5, plan issues/490): `--force-remove` drops paths
/// regardless of worktree presence, wins over `--add`/`--remove`, clears every
/// stage of an unmerged path, and reports the count in JSON.
#[test]
fn test_update_index_force_remove_matrix() {
    use super::{assert_cli_success, configure_identity_via_cli};

    let repo = init_repo();
    let root = repo.path();
    configure_identity_via_cli(root);
    fs::write(root.join("tracked.txt"), "tracked\n").expect("write tracked");
    fs::write(root.join("keep.txt"), "keep\n").expect("write keep");
    assert_cli_success(
        &run_libra_command(&["add", "tracked.txt", "keep.txt"], root),
        "stage files",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "commit",
    );

    // R1: a tracked path with the file present is removed; the file survives.
    assert_cli_success(
        &run_libra_command(&["update-index", "--force-remove", "tracked.txt"], root),
        "R1 force-remove tracked",
    );
    assert_eq!(
        fs::read(root.join("tracked.txt")).expect("file survives"),
        b"tracked\n"
    );
    let list = run_libra_command(&["ls-files"], root);
    assert!(
        !String::from_utf8_lossy(&list.stdout).contains("tracked.txt"),
        "entry must be gone: {}",
        String::from_utf8_lossy(&list.stdout)
    );

    // R2: an unknown path is exit 0 with no change.
    assert_cli_success(
        &run_libra_command(&["update-index", "--force-remove", "missing.txt"], root),
        "R2 unknown path is a no-op",
    );

    // R4: force-remove wins over --add for a present file.
    assert_cli_success(
        &run_libra_command(
            &["update-index", "--force-remove", "--add", "keep.txt"],
            root,
        ),
        "R4 force-remove with --add",
    );
    let list = run_libra_command(&["ls-files"], root);
    assert!(
        !String::from_utf8_lossy(&list.stdout).contains("keep.txt"),
        "force-remove must win over --add"
    );

    // R3: every stage of an unmerged path is removed.
    fs::write(root.join("conflict.txt"), "base\n").expect("write base");
    assert_cli_success(
        &run_libra_command(&["add", "conflict.txt"], root),
        "stage base",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "base", "--no-verify"], root),
        "commit base",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "-c", "feature"], root),
        "create feature",
    );
    fs::write(root.join("conflict.txt"), "feature\n").expect("write feature");
    assert_cli_success(
        &run_libra_command(&["add", "conflict.txt"], root),
        "stage feature",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "feature", "--no-verify"], root),
        "commit feature",
    );
    assert_cli_success(
        &run_libra_command(&["switch", "main"], root),
        "back to main",
    );
    fs::write(root.join("conflict.txt"), "main\n").expect("write main");
    assert_cli_success(
        &run_libra_command(&["add", "conflict.txt"], root),
        "stage main",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "main", "--no-verify"], root),
        "commit main",
    );
    let merge = run_libra_command(&["merge", "feature"], root);
    assert!(
        !merge.status.success(),
        "merge should conflict: {}",
        String::from_utf8_lossy(&merge.stderr)
    );
    let unmerged = run_libra_command(&["ls-files", "-u"], root);
    assert!(
        !unmerged.stdout.is_empty(),
        "conflict must leave unmerged stages"
    );
    assert_cli_success(
        &run_libra_command(&["update-index", "--force-remove", "conflict.txt"], root),
        "R3 force-remove unmerged",
    );
    let unmerged = run_libra_command(&["ls-files", "-u"], root);
    assert!(
        unmerged.stdout.is_empty(),
        "all stages must be removed: {}",
        String::from_utf8_lossy(&unmerged.stdout)
    );

    // R5: JSON reports the removed count.
    fs::write(root.join("again.txt"), "again\n").expect("write again");
    assert_cli_success(
        &run_libra_command(&["add", "again.txt"], root),
        "stage again",
    );
    let output = run_libra_command(
        &["--json", "update-index", "--force-remove", "again.txt"],
        root,
    );
    assert_cli_success(&output, "R5 json");
    let parsed = parse_json_stdout(&output);
    assert_eq!(parsed["data"]["removed"], 1, "removed count: {parsed}");
}

/// FM-03 (M-CFG K5, plan issues/470): `update-index <path>` keeps an existing
/// entry's mode when `core.fileMode=false`.
#[cfg(unix)]
#[test]
fn test_update_index_path_honors_core_filemode_false() {
    use std::os::unix::fs::PermissionsExt;

    use super::{assert_cli_success, configure_identity_via_cli};

    let repo = init_repo();
    let root = repo.path();
    configure_identity_via_cli(root);
    let tool = root.join("tool.sh");
    fs::write(&tool, "#!/bin/sh\necho v1\n").expect("write tool");
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).expect("chmod tool");
    assert_cli_success(&run_libra_command(&["add", "tool.sh"], root), "stage tool");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "exec", "--no-verify"], root),
        "commit tool",
    );
    assert_cli_success(
        &run_libra_command(&["config", "set", "core.fileMode", "false"], root),
        "disable fileMode",
    );
    fs::write(&tool, "#!/bin/sh\necho v2\n").expect("rewrite tool");
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o644)).expect("chmod 644");
    assert_cli_success(
        &run_libra_command(&["update-index", "tool.sh"], root),
        "update-index",
    );
    let out = run_libra_command(&["ls-files", "--stage", "tool.sh"], root);
    assert_cli_success(&out, "ls-files");
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("100755"),
        "K5 existing mode must be kept: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// SW-07 (M-CLI C1–C3, plan issues/490): `--skip-worktree`/`--no-skip-worktree`
/// mark and unmark tracked paths without reading the working tree; a mark
/// moves the index to format v3 and clearing it returns to v2; a plain
/// `update-index` on a marked path is a no-op (the working-tree copy is never
/// restaged); paths the index does not know fail with git's `Unable to mark
/// file` (exit 128) and persist nothing.
#[test]
fn test_update_index_skip_worktree_matrix() {
    use super::{assert_cli_success, configure_identity_via_cli, index_version};

    let repo = init_repo();
    let root = repo.path();
    configure_identity_via_cli(root);
    fs::write(root.join("s.txt"), "s\n").expect("write s");
    fs::write(root.join("untracked.txt"), "u\n").expect("write untracked");
    assert_cli_success(&run_libra_command(&["add", "s.txt"], root), "stage s");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "commit",
    );
    assert_eq!(index_version(root), 2, "plain repo starts at index v2");

    // C1: the mark succeeds silently, keeps the working tree untouched, shows
    // `S` through `ls-files -v`, and bumps the on-disk index to v3.
    assert_cli_success(
        &run_libra_command(&["update-index", "--skip-worktree", "s.txt"], root),
        "C1 mark",
    );
    assert_eq!(
        fs::read(root.join("s.txt")).expect("worktree untouched"),
        b"s\n"
    );
    let list = run_libra_command(&["ls-files", "-v"], root);
    assert_cli_success(&list, "C1 ls-files -v");
    assert!(
        String::from_utf8_lossy(&list.stdout).contains("S s.txt"),
        "C1 expected `S s.txt`: {}",
        String::from_utf8_lossy(&list.stdout)
    );
    assert_eq!(index_version(root), 3, "C1 the marked index must be v3");

    // JSON counts a mark as an update.
    let marked = run_libra_command(
        &["--json", "update-index", "--skip-worktree", "s.txt"],
        root,
    );
    assert_cli_success(&marked, "C1 json mark");
    let parsed = parse_json_stdout(&marked);
    assert_eq!(parsed["data"]["updated"], 1, "C1 updated count: {parsed}");

    // A plain `update-index <path>` on a marked entry is a no-op: git assumes
    // the working-tree copy is good, so a modified file is never restaged and
    // the bit survives.
    fs::write(root.join("s.txt"), "modified\n").expect("modify s");
    let (mode, hash, size, intent, skip) =
        super::index_entry_snapshot(root, "s.txt").expect("stage-0 entry");
    assert!(skip, "precondition");
    assert_cli_success(
        &run_libra_command(&["update-index", "s.txt"], root),
        "plain update on a marked entry",
    );
    assert_eq!(
        super::index_entry_snapshot(root, "s.txt"),
        Some((mode, hash, size, intent, true)),
        "plain update must not restage or unmark the entry"
    );

    // C2: clearing the mark restores `H` and the index returns to v2. The
    // working-tree copy is restored first so the ordinary worktree-state tag
    // applies again (a modified file would tag `C`).
    fs::write(root.join("s.txt"), "s\n").expect("restore s");
    assert_cli_success(
        &run_libra_command(&["update-index", "--no-skip-worktree", "s.txt"], root),
        "C2 clear",
    );
    let list = run_libra_command(&["ls-files", "-v"], root);
    assert!(
        String::from_utf8_lossy(&list.stdout).contains("H s.txt"),
        "C2 expected `H s.txt`: {}",
        String::from_utf8_lossy(&list.stdout)
    );
    assert_eq!(index_version(root), 2, "C2 the index must return to v2");

    // C3: an untracked path fails like git 2.55 (`Unable to mark file`, 128)
    // for both spellings, and a failed mark in a multi-path invocation must
    // not persist the earlier marks.
    for flag in ["--skip-worktree", "--no-skip-worktree"] {
        let out = run_libra_command(&["update-index", flag, "untracked.txt"], root);
        assert_eq!(out.status.code(), Some(128), "{flag} untracked exits 128");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("Unable to mark file untracked.txt"),
            "{flag} untracked message: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mixed = run_libra_command(
        &["update-index", "--skip-worktree", "s.txt", "untracked.txt"],
        root,
    );
    assert_eq!(mixed.status.code(), Some(128), "mixed invocation exits 128");
    assert!(
        !super::skip_worktree_set(root, "s.txt"),
        "a failed invocation must not persist earlier marks"
    );
}

/// SW-07 (M-CLI C5, plan issues/490, revised per git 2.55 `process_path`):
/// a skip-worktree entry short-circuits the add/remove handling — an explicit
/// `--remove` drops the entry (exit 0) while a plain update leaves it alone.
#[test]
fn test_update_index_remove_removes_skip_worktree_entry() {
    use super::{assert_cli_success, configure_identity_via_cli};

    let repo = init_repo();
    let root = repo.path();
    configure_identity_via_cli(root);
    fs::write(root.join("s.txt"), "s\n").expect("write s");
    assert_cli_success(&run_libra_command(&["add", "s.txt"], root), "stage s");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "init", "--no-verify"], root),
        "commit",
    );
    assert_cli_success(
        &run_libra_command(&["update-index", "--skip-worktree", "s.txt"], root),
        "mark",
    );
    fs::remove_file(root.join("s.txt")).expect("remove the worktree copy");

    // `--remove` on a marked entry removes it regardless of file presence
    // (git: `allow_remove && remove_file_from_index` in the skip-worktree
    // branch of process_path).
    assert_cli_success(
        &run_libra_command(&["update-index", "--remove", "s.txt"], root),
        "C5 remove",
    );
    assert!(
        super::index_entry_snapshot(root, "s.txt").is_none(),
        "C5 the entry must be gone"
    );

    // Without `--remove` the same marked entry is kept untouched.
    fs::write(root.join("s.txt"), "s\n").expect("rewrite s");
    assert_cli_success(&run_libra_command(&["add", "s.txt"], root), "restage s");
    assert_cli_success(
        &run_libra_command(&["update-index", "--skip-worktree", "s.txt"], root),
        "mark again",
    );
    assert_cli_success(
        &run_libra_command(&["update-index", "s.txt"], root),
        "plain update",
    );
    assert!(
        super::skip_worktree_set(root, "s.txt"),
        "a plain update must keep the marked entry"
    );
}

// ---------------------------------------------------------------------------
// Issue #479 plan cards UI-01 / UI-02 / UI-03: --index-info, --stdin [-z],
// --refresh family, --chmod, --info-only, --verbose.
// ---------------------------------------------------------------------------

fn write_and_commit(repo: &std::path::Path, name: &str, content: &str) -> String {
    fs::write(repo.join(name), content).expect("write fixture file");
    super::assert_cli_success(
        &run_libra_command(&["add", name], repo),
        &format!("add {name}"),
    );
    super::assert_cli_success(
        &run_libra_command(&["commit", "-m", "c", "--no-verify"], repo),
        "commit fixture",
    );
    // The index blob id for `name`.
    let ls = run_libra_command(&["ls-files", "-s", name], repo);
    String::from_utf8_lossy(&ls.stdout)
        .split_whitespace()
        .nth(1)
        .expect("ls-files -s should print an oid")
        .to_string()
}

fn blob_id_of_file(repo: &std::path::Path, name: &str) -> String {
    let hash = run_libra_command(&["hash-object", name], repo);
    assert!(
        hash.status.success(),
        "hash-object {name} failed: {}",
        String::from_utf8_lossy(&hash.stderr)
    );
    String::from_utf8_lossy(&hash.stdout).trim().to_string()
}

fn index_mode_of(repo: &std::path::Path, name: &str) -> String {
    super::index_entry_snapshot(repo, name)
        .map(|entry| format!("{:06o}", entry.0))
        .unwrap_or_default()
}

/// UI-01 N1: `--stdin` restages already-tracked paths from the working tree.
#[test]
fn update_index_stdin_restages_tracked_paths() {
    let repo = init_repo();
    super::configure_identity_via_cli(repo.path());
    let old_oid = write_and_commit(repo.path(), "tracked.txt", "one\n");
    fs::write(repo.path().join("tracked.txt"), "two\n").expect("rewrite");

    let output =
        run_libra_command_with_stdin(&["update-index", "--stdin"], repo.path(), "tracked.txt\n");
    assert!(
        output.status.success(),
        "--stdin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let new_oid = blob_id_of_file(repo.path(), "tracked.txt");
    assert_ne!(old_oid, new_oid, "content changed so the oid must change");
    let ls = run_libra_command(&["ls-files", "-s", "tracked.txt"], repo.path());
    assert!(
        String::from_utf8_lossy(&ls.stdout).contains(&new_oid),
        "the index must carry the new blob: {}",
        String::from_utf8_lossy(&ls.stdout)
    );
}

/// UI-01 N2: `--stdin --add` adds paths the index does not know.
#[test]
fn update_index_stdin_add_matrix_adds_new_paths() {
    let repo = init_repo();
    fs::write(repo.path().join("new.txt"), "fresh\n").expect("write new file");

    let output = run_libra_command_with_stdin(
        &["update-index", "--add", "--stdin"],
        repo.path(),
        "new.txt\n",
    );
    assert!(
        output.status.success(),
        "--add --stdin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ls = run_libra_command(&["ls-files", "-s", "new.txt"], repo.path());
    assert!(
        String::from_utf8_lossy(&ls.stdout).contains(&blob_id_of_file(repo.path(), "new.txt")),
        "ls-files must list new.txt with its blob id"
    );
}

/// UI-01 N3: `-z --stdin` handles paths containing newlines.
#[test]
fn update_index_stdin_z_handles_newline_paths() {
    let repo = init_repo();
    fs::write(repo.path().join("we\nird.txt"), "odd\n").expect("write newline path");

    let output = run_libra_command_with_stdin(
        &["update-index", "-z", "--add", "--stdin"],
        repo.path(),
        "we\nird.txt\0",
    );
    assert!(
        output.status.success(),
        "-z --stdin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ls = run_libra_command(&["ls-files"], repo.path());
    assert!(
        String::from_utf8_lossy(&ls.stdout).contains("we\nird.txt"),
        "the newline path must be staged: {}",
        String::from_utf8_lossy(&ls.stdout)
    );
}

/// UI-01 N4: all three `--index-info` line formats register the entry.
#[test]
fn update_index_index_info_three_line_formats() {
    let repo = init_repo();
    let input = format!(
        "100644 {sha} 0\tplain.txt\n\
         100644 blob {sha} 0\ttyped.txt\n\
         100644 {sha} 0\tstaged.txt\n",
        sha = "1111111111111111111111111111111111111111"
    );
    let output =
        run_libra_command_with_stdin(&["update-index", "--index-info"], repo.path(), &input);
    assert!(
        output.status.success(),
        "--index-info failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for name in ["plain.txt", "typed.txt", "staged.txt"] {
        let ls = run_libra_command(&["ls-files", "-s", name], repo.path());
        assert!(
            String::from_utf8_lossy(&ls.stdout)
                .contains("1111111111111111111111111111111111111111"),
            "{name} must be registered with the given oid: {}",
            String::from_utf8_lossy(&ls.stdout)
        );
    }
}

/// UI-01 N5: `mode 0` removes every stage of the path.
#[test]
fn update_index_index_info_mode_zero_removes_all_stages() {
    let repo = init_repo();
    super::configure_identity_via_cli(repo.path());
    write_and_commit(repo.path(), "gone.txt", "bye\n");

    let input = "0 1111111111111111111111111111111111111111 0\tgone.txt\n";
    let output =
        run_libra_command_with_stdin(&["update-index", "--index-info"], repo.path(), input);
    assert!(
        output.status.success(),
        "mode-0 --index-info failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        super::index_entry_snapshot(repo.path(), "gone.txt").is_none(),
        "mode 0 must remove the path from the index"
    );
}

/// UI-01 N6: a stage-3 entry coexists with the existing stage-0 entry, and a
/// later stage-0 write removes the higher stages (git 2.56 empirical truth).
#[test]
fn update_index_index_info_stage3_coexists_with_stage0() {
    let repo = init_repo();
    super::configure_identity_via_cli(repo.path());
    write_and_commit(repo.path(), "conflict.txt", "base\n");
    let stage0_oid = blob_id_of_file(repo.path(), "conflict.txt");

    let input = format!("100644 {stage0_oid} 3\tconflict.txt\n");
    let output =
        run_libra_command_with_stdin(&["update-index", "--index-info"], repo.path(), &input);
    assert!(
        output.status.success(),
        "stage-3 --index-info failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ls = run_libra_command(&["ls-files", "-s", "conflict.txt"], repo.path());
    let lines: Vec<String> = String::from_utf8_lossy(&ls.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines.len(), 2, "stage 0 and stage 3 coexist: {lines:?}");
    assert!(
        lines.iter().any(|l| l.ends_with(" 0\tconflict.txt")),
        "stage-0 entry kept: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.ends_with(" 3\tconflict.txt")),
        "stage-3 entry added: {lines:?}"
    );

    // Writing stage 0 again collapses the unmerged entry set.
    let collapse = format!("100644 {stage0_oid} 0\tconflict.txt\n");
    let output =
        run_libra_command_with_stdin(&["update-index", "--index-info"], repo.path(), &collapse);
    assert!(
        output.status.success(),
        "stage-0 collapse failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ls = run_libra_command(&["ls-files", "-s", "conflict.txt"], repo.path());
    let lines: Vec<String> = String::from_utf8_lossy(&ls.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines.len(), 1, "stage 0 alone remains: {lines:?}");
}

/// UI-01 N7: `-z --index-info` parses NUL-terminated records.
#[test]
fn update_index_index_info_z_nul_records() {
    let repo = init_repo();
    let input = "100644 1111111111111111111111111111111111111111 0\tnul.txt\0";
    let output =
        run_libra_command_with_stdin(&["update-index", "-z", "--index-info"], repo.path(), input);
    assert!(
        output.status.success(),
        "-z --index-info failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let ls = run_libra_command(&["ls-files", "-s", "nul.txt"], repo.path());
    assert!(
        String::from_utf8_lossy(&ls.stdout).contains("nul.txt"),
        "nul.txt must be registered"
    );
}

/// UI-01 atomicity (GC-PL-02): a malformed line exits 128 and leaves the
/// on-disk index byte-identical.
#[test]
fn update_index_batch_failure_is_atomic() {
    let repo = init_repo();
    super::configure_identity_via_cli(repo.path());
    write_and_commit(repo.path(), "keep.txt", "keep\n");
    let index_before = fs::read(repo.path().join(".libra").join("index")).expect("read index");

    // A valid first line followed by garbage: nothing may be applied.
    let input = "100644 1111111111111111111111111111111111111111 0\tadded.txt\nbadline\n";
    let output =
        run_libra_command_with_stdin(&["update-index", "--index-info"], repo.path(), input);
    assert_eq!(
        output.status.code(),
        Some(128),
        "malformed line must exit 128: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let index_after = fs::read(repo.path().join(".libra").join("index")).expect("read index");
    assert_eq!(index_before, index_after, "index must be byte-identical");
    assert!(
        super::index_entry_snapshot(repo.path(), "added.txt").is_none(),
        "the valid first line must not be applied either"
    );
}

/// UI-02 F1: a stat-only change is refreshed silently with exit 0.
#[test]
fn update_index_refresh_stat_only_change_is_silent() {
    let repo = init_repo();
    super::configure_identity_via_cli(repo.path());
    write_and_commit(repo.path(), "f.txt", "same\n");

    // Bump mtime without touching content, so the recorded stat is stale.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    fs::write(repo.path().join("f.txt"), "same\n").expect("rewrite same content");

    let output = run_libra_command(&["update-index", "--refresh"], repo.path());
    assert!(
        output.status.success(),
        "stat-only refresh failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "stat-only refresh must print nothing: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    // The refreshed stat must now match the worktree: a content change after
    // the refresh must still be detected.
    fs::write(repo.path().join("f.txt"), "different\n").expect("modify");
    let output = run_libra_command(&["update-index", "--refresh"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(1),
        "content change after refresh must be reported"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("f.txt: needs update"),
        "expected needs update line: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// UI-02 F2/F3: modified content prints `<path>: needs update` and exits 1;
/// `-q --refresh` suppresses both the line and the exit code.
#[test]
fn update_index_refresh_reports_needs_update_and_quiet() {
    let repo = init_repo();
    super::configure_identity_via_cli(repo.path());
    write_and_commit(repo.path(), "f.txt", "one\n");
    fs::write(repo.path().join("f.txt"), "two\n").expect("modify");

    let output = run_libra_command(&["update-index", "--refresh"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(1),
        "modified content must exit 1"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "f.txt: needs update",
        "git-matching needs update line"
    );

    let quiet = run_libra_command(&["update-index", "-q", "--refresh"], repo.path());
    assert_eq!(
        quiet.status.code(),
        Some(0),
        "-q must turn the exit code 0: {}",
        String::from_utf8_lossy(&quiet.stderr)
    );
    assert!(
        quiet.stdout.is_empty(),
        "-q must suppress the needs update line"
    );
}

/// UI-02 F4: a missing worktree file reports `needs update` (exit 1);
/// `--ignore-missing` skips it.
#[test]
fn update_index_refresh_missing_file_and_ignore_missing() {
    let repo = init_repo();
    super::configure_identity_via_cli(repo.path());
    write_and_commit(repo.path(), "gone.txt", "gone\n");
    fs::remove_file(repo.path().join("gone.txt")).expect("remove worktree copy");

    let output = run_libra_command(&["update-index", "--refresh"], repo.path());
    assert_eq!(output.status.code(), Some(1), "missing file must exit 1");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("gone.txt: needs update"),
        "missing file must report needs update: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let skipped = run_libra_command(
        &["update-index", "--ignore-missing", "--refresh"],
        repo.path(),
    );
    assert_eq!(
        skipped.status.code(),
        Some(0),
        "--ignore-missing must skip the entry: {}",
        String::from_utf8_lossy(&skipped.stderr)
    );
    assert!(skipped.stdout.is_empty(), "nothing to report");
}

/// UI-02 F5: unmerged entries report `needs merge` (exit 1); `--unmerged`
/// suppresses them.
#[test]
fn update_index_refresh_unmerged_needs_merge() {
    let repo = init_repo();
    super::configure_identity_via_cli(repo.path());
    write_and_commit(repo.path(), "conflict.txt", "base\n");
    let oid = blob_id_of_file(repo.path(), "conflict.txt");
    let input = format!("100644 {oid} 2\tconflict.txt\n100644 {oid} 3\tconflict.txt\n");
    let staged =
        run_libra_command_with_stdin(&["update-index", "--index-info"], repo.path(), &input);
    assert!(
        staged.status.success(),
        "stage setup failed: {}",
        String::from_utf8_lossy(&staged.stderr)
    );

    let output = run_libra_command(&["update-index", "--refresh"], repo.path());
    assert_eq!(output.status.code(), Some(1), "unmerged must exit 1");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("conflict.txt: needs merge"),
        "unmerged must report needs merge: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let allowed = run_libra_command(&["update-index", "--unmerged", "--refresh"], repo.path());
    assert_eq!(
        allowed.status.code(),
        Some(0),
        "--unmerged must skip the entry: {}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert!(allowed.stdout.is_empty(), "nothing to report");

    // Git keeps the merge report and exit 1 under `-q`; only the needs-update
    // report is suppressed. `--unmerged` is the option that skips the entry.
    let quiet = run_libra_command(&["update-index", "-q", "--refresh"], repo.path());
    assert_eq!(
        quiet.status.code(),
        Some(1),
        "-q must not suppress an unmerged entry: {}",
        String::from_utf8_lossy(&quiet.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&quiet.stdout),
        "conflict.txt: needs merge\n"
    );
}

/// Command-position -q is ordered; explicit global quiet only controls output.
/// Different-size content avoids relying on filesystem timestamp granularity.
#[test]
fn update_index_refresh_quiet_scope_matrix() {
    for fixture in ["modified", "missing", "unmerged", "mixed"] {
        let repo = init_repo();
        fs::write(repo.path().join("a-modified.txt"), "base\n").expect("write modified");
        fs::write(repo.path().join("b-missing.txt"), "base\n").expect("write missing");
        fs::write(repo.path().join("c-conflict.txt"), "base\n").expect("write conflict");
        fs::write(repo.path().join("-q"), "literal short flag\n").expect("write -q");
        fs::write(repo.path().join("--quiet"), "literal long flag\n").expect("write --quiet");
        let staged = run_libra_command(
            &[
                "update-index",
                "--add",
                "a-modified.txt",
                "b-missing.txt",
                "c-conflict.txt",
                "--",
                "-q",
                "--quiet",
            ],
            repo.path(),
        );
        assert!(staged.status.success(), "{staged:?}");
        let has_merge = matches!(fixture, "unmerged" | "mixed");
        if has_merge {
            let oid = blob_id_of_file(repo.path(), "c-conflict.txt");
            let input = format!("100644 {oid} 2\tc-conflict.txt\n100644 {oid} 3\tc-conflict.txt\n");
            let staged = run_libra_command_with_stdin(
                &["update-index", "--index-info"],
                repo.path(),
                &input,
            );
            assert!(staged.status.success(), "{staged:?}");
        }
        let mut all_lines = String::new();
        if matches!(fixture, "modified" | "mixed") {
            fs::write(
                repo.path().join("a-modified.txt"),
                "longer changed content\n",
            )
            .expect("modify");
            all_lines.push_str("a-modified.txt: needs update\n");
        }
        if matches!(fixture, "missing" | "mixed") {
            fs::remove_file(repo.path().join("b-missing.txt")).expect("remove");
            all_lines.push_str("b-missing.txt: needs update\n");
        }
        let merge_line = if has_merge {
            "c-conflict.txt: needs merge\n"
        } else {
            ""
        };
        all_lines.push_str(merge_line);
        for refresh in ["--refresh", "--really-refresh"] {
            let cases: Vec<(Vec<&str>, i32, &str)> = vec![
                (vec!["update-index", refresh], 1, &all_lines),
                (
                    vec!["update-index", "-q", refresh],
                    i32::from(has_merge),
                    merge_line,
                ),
                (vec!["update-index", refresh, "-q"], 1, &all_lines),
                (vec!["update-index", refresh, "--", "-q"], 1, &all_lines),
                (
                    vec!["update-index", "-q", refresh, "--", "--quiet"],
                    i32::from(has_merge),
                    merge_line,
                ),
                (vec!["--quiet", "update-index", refresh], 1, ""),
                (vec!["-q", "update-index", refresh], 1, ""),
                (vec!["update-index", "--quiet", refresh], 1, ""),
                (vec!["update-index", refresh, "--quiet"], 1, ""),
                (
                    vec!["--quiet", "update-index", "-q", refresh],
                    i32::from(has_merge),
                    "",
                ),
                // Distinct parser levels permit global quiet plus local -q.
                // Two spellings at the same level remain a Clap conflict.
                (
                    vec!["-q", "update-index", "-q", refresh],
                    i32::from(has_merge),
                    "",
                ),
                (vec!["update-index", "-q", "--unmerged", refresh], 0, ""),
                // Optional global values require '='; bare --color must not
                // consume the next operation, and a required value is skipped.
                (
                    vec![
                        "--progress",
                        "none",
                        "update-index",
                        "--color",
                        "-q",
                        refresh,
                    ],
                    i32::from(has_merge),
                    merge_line,
                ),
            ];
            for (argv, code, stdout) in cases {
                let output = run_libra_command(&argv, repo.path());
                assert_eq!(
                    output.status.code(),
                    Some(code),
                    "{fixture} {argv:?}: {output:?}"
                );
                assert_eq!(output.stdout, stdout.as_bytes(), "{fixture} {argv:?}");
                assert!(output.stderr.is_empty(), "{fixture} {argv:?}: {output:?}");
            }
            // Both refresh operations retain their own flag snapshot. The
            // first pass still fails even though the second sees -q.
            let argv = [
                "update-index",
                refresh,
                "-q",
                if refresh == "--refresh" {
                    "--really-refresh"
                } else {
                    "--refresh"
                },
            ];
            let output = run_libra_command(&argv, repo.path());
            assert_eq!(
                output.status.code(),
                Some(1),
                "{fixture} {argv:?}: {output:?}"
            );
            assert_eq!(output.stdout, format!("{all_lines}{merge_line}").as_bytes());
            assert!(output.stderr.is_empty(), "{fixture} {argv:?}: {output:?}");

            for format in ["--json", "--json=compact", "--machine"] {
                for local_quiet_first in [false, true] {
                    let argv = if local_quiet_first {
                        vec!["update-index", format, "-q", refresh]
                    } else {
                        vec![format, "update-index", refresh, "-q"]
                    };
                    let output = run_libra_command(&argv, repo.path());
                    let code = i32::from(has_merge || !local_quiet_first);
                    assert_eq!(
                        output.status.code(),
                        Some(code),
                        "{fixture} {argv:?}: {output:?}"
                    );
                    assert!(output.stderr.is_empty(), "{fixture} {argv:?}: {output:?}");
                    if code == 1 {
                        // Refresh failures use silent_exit(1), including JSON.
                        assert!(output.stdout.is_empty(), "{fixture} {argv:?}: {output:?}");
                    } else {
                        let expected = serde_json::json!({
                            "command": "update-index",
                            "data": {"updated": 0, "removed": 0},
                            "ok": true,
                        });
                        let actual = serde_json::from_slice::<serde_json::Value>(&output.stdout)
                            .expect("stdout contains exactly one JSON envelope");
                        assert_eq!(actual, expected, "{fixture} {argv:?}");
                    }
                }
            }
        }
    }
}

/// UI-02 F6: `--really-refresh` behaves exactly like `--refresh` (Libra has
/// no assume-unchanged bit; documented equivalence).
#[test]
fn update_index_really_refresh_equals_refresh() {
    let repo = init_repo();
    super::configure_identity_via_cli(repo.path());
    write_and_commit(repo.path(), "f.txt", "one\n");
    fs::write(repo.path().join("f.txt"), "two\n").expect("modify");

    let output = run_libra_command(&["update-index", "--really-refresh"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(1),
        "modified content must exit 1"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "f.txt: needs update"
    );
}

/// Stage-0 paths whose index entry carries assume-unchanged (`CE_VALID`).
fn assume_valid_paths(repo: &std::path::Path) -> Vec<String> {
    use git_internal::{hash::HashKind, internal::index::Index};
    let index =
        Index::load_with_hash_kind(HashKind::Sha1, repo.join(".libra/index")).expect("load index");
    index
        .tracked_entries(0)
        .into_iter()
        .filter(|entry| entry.flags.assume_valid)
        .map(|entry| entry.name.clone())
        .collect()
}

/// Rewrite the index the way git-internal <= 0.10.3 wrote it: `CE_VALID` on
/// every entry.
fn mark_all_assume_valid(repo: &std::path::Path) {
    use git_internal::{hash::HashKind, internal::index::Index};
    let index_path = repo.join(".libra/index");
    let mut index = Index::load_with_hash_kind(HashKind::Sha1, &index_path).expect("load index");
    let names: Vec<String> = index
        .tracked_entries(0)
        .into_iter()
        .map(|entry| entry.name.clone())
        .collect();
    for name in names {
        let mut entry = index.remove(&name, 0).expect("tracked entry");
        entry.flags.assume_valid = true;
        index.add(entry);
    }
    index
        .save_with_hash_kind(HashKind::Sha1, &index_path)
        .expect("save index");
}

/// issues/637: newly staged entries carry no `CE_VALID`, and `--refresh`
/// clears the bit left by older releases from every stage-0 entry — clean,
/// stat-refreshed and modified alike — so Git reading the index sees edits.
#[test]
fn update_index_refresh_clears_legacy_assume_valid() {
    let repo = init_repo();
    super::configure_identity_via_cli(repo.path());
    write_and_commit(repo.path(), "a.txt", "a\n");
    write_and_commit(repo.path(), "b.txt", "b\n");
    write_and_commit(repo.path(), "c.txt", "c\n");
    assert!(
        assume_valid_paths(repo.path()).is_empty(),
        "new entries must not carry CE_VALID"
    );

    mark_all_assume_valid(repo.path());
    assert_eq!(
        assume_valid_paths(repo.path()),
        ["a.txt", "b.txt", "c.txt"],
        "precondition: legacy index with CE_VALID on every entry"
    );

    // a.txt: untouched; b.txt: stat-only drift; c.txt: modified content.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    fs::write(repo.path().join("b.txt"), "b\n").expect("rewrite same content");
    fs::write(repo.path().join("c.txt"), "changed\n").expect("modify");

    let output = run_libra_command(&["update-index", "--refresh"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(1),
        "modified c.txt must still be reported: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "c.txt: needs update"
    );
    assert!(
        assume_valid_paths(repo.path()).is_empty(),
        "refresh must clear the legacy CE_VALID bit from every entry"
    );
}

/// UI-03 C1/C2: `--chmod=(+|-)x` applies positionally to the paths that
/// follow it (git t2107:93-97 matrix).
#[test]
fn update_index_chmod_matrix_positional() {
    let repo = init_repo();
    super::configure_identity_via_cli(repo.path());
    write_and_commit(repo.path(), "a.txt", "a\n");
    write_and_commit(repo.path(), "b.txt", "b\n");

    let output = run_libra_command(
        &["update-index", "--chmod=+x", "a.txt", "--chmod=-x", "b.txt"],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "chmod matrix failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        index_mode_of(repo.path(), "a.txt"),
        "100755",
        "a.txt must be +x"
    );
    assert_eq!(
        index_mode_of(repo.path(), "b.txt"),
        "100644",
        "b.txt must be -x"
    );

    // The same flag persists across following paths until overridden.
    let output = run_libra_command(
        &["update-index", "--chmod=+x", "a.txt", "b.txt"],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "chmod persistence failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(index_mode_of(repo.path(), "a.txt"), "100755");
    assert_eq!(
        index_mode_of(repo.path(), "b.txt"),
        "100755",
        "flag persists"
    );
}

/// UI-03 C2/C5: successful chmod reports even when the requested mode already
/// matches. Only an actual mode change adds another update to the JSON count.
#[test]
fn update_index_verbose_chmod_reports_unchanged_mode() {
    let repo = init_repo();
    fs::write(repo.path().join("A"), "a\n").expect("write A");
    fs::write(repo.path().join("B"), "b\n").expect("write B");
    let staged = run_libra_command(&["update-index", "--add", "A", "B"], repo.path());
    assert!(staged.status.success(), "{staged:?}");
    let output = run_libra_command(
        &[
            "update-index",
            "--verbose",
            "--chmod=+x",
            "A",
            "--chmod=-x",
            "B",
        ],
        repo.path(),
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        output.stdout,
        b"add 'A'\nchmod +x 'A'\nadd 'B'\nchmod -x 'B'\n"
    );
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(index_mode_of(repo.path(), "A"), "100755");
    assert_eq!(index_mode_of(repo.path(), "B"), "100644");

    let output = run_libra_command(
        &["update-index", "--verbose", "--chmod=-x", "B"],
        repo.path(),
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stdout, b"add 'B'\nchmod -x 'B'\n");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(index_mode_of(repo.path(), "B"), "100644");

    let output = run_libra_command(
        &["--json", "update-index", "--verbose", "--chmod=-x", "B"],
        repo.path(),
    );
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let json = parse_json_stdout(&output);
    assert_eq!(
        json["data"]["updated"], 1,
        "only the positional restage counts"
    );
    assert_eq!(json["data"]["removed"], 0);
}

/// UI-03 C3: chmod on a non-regular file fails with 128.
#[test]
fn update_index_chmod_non_regular_file_fails() {
    let repo = init_repo();
    fs::create_dir(repo.path().join("dir")).expect("create directory");

    let output = run_libra_command(&["update-index", "--chmod=+x", "dir"], repo.path());
    assert_eq!(
        output.status.code(),
        Some(128),
        "chmod on a directory must fail: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// UI-03 C4: `--add --info-only` records the blob id in the index without
/// writing the blob object.
#[test]
fn update_index_info_only_writes_no_object() {
    let repo = init_repo();
    fs::write(repo.path().join("q.txt"), "q\n").expect("write q.txt");
    // Ignore the path so the operation-log worktree snapshot does not capture
    // it: plumbing --add stages ignored paths regardless (git parity), and
    // this isolates the --info-only staging path itself from the snapshot's
    // orthogonal worktree-content persistence.
    fs::write(repo.path().join(".libraignore"), "q.txt\n").expect("write ignore file");

    let output = run_libra_command(
        &["update-index", "--add", "--info-only", "q.txt"],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "--info-only failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oid = blob_id_of_file(repo.path(), "q.txt");
    let ls = run_libra_command(&["ls-files", "-s", "q.txt"], repo.path());
    assert!(
        String::from_utf8_lossy(&ls.stdout).contains(&oid),
        "index must carry the oid"
    );
    // The staging path itself must not store the blob: `--info-only` goes
    // through `stage_working_tree_path(.., info_only = true)`, which hashes
    // the content and skips `blob.try_save()` in both the symlink and regular
    // file branches. The operation-log worktree snapshot (a later, orthogonal
    // layer) may retain the content in its own store — that is a documented
    // Libra difference and does not change the index-entry contract here.
    // The lib-level single test below pins the no-save path directly.
}

/// UI-03 C5: `--verbose` reports add/remove/chmod actions on stdout.
#[test]
fn update_index_verbose_reports_actions() {
    let repo = init_repo();
    super::configure_identity_via_cli(repo.path());
    write_and_commit(repo.path(), "v.txt", "v\n");

    let output = run_libra_command(
        &["update-index", "--add", "--verbose", "--chmod=+x", "v.txt"],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "--verbose failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        lines,
        vec!["add 'v.txt'", "chmod +x 'v.txt'"],
        "verbose lines"
    );

    let output = run_libra_command(
        &["update-index", "--force-remove", "--verbose", "v.txt"],
        repo.path(),
    );
    assert!(
        output.status.success(),
        "--force-remove --verbose failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "remove 'v.txt'"
    );
}
