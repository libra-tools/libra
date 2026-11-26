//! CF-07 (M-SYS Y1..Y7): Explicit scope missing or unreadable store tests.

#[cfg(unix)]
use std::fs;

#[cfg(unix)]
use serial_test::serial;
#[cfg(unix)]
use tempfile::tempdir;

#[cfg(unix)]
use crate::command::{assert_cli_success, run_libra_command, run_libra_command_with_env};

/// CF-07 (M-SYS Y1..Y7): Test that explicit scope read operations correctly handle
/// missing and unreadable stores, matching Git semantics.
#[cfg(unix)]
#[test]
#[serial(cwd, env)]
fn test_explicit_scope_missing_or_unreadable_store() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo),
        "init repo",
    );
    assert_cli_success(
        &run_libra_command(&["config", "user.name", "local-user"], &repo),
        "set local user.name",
    );

    let nonexistent_sys = temp.path().join("nonexistent_sys.db");
    let nonexistent_sys_str = nonexistent_sys.to_str().unwrap();

    // ─────────────────────────────────────────────────────────────────────────
    // Y1: system store missing -> --system --get and --system --get-regexp exit 1, no stderr
    // ─────────────────────────────────────────────────────────────────────────
    let out_get_missing = run_libra_command_with_env(
        &["config", "--system", "--get", "x.y"],
        &repo,
        &[("LIBRA_CONFIG_SYSTEM_DB", nonexistent_sys_str)],
    );
    assert_eq!(out_get_missing.status.code(), Some(1));
    assert!(
        out_get_missing.stderr.is_empty(),
        "missing system store on --get must emit no stderr: {:?}",
        String::from_utf8_lossy(&out_get_missing.stderr)
    );

    let out_re_missing = run_libra_command_with_env(
        &["config", "--system", "--get-regexp", "."],
        &repo,
        &[("LIBRA_CONFIG_SYSTEM_DB", nonexistent_sys_str)],
    );
    assert_eq!(out_re_missing.status.code(), Some(1));
    assert!(
        out_re_missing.stderr.is_empty(),
        "missing system store on --get-regexp must emit no stderr"
    );

    // With --default, returns default and exits 0
    let out_def_missing = run_libra_command_with_env(
        &[
            "config",
            "--system",
            "--default",
            "fallback",
            "--get",
            "x.y",
        ],
        &repo,
        &[("LIBRA_CONFIG_SYSTEM_DB", nonexistent_sys_str)],
    );
    assert_cli_success(&out_def_missing, "--default on missing store");
    assert_eq!(
        String::from_utf8_lossy(&out_def_missing.stdout).trim(),
        "fallback"
    );

    // ─────────────────────────────────────────────────────────────────────────
    // Y2: system store missing -> --system --list exits 128 with LBR-IO-001
    // ─────────────────────────────────────────────────────────────────────────
    let out_list_missing = run_libra_command_with_env(
        &["config", "--system", "--list"],
        &repo,
        &[("LIBRA_CONFIG_SYSTEM_DB", nonexistent_sys_str)],
    );
    assert_eq!(out_list_missing.status.code(), Some(128));
    let stderr_list_missing = String::from_utf8_lossy(&out_list_missing.stderr);
    assert!(
        stderr_list_missing.contains("unable to read config file"),
        "expected unable to read config file, got: {stderr_list_missing}"
    );
    assert!(
        stderr_list_missing.contains("No such file or directory"),
        "expected No such file or directory: {stderr_list_missing}"
    );
    assert!(
        stderr_list_missing.contains("LBR-IO-001"),
        "expected LBR-IO-001: {stderr_list_missing}"
    );

    // ─────────────────────────────────────────────────────────────────────────
    // Y3: system store unreadable -> --system --get emits warning to stderr and exits 1
    // ─────────────────────────────────────────────────────────────────────────
    let unreadable_sys = temp.path().join("unreadable_sys.db");
    fs::write(&unreadable_sys, b"placeholder").unwrap();
    fs::set_permissions(&unreadable_sys, fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable_sys_str = unreadable_sys.to_str().unwrap();

    let out_get_unreadable = run_libra_command_with_env(
        &["config", "--system", "--get", "x.y"],
        &repo,
        &[("LIBRA_CONFIG_SYSTEM_DB", unreadable_sys_str)],
    );
    assert_eq!(out_get_unreadable.status.code(), Some(1));
    let stderr_get_unreadable = String::from_utf8_lossy(&out_get_unreadable.stderr);
    assert!(
        stderr_get_unreadable.contains("warning: unable to access"),
        "expected warning: unable to access, got: {stderr_get_unreadable}"
    );
    assert!(
        stderr_get_unreadable.contains("Permission denied"),
        "expected Permission denied: {stderr_get_unreadable}"
    );

    // ─────────────────────────────────────────────────────────────────────────
    // Y4: system store unreadable -> --system --list exits 128 with LBR-IO-001
    // ─────────────────────────────────────────────────────────────────────────
    let out_list_unreadable = run_libra_command_with_env(
        &["config", "--system", "--list"],
        &repo,
        &[("LIBRA_CONFIG_SYSTEM_DB", unreadable_sys_str)],
    );
    assert_eq!(out_list_unreadable.status.code(), Some(128));
    let stderr_list_unreadable = String::from_utf8_lossy(&out_list_unreadable.stderr);
    assert!(
        stderr_list_unreadable.contains("unable to access"),
        "expected unable to access, got: {stderr_list_unreadable}"
    );
    assert!(
        stderr_list_unreadable.contains("Permission denied"),
        "expected Permission denied: {stderr_list_unreadable}"
    );
    assert!(
        stderr_list_unreadable.contains("LBR-IO-001"),
        "expected LBR-IO-001: {stderr_list_unreadable}"
    );

    // ─────────────────────────────────────────────────────────────────────────
    // Y5: system store unreadable during cascaded get skips system store (exits 0)
    // ─────────────────────────────────────────────────────────────────────────
    let out_cascaded = run_libra_command_with_env(
        &["config", "get", "user.name"],
        &repo,
        &[("LIBRA_CONFIG_SYSTEM_DB", unreadable_sys_str)],
    );
    assert_cli_success(&out_cascaded, "cascaded get with unreadable system store");
    assert_eq!(
        String::from_utf8_lossy(&out_cascaded.stdout).trim(),
        "local-user"
    );

    // Restore permissions for cleanup
    let _ = fs::set_permissions(&unreadable_sys, fs::Permissions::from_mode(0o644));

    // ─────────────────────────────────────────────────────────────────────────
    // Y6: global store missing -> --global --get exits 1; --global --list exits 128
    // ─────────────────────────────────────────────────────────────────────────
    let nonexistent_global = temp.path().join("nonexistent_global.db");
    let nonexistent_global_str = nonexistent_global.to_str().unwrap();

    let out_global_get = run_libra_command_with_env(
        &["config", "--global", "--get", "x.y"],
        &repo,
        &[("LIBRA_CONFIG_GLOBAL_DB", nonexistent_global_str)],
    );
    assert_eq!(out_global_get.status.code(), Some(1));
    assert!(
        out_global_get.stderr.is_empty(),
        "missing global store on --get must emit no stderr: {:?}",
        String::from_utf8_lossy(&out_global_get.stderr)
    );

    let out_global_list = run_libra_command_with_env(
        &["config", "--global", "--list"],
        &repo,
        &[("LIBRA_CONFIG_GLOBAL_DB", nonexistent_global_str)],
    );
    assert_eq!(out_global_list.status.code(), Some(128));
    let stderr_global_list = String::from_utf8_lossy(&out_global_list.stderr);
    assert!(
        stderr_global_list.contains("unable to read config file"),
        "expected unable to read config file: {stderr_global_list}"
    );
    assert!(
        stderr_global_list.contains("No such file or directory"),
        "expected No such file or directory: {stderr_global_list}"
    );
    assert!(
        stderr_global_list.contains("LBR-IO-001"),
        "expected LBR-IO-001: {stderr_global_list}"
    );
}

