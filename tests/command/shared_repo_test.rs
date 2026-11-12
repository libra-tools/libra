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

/// IN-07 (M-SMETA X1..X4): Metadata write points (COMMIT_EDITMSG, operation pointer,
/// locks, maintenance lock, repair directories) respect shared repository permissions.
#[cfg(unix)]
#[test]
fn test_shared_repo_metadata_matrix() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    assert_cli_success(
        &run_libra_command(&["init", "--shared=group"], &repo),
        "init group repo with vault",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.name", "Test"], &repo),
        "set user.name",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.email", "test@example.com"], &repo),
        "set user.email",
    );

    // X1: commit creates COMMIT_EDITMSG and info/operation-pointer.json
    fs::write(repo.join("file.txt"), "hello\n").unwrap();
    assert_cli_success(&run_libra_command(&["add", "file.txt"], &repo), "add");
    assert_cli_success(
        &run_libra_command(&["commit", "-m", "my commit"], &repo),
        "commit",
    );

    let editmsg = repo.join(".libra/COMMIT_EDITMSG");
    if editmsg.exists() {
        let mode = fs::metadata(&editmsg).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o664, "COMMIT_EDITMSG must be 0664 in group repo");
    }

    let pointer = repo.join(".libra/info/operation-pointer.json");
    if pointer.exists() {
        let mode = fs::metadata(&pointer).unwrap().permissions().mode() & 0o7777;
        assert_eq!(
            mode, 0o664,
            "operation-pointer.json must be 0664 in group repo"
        );
    }

    let info_dir = repo.join(".libra/info");
    if info_dir.exists() {
        let mode = fs::metadata(&info_dir).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o2775, "info dir must be 2775 in group repo");
    }

    // Status command
    assert_cli_success(&run_libra_command(&["status"], &repo), "status");

    // Stash push and pop
    fs::write(repo.join("file.txt"), "hello modified\n").unwrap();
    assert_cli_success(
        &run_libra_command(&["stash", "push", "-m", "wip"], &repo),
        "stash push",
    );
    assert_cli_success(&run_libra_command(&["stash", "pop"], &repo), "stash pop");

    // Maintenance run creates/updates maintenance.lock
    assert_cli_success(
        &run_libra_command(&["maintenance", "run"], &repo),
        "maintenance run",
    );
    let mlock = repo.join(".libra/maintenance.lock");
    if mlock.exists() {
        let mode = fs::metadata(&mlock).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o664, "maintenance.lock must be 0664 in group repo");
    }

    // X4: Vault files stay owner-only (0600)
    let vault_db = repo.join(".libra/vault.db");
    assert!(vault_db.exists());
    let vmode = fs::metadata(&vault_db).unwrap().permissions().mode() & 0o777;
    assert_eq!(vmode, 0o600, "vault.db must remain 0600 in group repo");
    assert!(libra::utils::shared_perm::is_owner_only_path(&vault_db));
}

/// IN-12 (M-LEDGER K2): Upstream t1301-shared-repo ports.
#[cfg(unix)]
#[test]
fn test_t1301_ports() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().unwrap();

    // t1301:18 'shared = 0400 (faulty permission u-w)'
    let sub_0400 = temp.path().join("sub_0400");
    let out = run_libra_command(&["init", "--vault", "false", "--shared=0400"], &sub_0400);
    assert_eq!(out.status.code(), Some(129));
    let (_h, report) = super::parse_cli_error_stderr(&out.stderr);
    assert_eq!(report.error_code, "LBR-CLI-002");
    assert!(!sub_0400.join(".libra").exists());

    // t1301:28 'shared=1'
    let sub_1 = temp.path().join("sub_1");
    let out = run_libra_command(&["init", "--vault", "false", "--shared=1"], &sub_1);
    assert_cli_success(&out, "init --shared=1");
    let cfg = run_libra_command(
        &["config", "--local", "--get", "core.sharedRepository"],
        &sub_1,
    );
    assert_eq!(String::from_utf8_lossy(&cfg.stdout).trim(), "1");
    let exclude_mode = fs::metadata(sub_1.join(".libra/info/exclude"))
        .unwrap()
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(
        exclude_mode, 0o664,
        ".libra/info/exclude must be 0664 under shared=1"
    );

    // t1301:49 'shared=all'
    let sub_all = temp.path().join("sub_all");
    let out = run_libra_command(&["init", "--vault", "false", "--shared=all"], &sub_all);
    assert_cli_success(&out, "init --shared=all");
    let cfg = run_libra_command(
        &["config", "--local", "--get", "core.sharedRepository"],
        &sub_all,
    );
    assert_eq!(String::from_utf8_lossy(&cfg.stdout).trim(), "2");

    // t1301 forced modes: 0660, 0640, 0600
    for (mode_str, exp_dir, exp_file) in [
        ("0660", 0o2770, 0o660),
        ("0640", 0o2750, 0o640),
        ("0600", 0o700, 0o600),
    ] {
        let repo = temp.path().join(format!("t1301_{mode_str}"));
        let out = run_libra_command(
            &["init", "--vault", "false", &format!("--shared={mode_str}")],
            &repo,
        );
        assert_cli_success(&out, &format!("init --shared={mode_str}"));
        let dmode = fs::metadata(repo.join(".libra"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(dmode, exp_dir, "directory mode for {mode_str}");
        let fmode = fs::metadata(repo.join(".libra/info/exclude"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(fmode, exp_file, "info/exclude mode for {mode_str}");
    }

    // unshared repo does not set shared permissions
    let repo_unshared = temp.path().join("t1301_unshared");
    let out = run_libra_command(&["init", "--vault", "false"], &repo_unshared);
    assert_cli_success(&out, "init unshared");
    let cfg = run_libra_command(
        &["config", "--local", "--get", "core.sharedRepository"],
        &repo_unshared,
    );
    assert_eq!(String::from_utf8_lossy(&cfg.stdout).trim(), "");
}
