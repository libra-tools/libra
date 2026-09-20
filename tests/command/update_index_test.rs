//! Integration tests for `libra update-index`.
//!
//! Layer: L1 (deterministic; tempdir + isolated HOME, no network).

use std::fs;

use tempfile::tempdir;

use super::{parse_cli_error_stderr, parse_json_stdout, run_libra_command};

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