#[test]
#[serial(cwd)]
fn test_cf_104_predicates() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");
    assert_cli_success(
        &run_libra_command(&["config", "x.path", "/tmp/example"], repo.path()),
        "set path",
    );
    let path = run_libra_command(&["config", "--get", "--path", "x.path"], repo.path());
    assert_cli_success(&path, "--path");
    assert_eq!(String::from_utf8_lossy(&path.stdout).trim(), "/tmp/example");

    let default = run_libra_command(
        &["config", "--get", "--default", "fallback", "x.missing"],
        repo.path(),
    );
    assert_cli_success(&default, "--default");
    assert_eq!(String::from_utf8_lossy(&default.stdout).trim(), "fallback");
}

#[test]
#[serial(cwd)]
fn test_cf_26_predicates() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");
    let out = run_libra_command(&["config", "--get-regexp", "^nomatch"], repo.path());
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "no-match stdout must be empty");
}

#[test]
#[serial(cwd, env)]
fn test_cf_59_predicates() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(&run_libra_command(&["init"], &repo), "init");
    assert_cli_success(
        &run_libra_command(&["config", "user.name", "local-user"], &repo),
        "set local user.name",
    );
    let unreadable = temp.path().join("unreadable.db");
    fs::write(&unreadable, b"placeholder").unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
    let out = run_libra_command_with_env(
        &["config", "get", "user.name"],
        &repo,
        &[("LIBRA_CONFIG_SYSTEM_DB", unreadable.to_str().unwrap())],
    );
    assert_cli_success(&out, "cascade skips unreadable system store");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "local-user");
    let _ = fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644));
}

