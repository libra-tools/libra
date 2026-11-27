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
    let system_dir = temp.path().join("system-dir");
    fs::create_dir_all(&system_dir).unwrap();
    let unreadable_sys = system_dir.join("unreadable_sys.db");
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
    let system_dir = temp.path().join("system-dir");
    fs::create_dir_all(&system_dir).unwrap();
    let unreadable = system_dir.join("unreadable.db");
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
    let system_dir = temp.path().join("system-dir");
    fs::create_dir_all(&system_dir).unwrap();
    let missing = system_dir.join("missing-system.db");
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
    let system_dir = temp.path().join("system-dir");
    fs::create_dir_all(&system_dir).unwrap();
    let unreadable = system_dir.join("unreadable.db");
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

#[test]
#[serial(cwd)]
fn test_cf_27_predicates() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");
    for pattern in ["(", "["] {
        let out = run_libra_command(&["config", "--get-regexp", pattern], repo.path());
        assert_eq!(out.status.code(), Some(6), "{pattern}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("invalid key pattern"), "{stderr}");
        assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
    }
}

#[test]
#[serial(cwd)]
fn test_cf_28_predicates() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");
    for flag in ["--get", "--get-all"] {
        let out = run_libra_command(&["config", flag, "nosuchkey"], repo.path());
        assert_eq!(out.status.code(), Some(1), "{flag}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("key does not contain a section"),
            "{stderr}"
        );
        assert!(stderr.contains("LBR-CLI-002"), "{stderr}");
    }
}

#[test]
#[serial(cwd)]
fn test_cf_30_predicates() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");
    for args in [
        vec!["config", "--get-all", "no.such"],
        vec!["config", "get", "--all", "no.such"],
    ] {
        let out = run_libra_command(&args, repo.path());
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-003"),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
#[serial(cwd)]
fn test_config_key_without_section_spellings() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");
    for args in [
        vec!["config", "--unset", "nosuchkey"],
        vec!["config", "nosuchkey", "value"],
        vec!["config", "set", "nosuchkey", "v"],
        vec!["config", "unset", "nosuchkey"],
    ] {
        let out = run_libra_command(&args, repo.path());
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("key does not contain a section"),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
#[serial(cwd)]
fn test_config_validation_exit_codes_matrix() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");
    assert_cli_success(
        &run_libra_command(&["config", "--local", "user.name", "Test"], repo.path()),
        "seed",
    );
    for pattern in ["^user", "user", "name$"] {
        let out = run_libra_command(&["config", "--get-regexp", pattern], repo.path());
        assert_cli_success(&out, pattern);
    }
    let invalid = run_libra_command(&["config", "--get-regexp", "("], repo.path());
    assert_eq!(invalid.status.code(), Some(6));
    let undotted = run_libra_command(&["config", "--get", "nosuchkey"], repo.path());
    assert_eq!(undotted.status.code(), Some(1));
}

#[test]
#[serial(cwd)]
fn test_get_regexp_reserved_rule_scoped() {
    let repo = tempdir().unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault=false"], repo.path()),
        "init",
    );
    // The reserved-namespace matcher only applies to the global scope.
    let local = run_libra_command(
        &["config", "--local", "--get-regexp", "^upgrade\\."],
        repo.path(),
    );
    assert_eq!(local.status.code(), Some(1), "local regexp has no matches");
    let global = run_libra_command(
        &["config", "--global", "--get-regexp", "^upgrade\\."],
        repo.path(),
    );
    assert_eq!(
        global.status.code(),
        Some(129),
        "global reserved regexp is rejected"
    );
}

