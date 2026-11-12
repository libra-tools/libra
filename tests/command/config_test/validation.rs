//! CF-07 (M-SYS Y1..Y7): Explicit scope missing or unreadable store tests.

use std::fs;

use serial_test::serial;
use tempfile::tempdir;

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