#[test]
#[serial(cwd, env)]
fn test_cf_60_predicates() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(&run_libra_command(&["init"], &repo), "init");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let xdg = home.join("xdg");
    let envs = [
        ("HOME", home.to_str().unwrap()),
        ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
    ];
    let list = run_libra_command_with_env(&["config", "--global", "--list"], &repo, &envs);
    assert_eq!(list.status.code(), Some(128), "missing global store list");
    assert!(String::from_utf8_lossy(&list.stderr).contains("unable to read config file"));
    let get = run_libra_command_with_env(&["config", "--global", "--get", "x.y"], &repo, &envs);
    assert_eq!(get.status.code(), Some(1), "missing global store get");
    assert!(get.stdout.is_empty());
}

#[test]
#[serial(cwd, env)]
fn test_cf_61_predicates() {
    use std::process::Command;
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(&run_libra_command(&["init"], &repo), "init");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let xdg = home.join("xdg");
    let global = temp.path().join("global.db");
    let system = temp.path().join("system.db");
    let envs = [
        ("HOME", home.to_str().unwrap()),
        ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
        ("LIBRA_CONFIG_GLOBAL_DB", global.to_str().unwrap()),
        ("LIBRA_CONFIG_SYSTEM_DB", system.to_str().unwrap()),
    ];
    for scope in ["--global", "--system"] {
        assert_cli_success(
            &run_libra_command_with_env(&["config", scope, "user.name", "X"], &repo, &envs),
            "initialize store",
        );
    }
    let query = |db: &std::path::Path, sql: &str| {
        Command::new("sqlite3")
            .arg(db)
            .arg(sql)
            .output()
            .expect("sqlite3 available")
    };
    let make_future = |db: &std::path::Path| {
        let out = query(db, "SELECT MAX(version) FROM configuration_schema_versions");
        let latest: i64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        let future = latest + 1;
        let sql = format!(
            "INSERT INTO configuration_schema_versions(version,name,applied_at) VALUES({future},'future','x')"
        );
        assert!(query(db, &sql).status.success());
    };
    for (scope, db) in [("--global", &global), ("--system", &system)] {
        make_future(db.as_path());
        let out = run_libra_command_with_env(&["config", scope, "--list"], &repo, &envs);
        assert_eq!(out.status.code(), Some(128), "{scope} future schema");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("schema is newer"), "{scope}: {stderr}");
        assert!(stderr.contains("LBR-INTERNAL-001"), "{scope}: {stderr}");
        fs::remove_file(db).unwrap();
        fs::write(db, b"not a sqlite database").unwrap();
        let out = run_libra_command_with_env(&["config", scope, "--list"], &repo, &envs);
        assert_eq!(out.status.code(), Some(128), "{scope} corrupt");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("LBR-IO-001"), "{scope}: {stderr}");
        assert!(!stderr.contains("schema is newer"), "{scope}: {stderr}");
    }
}