#[test]
#[serial(cwd)]
fn test_config_validation_json_envelope() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");
    let undotted = run_libra_command(&["--json", "config", "--get", "nosuchkey"], repo.path());
    assert_eq!(undotted.status.code(), Some(1));
    let parsed: serde_json::Value = serde_json::from_slice(&undotted.stderr).expect("json");
    assert_eq!(parsed["exit_code"], 1);
    assert_eq!(parsed["error_code"], "LBR-CLI-002");

    for pattern in ["(", "["] {
        let invalid =
            run_libra_command(&["--json", "config", "--get-regexp", pattern], repo.path());
        assert_eq!(invalid.status.code(), Some(6));
        let parsed: serde_json::Value = serde_json::from_slice(&invalid.stderr).expect("json");
        assert_eq!(parsed["error_code"], "LBR-CLI-002");
        assert_eq!(parsed["exit_code"], 6);
    }
}

#[test]
#[serial(cwd)]
fn test_config_already_compatible_surface() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");
    assert_cli_success(
        &run_libra_command(&["config", "--local", "surface.k", "v"], repo.path()),
        "set",
    );
    let get = run_libra_command(&["config", "--get", "surface.k"], repo.path());
    assert_eq!(String::from_utf8_lossy(&get.stdout).trim(), "v");
    let list = run_libra_command(&["config", "--local", "--list"], repo.path());
    assert!(String::from_utf8_lossy(&list.stdout).contains("surface.k=v"));
}

#[test]
#[serial(cwd)]
fn test_config_missing_key_unset_and_reserved_regexp_scope() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");
    let before = run_libra_command(&["config", "--local", "--list"], repo.path());
    for flag in ["--unset", "--unset-all"] {
        let out = run_libra_command(&["config", flag, "no.such"], repo.path());
        assert_eq!(out.status.code(), Some(5), "{flag}");
        assert!(out.stdout.is_empty(), "{flag} no success line");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-003"),
            "{flag}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let after = run_libra_command(&["config", "--local", "--list"], repo.path());
        assert_eq!(before.stdout, after.stdout, "{flag} writes nothing");
    }
    let local = run_libra_command(
        &["config", "--local", "--get-regexp", "^upgrade\\."],
        repo.path(),
    );
    assert_eq!(local.status.code(), Some(1));
    let global = run_libra_command(
        &["config", "--global", "--get-regexp", "^upgrade\\."],
        repo.path(),
    );
    assert_eq!(global.status.code(), Some(129));
}

#[test]
#[serial(cwd)]
fn test_cf_32_predicates() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");
    let before = run_libra_command(&["config", "--local", "--list"], repo.path());
    for args in [
        vec!["config", "unset", "no.such"],
        vec!["config", "unset", "--all", "no.such"],
    ] {
        let out = run_libra_command(&args, repo.path());
        assert_eq!(out.status.code(), Some(5), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("LBR-CLI-003"),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let after = run_libra_command(&["config", "--local", "--list"], repo.path());
        assert_eq!(before.stdout, after.stdout, "{args:?}");
    }
}

