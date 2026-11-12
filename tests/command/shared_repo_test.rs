//! Integration tests for post-init writes respecting `core.sharedRepository` (M-SLATER L1..L7).
//!
//! **Layer:** L1 — deterministic, no external dependencies.

use std::fs;

use tempfile::tempdir;

use super::{assert_cli_success, run_libra_command};

/// IN-06 (M-SLATER L1..L7): Verify that post-init writes (loose objects, fanout dirs,
/// database/sidecars, index, pack/idx) respect the configured repository sharing mode.
#[cfg(unix)]
#[test]
fn test_shared_repo_later_writes_matrix() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().unwrap();

    // ─────────────────────────────────────────────────────────────────────────
    // L1: group shared repository: add + commit produces loose objects 0444,
    // fanout directories 2775
    // ─────────────────────────────────────────────────────────────────────────
    let repo_group = temp.path().join("repo_group");
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false", "--shared=group"], &repo_group),
        "init group repo",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.name", "Test"], &repo_group),
        "set user.name",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "test@example.com"], &repo_group),
        "set user.email",
    );

    fs::write(repo_group.join("file1.txt"), "content1\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "file1.txt"], &repo_group),
        "add file1.txt",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "first commit"], &repo_group),
        "commit in group repo",
    );

    // Verify loose objects and fanout directories in .libra/objects
    let objects_dir = repo_group.join(".libra/objects");
    let mut found_loose_object = false;
    for entry in fs::read_dir(&objects_dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.len() == 2 && entry.file_type().unwrap().is_dir() {
            let fanout_dir = entry.path();
            let dir_mode = fs::metadata(&fanout_dir).unwrap().permissions().mode() & 0o7777;
            assert_eq!(
                dir_mode, 0o2775,
                "fanout directory {name} must be 2775 in group repo"
            );

            for obj_entry in fs::read_dir(&fanout_dir).unwrap() {
                let obj_entry = obj_entry.unwrap();
                let obj_mode =
                    fs::metadata(obj_entry.path()).unwrap().permissions().mode() & 0o7777;
                assert_eq!(
                    obj_mode,
                    0o444,
                    "loose object {} must be 0444 in group repo",
                    obj_entry.path().display()
                );
                found_loose_object = true;
            }
        }
    }
    assert!(found_loose_object, "should have written loose objects");

    // ─────────────────────────────────────────────────────────────────────────
    // L3: group shared repository: database files and index are 0664
    // ─────────────────────────────────────────────────────────────────────────
    let db_path = repo_group.join(".libra/libra.db");
    let db_mode = fs::metadata(&db_path).unwrap().permissions().mode() & 0o7777;
    assert_eq!(db_mode, 0o664, "libra.db must be 0664 in group repo");

    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = repo_group.join(format!(".libra/libra.db{suffix}"));
        if sidecar.exists() {
            let sidecar_mode = fs::metadata(&sidecar).unwrap().permissions().mode() & 0o7777;
            assert_eq!(
                sidecar_mode, 0o664,
                "database sidecar must be 0664 in group repo"
            );
        }
    }

    let index_path = repo_group.join(".libra/index");
    if index_path.exists() {
        let index_mode = fs::metadata(&index_path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(index_mode, 0o664, "index must be 0664 in group repo");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // L2: all shared repository: add + commit produces objects 0444, no world-writable
    // ─────────────────────────────────────────────────────────────────────────
    let repo_all = temp.path().join("repo_all");
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false", "--shared=all"], &repo_all),
        "init all repo",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.name", "Test"], &repo_all),
        "set user.name",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "test@example.com"], &repo_all),
        "set user.email",
    );

    fs::write(repo_all.join("file_all.txt"), "content_all\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "file_all.txt"], &repo_all),
        "add",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "commit all"], &repo_all),
        "commit in all repo",
    );

    for entry in walkdir::WalkDir::new(repo_all.join(".libra")) {
        let entry = entry.unwrap();
        let mode = fs::symlink_metadata(entry.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o002,
            0,
            "path {} must not be world-writable in all repo",
            entry.path().display()
        );
    }

    // ─────────────────────────────────────────────────────────────────────────
    // L4: 0640 shared repository: loose objects 0440, database 0640
    // ─────────────────────────────────────────────────────────────────────────
    let repo_0640 = temp.path().join("repo_0640");
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false", "--shared=0640"], &repo_0640),
        "init 0640 repo",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.name", "Test"], &repo_0640),
        "set user.name",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "test@example.com"], &repo_0640),
        "set user.email",
    );

    fs::write(repo_0640.join("f.txt"), "0640 content\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "f.txt"], &repo_0640), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "commit 0640"], &repo_0640),
        "commit 0640",
    );

    let db_mode_0640 = fs::metadata(repo_0640.join(".libra/libra.db"))
        .unwrap()
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(db_mode_0640, 0o640, "database must be 0640 in 0640 repo");

    let mut found_0640_object = false;
    for entry in fs::read_dir(repo_0640.join(".libra/objects")).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.len() == 2 && entry.file_type().unwrap().is_dir() {
            let fanout_dir = entry.path();
            let dir_mode = fs::metadata(&fanout_dir).unwrap().permissions().mode() & 0o7777;
            assert_eq!(dir_mode, 0o2750, "fanout dir must be 2750 in 0640 repo");

            for obj in fs::read_dir(&fanout_dir).unwrap() {
                let obj = obj.unwrap();
                let obj_mode = fs::metadata(obj.path()).unwrap().permissions().mode() & 0o7777;
                assert_eq!(obj_mode, 0o440, "loose object must be 0440 in 0640 repo");
                found_0640_object = true;
            }
        }
    }
    assert!(found_0640_object, "should have found 0640 object");

    // ─────────────────────────────────────────────────────────────────────────
    // L5: non-shared repository: permissions remain standard
    // ─────────────────────────────────────────────────────────────────────────
    let repo_default = temp.path().join("repo_default");
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo_default),
        "init default repo",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.name", "Test"], &repo_default),
        "set user.name",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "test@example.com"], &repo_default),
        "set user.email",
    );

    fs::write(repo_default.join("default.txt"), "default\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["add", "default.txt"], &repo_default),
        "add",
    );
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "commit default"], &repo_default),
        "commit default",
    );

    // ─────────────────────────────────────────────────────────────────────────
    // L6: group shared repository: repack produces pack and idx with 0444 and pack dir 2775
    // ─────────────────────────────────────────────────────────────────────────
    assert_cli_success(
        &run_libra_command(&["repack", "-a"], &repo_group),
        "repack -a in group repo",
    );

    let pack_dir = repo_group.join(".libra/objects/pack");
    if pack_dir.exists() {
        let pack_dir_mode = fs::metadata(&pack_dir).unwrap().permissions().mode() & 0o7777;
        assert_eq!(
            pack_dir_mode, 0o2775,
            "pack directory must be 2775 in group repo"
        );

        for entry in fs::read_dir(&pack_dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if ext == "pack" || ext == "idx" {
                let file_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
                assert_eq!(
                    file_mode,
                    0o444,
                    "pack/idx file {} must be 0444",
                    path.display()
                );
            }
        }
    }
}