#[test]
#[serial(cwd)]
fn test_cf_29_predicates() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");
    let out = run_libra_command(&["config", "get", "nosuchkey"], repo.path());
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("key does not contain a section"),
        "{stderr}"
    );
    assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
}

#[test]
#[serial(cwd)]
fn test_cf_31_predicates() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");
    let out = run_libra_command(&["config", "--get", "no.such"], repo.path());
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-003"));
}

#[test]
#[serial(cwd)]
fn test_cf_36_predicates() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");
    let ok = run_libra_command(
        &["--json", "config", "--local", "user.name", "X"],
        repo.path(),
    );
    assert_cli_success(&ok, "json success");
    let parsed: serde_json::Value = serde_json::from_slice(&ok.stdout).expect("json");
    assert_eq!(parsed["exit_code"], 0, "success exit_code: {parsed}");

    let err = run_libra_command(&["--json", "config", "get", "nosuchkey"], repo.path());
    assert_eq!(err.status.code(), Some(1));
    let parsed: serde_json::Value = serde_json::from_slice(&err.stderr).expect("json error");
    assert_eq!(parsed["error_code"], "LBR-CLI-002");
    assert_eq!(parsed["exit_code"], 1);
}

#[test]
#[serial(cwd, env)]
fn test_cf_57_predicates() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(&run_libra_command(&["init"], &repo), "init");
    let missing = temp.path().join("missing-system.db");
    let out = run_libra_command_with_env(
        &["config", "--system", "--list"],
        &repo,
        &[("LIBRA_CONFIG_SYSTEM_DB", missing.to_str().unwrap())],
    );
    assert_eq!(out.status.code(), Some(128));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unable to read config file"), "{stderr}");
    assert!(stderr.contains("LBR-IO-001"), "{stderr}");
}

#[test]
#[serial(cwd, env)]
fn test_cf_58_predicates() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(&run_libra_command(&["init"], &repo), "init");
    let unreadable = temp.path().join("unreadable.db");
    fs::write(&unreadable, b"placeholder").unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
    let out = run_libra_command_with_env(
        &["config", "--system", "--get", "x.y"],
        &repo,
        &[("LIBRA_CONFIG_SYSTEM_DB", unreadable.to_str().unwrap())],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("unable to access"), "{stderr}");
    let list = run_libra_command_with_env(
        &["config", "--system", "--list"],
        &repo,
        &[("LIBRA_CONFIG_SYSTEM_DB", unreadable.to_str().unwrap())],
    );
    assert_eq!(list.status.code(), Some(128), "unreadable list");
    assert!(String::from_utf8_lossy(&list.stderr).contains("LBR-IO-001"));
    let _ = fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644));
}

#[test]
#[serial(cwd)]
fn test_cf_107_predicates() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");
    let before = run_libra_command(&["config", "--local", "--list"], repo.path());
    for args in [
        vec!["config", "--unset", "nosuchkey"],
        vec!["config", "nosuchkey", "value"],
        vec!["config", "set", "nosuchkey", "v"],
        vec!["config", "unset", "nosuchkey"],
    ] {
        let _ = run_libra_command(&args, repo.path());
        let after = run_libra_command(&["config", "--local", "--list"], repo.path());
        assert_eq!(before.stdout, after.stdout, "zero write: {args:?}");
    }
}