#[test]
#[serial(cwd, env)]
fn test_cf_33_predicates() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(&run_libra_command(&["init"], &repo), "init");
    assert_cli_success(
        &run_libra_command(&["config", "--local", "env.key", "v"], &repo),
        "local set",
    );
    let local = run_libra_command(&["config", "--local", "--get-regexp", "e"], &repo);
    assert_cli_success(&local, "local regexp");
    let local_miss = run_libra_command(&["config", "--local", "--get-regexp", "nomatch"], &repo);
    assert_eq!(local_miss.status.code(), Some(1));

    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let xdg = home.join("xdg");
    let global_db = temp.path().join("global-dir").join("config.db");
    fs::create_dir_all(global_db.parent().unwrap()).unwrap();
    let global_envs = [
        ("HOME", home.to_str().unwrap()),
        ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
        ("LIBRA_CONFIG_GLOBAL_DB", global_db.to_str().unwrap()),
    ];
    let global = run_libra_command_with_env(
        &["config", "--global", "--get-regexp", "upgrade."],
        &repo,
        &global_envs,
    );
    assert_eq!(global.status.code(), Some(129));
    assert!(String::from_utf8_lossy(&global.stderr).contains("LBR-CLI-002"));

    let system_repo = temp.path().join("system-repo");
    fs::create_dir_all(&system_repo).unwrap();
    assert_cli_success(&run_libra_command(&["init"], &system_repo), "system init");
    let system_db = temp.path().join("system-dir").join("config.db");
    fs::create_dir_all(system_db.parent().unwrap()).unwrap();
    let system_envs = [
        ("HOME", home.to_str().unwrap()),
        ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
        ("LIBRA_CONFIG_SYSTEM_DB", system_db.to_str().unwrap()),
    ];
    assert_cli_success(
        &run_libra_command_with_env(
            &["config", "--system", "env.key", "system"],
            &system_repo,
            &system_envs,
        ),
        "system set",
    );
    let system = run_libra_command_with_env(
        &["config", "--system", "--get-regexp", "."],
        &system_repo,
        &system_envs,
    );
    assert_cli_success(&system, "system regexp");
    let system_miss = run_libra_command_with_env(
        &["config", "--system", "--get-regexp", "nomatch"],
        &system_repo,
        &system_envs,
    );
    assert_eq!(system_miss.status.code(), Some(1));
}

#[test]
#[serial(cwd)]
fn test_cf_37_predicates() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");
    for value in ["one", "two"] {
        assert_cli_success(
            &run_libra_command(&["config", "--add", "reg.multi", value], repo.path()),
            "add",
        );
    }
    let get = run_libra_command(&["config", "--get", "reg.multi"], repo.path());
    assert_eq!(String::from_utf8_lossy(&get.stdout).trim(), "two");
    let all = run_libra_command(&["config", "--get-all", "reg.multi"], repo.path());
    assert_eq!(String::from_utf8_lossy(&all.stdout).trim(), "one\ntwo");
    let null = run_libra_command(&["config", "-z", "--get", "reg.multi"], repo.path());
    assert_eq!(null.stdout, b"two\0");
    let null_list = run_libra_command(&["config", "--local", "-z", "--list"], repo.path());
    assert!(
        null_list
            .stdout
            .windows(b"reg.multi\none\0".len())
            .any(|window| window == b"reg.multi\none\0")
    );
    let json = run_libra_command(
        &["--json", "config", "--unset-all", "reg.multi"],
        repo.path(),
    );
    assert_cli_success(&json, "unset-all json");
    let parsed: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json");
    assert_eq!(parsed["data"]["removed_count"], 2);

    for (key, value) in [("sec.a", "1"), ("sec.b", "2"), ("ren.a", "3")] {
        assert_cli_success(
            &run_libra_command(&["config", "--local", key, value], repo.path()),
            "seed",
        );
    }
    assert_cli_success(
        &run_libra_command(&["config", "--remove-section", "sec"], repo.path()),
        "remove section",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--rename-section", "ren", "ren2"], repo.path()),
        "rename section",
    );
    let list = run_libra_command(&["config", "--local", "--list"], repo.path());
    let text = String::from_utf8_lossy(&list.stdout);
    assert!(!text.contains("sec."), "{text}");
    assert!(text.contains("ren2.a=3"), "{text}");

    assert_cli_success(
        &run_libra_command(&["config", "--local", "typed.bool", "yes"], repo.path()),
        "bool seed",
    );
    let bool_out = run_libra_command(
        &["config", "--local", "--bool", "--get", "typed.bool"],
        repo.path(),
    );
    assert_eq!(String::from_utf8_lossy(&bool_out.stdout).trim(), "true");
    assert_cli_success(
        &run_libra_command(&["config", "--local", "typed.int", "1k"], repo.path()),
        "int seed",
    );
    let int_out = run_libra_command(
        &["config", "--local", "--int", "--get", "typed.int"],
        repo.path(),
    );
    assert_eq!(String::from_utf8_lossy(&int_out.stdout).trim(), "1024");
}
