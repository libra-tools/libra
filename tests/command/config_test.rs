//! Tests config command read/write behaviors, scope handling, and edge cases.
//!
//! **Layer:** L1 — deterministic, no external dependencies.
//
use std::process::Command;

use clap::Parser;
use libra::{CliErrorKind, CliResult, command::config, utils::output::OutputConfig};
use serial_test::serial;
use tempfile::tempdir;

use super::*;

/// Guard for temporarily setting an environment variable during a test and restoring it on drop.
///
/// # Safety
/// Modifying environment variables is process-global state. These tests are all annotated with
/// `#[serial]`, ensuring no concurrent mutation happens across tests.
struct EnvVarGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

async fn exec_config(args: Vec<&str>) -> CliResult<()> {
    config::execute_safe(
        config::ConfigArgs::parse_from(args),
        &OutputConfig::default(),
    )
    .await
}

#[tokio::test]
#[serial]
async fn test_cli_config_global_without_repo() {
    let temp_dir = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(temp_dir.path());

    let global_db_dir = tempdir().unwrap();
    let _scoped = ScopedConfigPathGuard::new(&global_db_dir.path().join("global_config_cli.db"));

    let result = exec_config(vec!["config", "--global", "user.name", "cli_global_user"]).await;
    assert!(result.is_ok());

    let read_result = exec_config(vec!["config", "--global", "--get", "user.name"]).await;
    assert!(read_result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_cli_config_list_global_without_repo() {
    let temp_dir = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(temp_dir.path());

    let global_db_dir = tempdir().unwrap();
    let _scoped =
        ScopedConfigPathGuard::new(&global_db_dir.path().join("global_config_cli_list.db"));

    let result = exec_config(vec!["config", "--list", "--global"]).await;
    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_cli_config_system_read_write() {
    let temp_dir = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(temp_dir.path());

    // Point the system scope at a temp DB so the test never touches /etc/libra.
    let system_db_dir = tempdir().unwrap();
    let _system = EnvVarGuard::set(
        "LIBRA_CONFIG_SYSTEM_DB",
        system_db_dir
            .path()
            .join("system_config_cli.db")
            .as_os_str(),
    );

    // --system writes and reads back (no repository required, like --global).
    let result = exec_config(vec!["config", "--system", "user.name", "cli_system_user"]).await;
    assert!(result.is_ok(), "--system set should succeed: {result:?}");

    let read_result = exec_config(vec!["config", "--system", "--get", "user.name"]).await;
    assert!(read_result.is_ok(), "--system --get should succeed");

    let list_result = exec_config(vec!["config", "--list", "--system"]).await;
    assert!(list_result.is_ok(), "--system --list should succeed");
}

#[tokio::test]
#[serial]
async fn test_config_cascade_system_is_lowest_precedence() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Pin both global and system scopes to writable temp DBs.
    let global_db = temp_path
        .path()
        .join("glob.db")
        .to_string_lossy()
        .to_string();
    let system_db = temp_path
        .path()
        .join("sys.db")
        .to_string_lossy()
        .to_string();
    let env: [(&str, &str); 2] = [
        ("LIBRA_CONFIG_GLOBAL_DB", global_db.as_str()),
        ("LIBRA_CONFIG_SYSTEM_DB", system_db.as_str()),
    ];

    // System-only value resolves via the cascade (local/global have no key).
    let set_sys = run_libra_command_with_stdin_and_env(
        &["config", "--system", "custom.scopetest", "from-system"],
        temp_path.path(),
        "",
        &env,
    );
    assert!(set_sys.status.success(), "set --system");
    let get = run_libra_command_with_stdin_and_env(
        &["config", "--get", "custom.scopetest"],
        temp_path.path(),
        "",
        &env,
    );
    assert!(
        String::from_utf8_lossy(&get.stdout).contains("from-system"),
        "cascade resolves to the system value: {}",
        String::from_utf8_lossy(&get.stdout)
    );

    // A global value of the same key overrides system (global > system).
    let set_glob = run_libra_command_with_stdin_and_env(
        &["config", "--global", "custom.scopetest", "from-global"],
        temp_path.path(),
        "",
        &env,
    );
    assert!(set_glob.status.success(), "set --global");
    let get2 = run_libra_command_with_stdin_and_env(
        &["config", "--get", "custom.scopetest"],
        temp_path.path(),
        "",
        &env,
    );
    assert!(
        String::from_utf8_lossy(&get2.stdout).contains("from-global"),
        "global overrides system in the cascade: {}",
        String::from_utf8_lossy(&get2.stdout)
    );
}

#[tokio::test]
#[serial]
async fn test_cli_config_local_requires_repo() {
    let temp_dir = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(temp_dir.path());

    let result = exec_config(vec!["config", "--local", "--list"]).await;
    let err = result.unwrap_err();
    assert_eq!(err.kind(), CliErrorKind::Fatal);
    assert!(err.message().contains("not a libra repository"));
}

#[tokio::test]
#[serial]
async fn test_config_system_scope_roundtrip_and_vault_rejection() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Redirect the system scope to a writable temp DB (never touch /etc/libra).
    let sys_db = temp_path.path().join("sys").join("config.db");
    let sys_db_str = sys_db.to_string_lossy().to_string();
    let env: [(&str, &str); 1] = [("LIBRA_CONFIG_SYSTEM_DB", sys_db_str.as_str())];

    // `--system` set then get roundtrips through the system DB.
    let set = run_libra_command_with_stdin_and_env(
        &["config", "--system", "user.name", "sys user"],
        temp_path.path(),
        "",
        &env,
    );
    assert!(
        set.status.success(),
        "--system set: {}",
        String::from_utf8_lossy(&set.stderr)
    );
    assert!(sys_db.exists(), "the system config DB was created");

    let get = run_libra_command_with_stdin_and_env(
        &["config", "--system", "--get", "user.name"],
        temp_path.path(),
        "",
        &env,
    );
    assert!(get.status.success(), "--system --get should succeed");
    assert!(
        String::from_utf8_lossy(&get.stdout).contains("sys user"),
        "system value read back: {}",
        String::from_utf8_lossy(&get.stdout)
    );

    // Vault-encrypted secrets are not supported in the system scope.
    let vault = run_libra_command_with_stdin_and_env(
        &[
            "config",
            "set",
            "--system",
            "--encrypt",
            "custom.secret",
            "s3cr3t",
        ],
        temp_path.path(),
        "",
        &env,
    );
    assert!(
        !vault.status.success(),
        "--system --encrypt must be rejected"
    );
    assert!(
        String::from_utf8_lossy(&vault.stderr).contains("not supported in --system scope"),
        "vault-rejection message: {}",
        String::from_utf8_lossy(&vault.stderr)
    );
}

#[tokio::test]
#[serial]
async fn test_config_import_global_from_git() {
    let temp_dir = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(temp_dir.path());

    let global_db_dir = tempdir().unwrap();
    let _scoped = ScopedConfigPathGuard::new(&global_db_dir.path().join("global_config_import.db"));

    let fake_home = tempdir().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", fake_home.path().as_os_str());
    let _xdg_guard = EnvVarGuard::set(
        "XDG_CONFIG_HOME",
        fake_home.path().join(".config").as_os_str(),
    );

    let set_name = Command::new("git")
        .args(["config", "--global", "user.name", "Git Global Import User"])
        .output()
        .unwrap();
    assert!(set_name.status.success());

    let set_email = Command::new("git")
        .args([
            "config",
            "--global",
            "user.email",
            "git-global-import@example.com",
        ])
        .output()
        .unwrap();
    assert!(set_email.status.success());

    let result = exec_config(vec!["config", "--global", "import"]).await;
    assert!(result.is_ok());

    let imported_name = config::ScopedConfig::get(config::ConfigScope::Global, "user.name")
        .await
        .unwrap();
    let imported_email = config::ScopedConfig::get(config::ConfigScope::Global, "user.email")
        .await
        .unwrap();
    assert_eq!(
        imported_name.map(|e| e.value).as_deref(),
        Some("Git Global Import User")
    );
    assert_eq!(
        imported_email.map(|e| e.value).as_deref(),
        Some("git-global-import@example.com")
    );
}

#[tokio::test]
#[serial]
async fn test_config_import_local_from_git_repository() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    use libra::internal::config::ConfigKv;
    ConfigKv::unset_all("user.name").await.unwrap();
    ConfigKv::unset_all("user.email").await.unwrap();

    let git_init = Command::new("git").args(["init"]).output().unwrap();
    assert!(git_init.status.success());

    let set_name = Command::new("git")
        .args(["config", "user.name", "Git Local Import User"])
        .output()
        .unwrap();
    assert!(set_name.status.success());

    let set_email = Command::new("git")
        .args(["config", "user.email", "git-local-import@example.com"])
        .output()
        .unwrap();
    assert!(set_email.status.success());

    let result = exec_config(vec!["config", "import"]).await;
    assert!(result.is_ok());

    let imported_names: Vec<String> =
        config::ScopedConfig::get_all(config::ConfigScope::Local, "user.name")
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.value)
            .collect();
    let imported_emails: Vec<String> =
        config::ScopedConfig::get_all(config::ConfigScope::Local, "user.email")
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.value)
            .collect();
    assert!(imported_names.iter().any(|v| v == "Git Local Import User"));
    assert!(
        imported_emails
            .iter()
            .any(|v| v == "git-local-import@example.com")
    );
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &std::ffi::OsStr) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: test is #[serial], so no concurrent env access/mutation across tests.
        unsafe { std::env::set_var(key, value) };
        Self { key, original }
    }

    fn unset(key: &'static str) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: test is #[serial], so no concurrent env access/mutation across tests.
        unsafe { std::env::remove_var(key) };
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: test is #[serial], so no concurrent env access/mutation across tests.
        match &self.original {
            Some(v) => unsafe { std::env::set_var(self.key, v) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

/// Sets `LIBRA_CONFIG_GLOBAL_DB` to point at a temp file for isolation.
///
/// This prevents tests from touching real host paths like `~/.libra/config.db`.
struct ScopedConfigPathGuard {
    _global: EnvVarGuard,
}

impl ScopedConfigPathGuard {
    fn new(global_db_path: &std::path::Path) -> Self {
        let _global = EnvVarGuard::set("LIBRA_CONFIG_GLOBAL_DB", global_db_path.as_os_str());
        Self { _global }
    }
}

#[tokio::test]
#[serial]
async fn test_config_get_failed() {
    let temp_path = tempdir().unwrap();
    // start a new libra repository in a temporary directory
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // --default with --add (no --get or --get-all) should error
    let result = exec_config(vec![
        "config",
        "--add",
        "-d",
        "erasernoob",
        "user.name",
        "value",
    ])
    .await;
    assert!(result.is_err());
}

#[tokio::test]
#[serial]
async fn test_config_get_all() {
    let temp_path = tempdir().unwrap();
    // start a new libra repository in a temporary directory
    test::setup_with_new_libra_in(temp_path.path()).await;

    // set the current working directory to the temporary path
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Add the config first
    let result = exec_config(vec!["config", "--add", "user.name", "erasernoob"]).await;
    assert!(result.is_ok());

    let result = exec_config(vec!["config", "--get", "user.name"]).await;
    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_config_get_all_with_default() {
    let temp_path = tempdir().unwrap();
    let global_db_dir = tempdir().unwrap();
    let _scoped = ScopedConfigPathGuard::new(
        &global_db_dir
            .path()
            .join("global_config_get_all_default.db"),
    );

    // start a new libra repository in a temporary directory
    test::setup_with_new_libra_in(temp_path.path()).await;

    // set the current working directory to the temporary path
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let result = exec_config(vec!["config", "--get-all", "-d", "erasernoob", "user.name"]).await;
    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_config_get() {
    let temp_path = tempdir().unwrap();
    // start a new libra repository in a temporary directory
    test::setup_with_new_libra_in(temp_path.path()).await;

    // set the current working directory to the temporary path
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Add the config first
    let result = exec_config(vec!["config", "--add", "user.name", "erasernoob"]).await;
    assert!(result.is_ok());

    let result = exec_config(vec!["config", "--get", "user.name"]).await;
    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_config_get_with_default() {
    let temp_path = tempdir().unwrap();
    // start a new libra repository in a temporary directory
    test::setup_with_new_libra_in(temp_path.path()).await;

    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let result = exec_config(vec!["config", "--get", "-d", "erasernoob", "user.name"]).await;
    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_config_list() {
    let temp_path = tempdir().unwrap();
    // start a new libra repository in a temporary directory
    test::setup_with_new_libra_in(temp_path.path()).await;

    // set the current working directory to the temporary path
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Add the config first
    let result = exec_config(vec!["config", "--add", "user.name", "erasernoob"]).await;
    assert!(result.is_ok());

    let result = exec_config(vec![
        "config",
        "--add",
        "user.email",
        "erasernoob@example.com",
    ])
    .await;
    assert!(result.is_ok());

    // List configs
    let result = exec_config(vec!["config", "--list"]).await;
    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_config_list_name_only() {
    let temp_path = tempdir().unwrap();
    // start a new libra repository in a temporary directory
    test::setup_with_new_libra_in(temp_path.path()).await;

    // set the current working directory to the temporary path
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Add the config first
    let result = exec_config(vec!["config", "--add", "user.name", "erasernoob"]).await;
    assert!(result.is_ok());

    let result = exec_config(vec![
        "config",
        "--add",
        "user.email",
        "erasernoob@example.com",
    ])
    .await;
    assert!(result.is_ok());

    // List configs with name_only via subcommand
    let result = exec_config(vec!["config", "list", "--name-only"]).await;
    assert!(result.is_ok());
}

// New tests for scope functionality
#[tokio::test]
#[serial]
async fn test_config_scope_local_default() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Test that no scope specified defaults to local
    let result = exec_config(vec!["config", "user.name", "test_user_local_default"]).await;
    assert!(result.is_ok());

    // Verify the value was written to local scope by reading it back
    let result = exec_config(vec!["config", "--get", "user.name"]).await;
    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_config_scope_global() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Isolate global DB paths to temp files (no host pollution).
    let global_db_dir = tempdir().unwrap();
    let _scoped = ScopedConfigPathGuard::new(&global_db_dir.path().join("global_config.db"));

    // Set a value in global scope
    let result = exec_config(vec![
        "config",
        "--global",
        "user.email",
        "global_user@example.com",
    ])
    .await;
    assert!(result.is_ok());

    // Verify the value was written to global scope by reading it back
    let result = exec_config(vec!["config", "--global", "--get", "user.email"]).await;
    assert!(result.is_ok());

    // Verify that the global value is NOT accessible from local scope
    let result = exec_config(vec![
        "config",
        "--local",
        "--get",
        "-d",
        "not_found",
        "user.email",
    ])
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_config_scope_system_errors() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Redirect the system scope to a temp DB so nothing touches /etc/libra.
    let system_db_dir = tempdir().unwrap();
    let _system = EnvVarGuard::set(
        "LIBRA_CONFIG_SYSTEM_DB",
        system_db_dir.path().join("system_vault.db").as_os_str(),
    );

    // Plain `--system` writes succeed, but vault-encrypted secrets are rejected.
    let ok = exec_config(vec!["config", "--system", "user.name", "system_user"]).await;
    assert!(ok.is_ok(), "--system plain set should succeed: {ok:?}");

    let result = exec_config(vec![
        "config",
        "set",
        "--system",
        "--encrypt",
        "custom.secret",
        "s3cr3t",
    ])
    .await;
    assert!(result.is_err(), "--system --encrypt should be rejected");
    let err = result.unwrap_err();
    assert!(
        err.message().contains("not supported in --system scope"),
        "unexpected error: {}",
        err.message()
    );

    // The whole `vault.*` namespace is rejected in system scope, including
    // non-sensitive pubkey keys that `is_sensitive_key` does not flag, and
    // mixed-case section names (Git section names are case-insensitive).
    for key in [
        "vault.signing",
        "vault.ssh.origin.pubkey",
        "Vault.signing",
        "VAULT.gpg.pubkey",
    ] {
        let r = exec_config(vec!["config", "--system", key, "x"]).await;
        assert!(r.is_err(), "--system {key} should be rejected");
        assert!(
            r.unwrap_err()
                .message()
                .contains("not supported in --system scope"),
            "{key} rejection should name the system scope"
        );
    }

    // `config import --system` is rejected up front: import auto-encrypts
    // sensitive keys, which the system scope does not support.
    let import = exec_config(vec!["config", "import", "--system"]).await;
    assert!(import.is_err(), "config import --system should be rejected");
    assert!(
        import
            .unwrap_err()
            .message()
            .contains("not supported in --system scope"),
        "import rejection should name the system scope"
    );
}

#[tokio::test]
#[serial]
async fn test_config_system_rejected_vault_write_does_not_create_db() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Point the system scope at a path that does NOT yet exist.
    let fresh_dir = tempdir().unwrap();
    let sys_db = fresh_dir.path().join("never").join("config.db");
    let _system = EnvVarGuard::set("LIBRA_CONFIG_SYSTEM_DB", sys_db.as_os_str());

    // A rejected `--system --encrypt` write must short-circuit before touching
    // the DB, so the system config path is never created.
    let result = exec_config(vec![
        "config",
        "set",
        "--system",
        "--encrypt",
        "custom.secret",
        "s3cr3t",
    ])
    .await;
    assert!(result.is_err(), "--system --encrypt should be rejected");
    assert!(
        !sys_db.exists(),
        "the rejected vault write must not create the system DB"
    );
}

#[tokio::test]
#[serial]
async fn test_config_system_rename_into_vault_namespace_rejected() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let system_db_dir = tempdir().unwrap();
    let _system = EnvVarGuard::set(
        "LIBRA_CONFIG_SYSTEM_DB",
        system_db_dir.path().join("system_rename.db").as_os_str(),
    );

    // Seed a plain (non-sensitive) system key, then try to rename its section
    // into the vault namespace — which would smuggle a secret key past the
    // direct-set guard. It must be rejected.
    let seed = exec_config(vec!["config", "--system", "foo.bar", "value"]).await;
    assert!(seed.is_ok(), "plain system set should succeed: {seed:?}");

    let rename = exec_config(vec![
        "config",
        "--system",
        "--rename-section",
        "foo",
        "vault.env",
    ])
    .await;
    assert!(
        rename.is_err(),
        "renaming a system section into vault.env must be rejected"
    );
    assert!(
        rename
            .unwrap_err()
            .message()
            .contains("not supported in --system scope"),
        "rename rejection should name the system scope"
    );
}

#[tokio::test]
#[serial]
async fn test_config_system_set_rejected_when_existing_row_is_encrypted() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Isolate HOME so the global vault key lands in the temp dir, then build an
    // encrypted row in a shared DB via the (vault-capable) global scope.
    let home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", home.path().as_os_str());
    let shared_db = temp_path.path().join("shared.db");
    let _global = EnvVarGuard::set("LIBRA_CONFIG_GLOBAL_DB", shared_db.as_os_str());

    let seed = exec_config(vec![
        "config",
        "set",
        "--global",
        "--encrypt",
        "custom.secret",
        "cipher",
    ])
    .await;
    assert!(seed.is_ok(), "seed encrypted global row: {seed:?}");

    // Reuse that DB as the system DB so it already holds an encrypted row, then
    // a `--system --plaintext` write to the same key must be rejected (it would
    // otherwise keep the row's encrypted flag while storing a plaintext value).
    let _system = EnvVarGuard::set("LIBRA_CONFIG_SYSTEM_DB", shared_db.as_os_str());
    let result = exec_config(vec![
        "config",
        "set",
        "--system",
        "--plaintext",
        "custom.secret",
        "newval",
    ])
    .await;
    assert!(
        result.is_err(),
        "--system --plaintext over an encrypted row must be rejected"
    );
    assert!(
        result
            .unwrap_err()
            .message()
            .contains("not supported in --system scope"),
        "rejection should name the system scope"
    );
}

#[tokio::test]
#[serial]
async fn test_config_scope_explicit_local() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Set a value explicitly in local scope
    let result = exec_config(vec![
        "config",
        "--local",
        "user.name",
        "explicit_local_user",
    ])
    .await;
    assert!(result.is_ok());

    // Verify the value was written to local scope by reading it back
    let result = exec_config(vec!["config", "--local", "--get", "user.name"]).await;
    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_config_scope_isolation() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    // Isolate global DB paths to temp files (no host pollution).
    let global_db_dir = tempdir().unwrap();
    let _scoped = ScopedConfigPathGuard::new(&global_db_dir.path().join("global_config.db"));

    // Set the same key with different values in different scopes
    let result = exec_config(vec!["config", "--local", "test.isolation", "local_value"]).await;
    assert!(result.is_ok());

    let result = exec_config(vec!["config", "--global", "test.isolation", "global_value"]).await;
    assert!(result.is_ok());

    // Verify that each scope returns its own value
    println!("Reading from local scope:");
    let result = exec_config(vec!["config", "--local", "--get", "test.isolation"]).await;
    assert!(result.is_ok());

    println!("Reading from global scope:");
    let result = exec_config(vec!["config", "--global", "--get", "test.isolation"]).await;
    assert!(result.is_ok());
}

#[tokio::test]
#[serial]
async fn test_config_get_reveal_decrypt_failure_returns_error() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    libra::internal::vault::lazy_init_vault_for_scope("local")
        .await
        .unwrap();
    libra::internal::config::ConfigKv::set("vault.env.TEST_SECRET", "not-valid-hex", true)
        .await
        .unwrap();

    let result = exec_config(vec!["config", "get", "--reveal", "vault.env.TEST_SECRET"]).await;
    let err = result.expect_err("decrypt failure should surface as an error");
    assert_eq!(err.kind(), CliErrorKind::Fatal);
    assert_eq!(err.exit_code(), 128);
    assert!(
        err.message()
            .contains("failed to decrypt value for key 'vault.env.TEST_SECRET'")
    );
}

#[tokio::test]
#[serial]
async fn test_config_get_cascaded_global_read_failure_returns_error() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let bad_global_db = temp_path.path().join("bad-global.db");
    std::fs::write(&bad_global_db, "definitely-not-a-sqlite-database").unwrap();
    let _scoped = ScopedConfigPathGuard::new(&bad_global_db);

    let result = exec_config(vec!["config", "get", "user.missing"]).await;
    let err = result.expect_err("broken cascaded scope should not be ignored");
    assert_eq!(err.kind(), CliErrorKind::Fatal);
    assert_eq!(err.exit_code(), 128);
    assert!(err.message().contains("failed to read global config"));
}

#[tokio::test]
#[serial]
async fn test_config_add_rejects_implicit_encryption_mixed_with_existing_plaintext() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let result = exec_config(vec![
        "config",
        "set",
        "--plaintext",
        "custom.token",
        "plaintext-token",
    ])
    .await;
    assert!(result.is_ok());

    let result = exec_config(vec![
        "config",
        "set",
        "--add",
        "custom.token",
        "second-token",
    ])
    .await;
    let err = result.expect_err("implicit auto-encryption should not mix with plaintext values");
    assert!(
        err.message()
            .contains("cannot mix encrypted and plaintext values for the same key"),
        "unexpected error: {}",
        err.message()
    );

    let entries = config::ScopedConfig::get_all(config::ConfigScope::Local, "custom.token")
        .await
        .unwrap();
    assert_eq!(entries.len(), 1, "mixed-state insert should be rejected");
    assert!(
        !entries[0].encrypted,
        "original plaintext entry should remain"
    );
    assert_eq!(entries[0].value, "plaintext-token");
}

#[tokio::test]
#[serial]
async fn test_config_set_encrypt_plaintext_mutex_is_command_usage_error() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let output = run_libra_command(
        &[
            "config",
            "set",
            "--encrypt",
            "--plaintext",
            "custom.token",
            "value",
        ],
        temp_path.path(),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--encrypt and --plaintext are mutually exclusive"),
        "stderr should describe the mutex violation, got: {stderr}"
    );
    // config.md line 77: classified as a usage error (exit 2 fine / 129 coarse).
    assert_eq!(
        output.status.code(),
        Some(129),
        "mutex flag error must classify as CLI usage (exit 129), got status: {:?}, stderr: {stderr}",
        output.status,
    );
}

#[tokio::test]
#[serial]
async fn test_config_set_stdin_with_positional_value_is_command_usage_error() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let output = run_libra_command(
        &["config", "set", "--stdin", "custom.token", "value"],
        temp_path.path(),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot use both value argument and --stdin"),
        "stderr should describe the --stdin vs positional mutex, got: {stderr}"
    );
    // config.md line 144: usage error (exit 2 fine / 129 coarse).
    assert_eq!(
        output.status.code(),
        Some(129),
        "--stdin + positional must classify as CLI usage (exit 129), got status: {:?}, stderr: {stderr}",
        output.status,
    );
}

#[tokio::test]
#[serial]
async fn test_config_set_plaintext_on_vault_internal_key_is_failure() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let output = run_libra_command(
        &[
            "config",
            "set",
            "--plaintext",
            "vault.env.API_KEY",
            "secret-value",
        ],
        temp_path.path(),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--plaintext cannot be used with vault internal/secret keys"),
        "stderr should describe the secret-key plaintext reject, got: {stderr}"
    );
    // config.md line 77: validation reject (exit 1 fine / 128 coarse) — must
    // classify as a runtime Failure (exit 128) rather than the previous
    // legacy-string fallthrough that produced the same number but with the
    // internal-invariant stable code.
    assert_eq!(
        output.status.code(),
        Some(128),
        "vault internal key plaintext reject must classify as Failure (exit 128), got status: {:?}, stderr: {stderr}",
        output.status,
    );
}

#[tokio::test]
#[serial]
async fn test_config_set_read_failure_does_not_silently_skip_existing_state_check() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());
    // Prevent any interactive prompts from blocking the test.
    let _test_env = EnvVarGuard::set("LIBRA_TEST", std::ffi::OsStr::new("1"));

    let bad_global_dir = tempdir().unwrap();
    let bad_global_db = bad_global_dir.path().join("bad-global.db");
    std::fs::write(&bad_global_db, "definitely-not-a-sqlite-database").unwrap();
    let _scoped = ScopedConfigPathGuard::new(&bad_global_db);

    let fake_home = tempdir().unwrap();
    let _home_guard = EnvVarGuard::set("HOME", fake_home.path().as_os_str());
    let _userprofile_guard = EnvVarGuard::set("USERPROFILE", fake_home.path().as_os_str());

    let result = exec_config(vec![
        "config",
        "set",
        "--global",
        "vault.env.TEST_SECRET",
        "super-secret",
    ])
    .await;
    let err = result.expect_err("broken config read should surface before write/lazy-init");
    assert_eq!(err.kind(), CliErrorKind::Fatal);
    assert_eq!(err.exit_code(), 128);
    assert!(
        err.message()
            .contains("failed to read global config while checking existing values"),
        "unexpected error: {}",
        err.message()
    );

    assert!(
        !fake_home
            .path()
            .join(".libra")
            .join("vault-unseal-key")
            .exists(),
        "failed existing-state lookup should not trigger global vault lazy init"
    );
}

#[tokio::test]
#[serial]
async fn test_config_set_missing_value_uses_protected_input_when_existing_key_is_encrypted() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());
    // Prevent rpassword::read_password() from blocking on stdin.
    let _test_env = EnvVarGuard::set("LIBRA_TEST", std::ffi::OsStr::new("1"));

    let result = exec_config(vec![
        "config",
        "set",
        "--encrypt",
        "custom.value",
        "encrypted-value",
    ])
    .await;
    assert!(result.is_ok());

    let result = exec_config(vec!["config", "set", "custom.value"]).await;
    let err = result.expect_err("existing encrypted state should require protected input");
    assert_eq!(err.exit_code(), 2);
    assert!(
        err.message()
            .contains("missing value for protected key 'custom.value'"),
        "unexpected error: {}",
        err.message()
    );
}

#[tokio::test]
#[serial]
async fn test_config_list_defaults_to_local_scope_without_global_entries() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    libra::internal::config::ConfigKv::set("user.name", "local-user", false)
        .await
        .unwrap();

    let child_home = temp_path.path().join(".libra-test-home");
    let child_global_dir = child_home.join(".libra");
    std::fs::create_dir_all(&child_global_dir).unwrap();
    let child_global_db = child_global_dir.join("config.db");
    let global_conn =
        libra::internal::db::create_database(child_global_db.to_string_lossy().as_ref())
            .await
            .unwrap();
    libra::internal::config::ConfigKv::set_with_conn(&global_conn, "core.editor", "vim", false)
        .await
        .unwrap();

    let output = run_libra_command(&["config", "list"], temp_path.path());
    assert!(
        output.status.success(),
        "config list should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("user.name=local-user"),
        "local entry should be listed, stdout: {stdout}"
    );
    assert!(
        !stdout.contains("core.editor"),
        "default list should not include global entries, stdout: {stdout}"
    );
}

#[tokio::test]
#[serial]
async fn test_config_list_ssh_keys_outputs_configured_public_keys() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    libra::internal::config::ConfigKv::set(
        "vault.ssh.origin.pubkey",
        "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQC origin-key",
        false,
    )
    .await
    .unwrap();
    libra::internal::config::ConfigKv::set(
        "vault.ssh.upstream.pubkey",
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA upstream-key",
        false,
    )
    .await
    .unwrap();
    libra::internal::config::ConfigKv::set("vault.ssh.origin.privkey", "ciphertext", true)
        .await
        .unwrap();

    let output = run_libra_command(&["config", "list", "--ssh-keys"], temp_path.path());
    assert!(
        output.status.success(),
        "config list --ssh-keys should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("SSH keys:"), "stdout: {stdout}");
    assert!(stdout.contains("origin"), "stdout: {stdout}");
    assert!(stdout.contains("upstream"), "stdout: {stdout}");
    assert!(
        stdout.contains("ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQC origin-key"),
        "stdout: {stdout}"
    );
    assert!(
        !stdout.contains("ciphertext"),
        "private key entries must not be listed, stdout: {stdout}"
    );
}

#[tokio::test]
#[serial]
async fn test_config_list_gpg_keys_outputs_configured_key_namespaces() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    libra::internal::config::ConfigKv::set(
        "vault.gpg.pubkey",
        "-----BEGIN PGP PUBLIC KEY BLOCK-----\nSIGNING\n-----END PGP PUBLIC KEY BLOCK-----",
        false,
    )
    .await
    .unwrap();
    libra::internal::config::ConfigKv::set(
        "vault.gpg.encrypt.pubkey",
        "-----BEGIN PGP PUBLIC KEY BLOCK-----\nENCRYPT\n-----END PGP PUBLIC KEY BLOCK-----",
        false,
    )
    .await
    .unwrap();
    libra::internal::config::ConfigKv::set("vault.signing", "true", false)
        .await
        .unwrap();

    let output = run_libra_command(&["config", "list", "--gpg-keys"], temp_path.path());
    assert!(
        output.status.success(),
        "config list --gpg-keys should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("GPG keys:"), "stdout: {stdout}");
    assert!(stdout.contains("signing"), "stdout: {stdout}");
    assert!(stdout.contains("encrypt"), "stdout: {stdout}");
    assert!(
        stdout.contains("vault.gpg.pubkey"),
        "signing pubkey key should be listed, stdout: {stdout}"
    );
    assert!(
        stdout.contains("vault.gpg.encrypt.pubkey"),
        "encrypt pubkey key should be listed, stdout: {stdout}"
    );
    assert!(
        stdout.contains("vault.signing = true"),
        "signing-enabled hint should be listed, stdout: {stdout}"
    );
}

#[tokio::test]
#[serial]
async fn test_config_generate_ssh_key_replaces_vault_generate_ssh_key_flow() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let remote = run_libra_command(
        &["remote", "add", "origin", "git@github.com:example/repo.git"],
        temp_path.path(),
    );
    assert_cli_success(&remote, "remote add origin");

    let output = run_libra_command(
        &["config", "generate-ssh-key", "--remote", "origin"],
        temp_path.path(),
    );
    assert_cli_success(&output, "config generate-ssh-key --remote origin");

    let pubkey = libra::internal::config::ConfigKv::get("vault.ssh.origin.pubkey")
        .await
        .unwrap()
        .expect("config generate-ssh-key should store a public key");
    assert!(
        pubkey.value.starts_with("ssh-rsa "),
        "expected RSA SSH public key, got: {}",
        pubkey.value
    );

    let privkey = libra::internal::config::ConfigKv::get("vault.ssh.origin.privkey")
        .await
        .unwrap()
        .expect("config generate-ssh-key should store an encrypted private key");
    assert!(privkey.encrypted, "private key must stay vault-encrypted");
    assert!(
        !privkey.value.contains("PRIVATE KEY"),
        "private key must not be stored as plaintext"
    );

    let get_output = run_libra_command(
        &["config", "get", "vault.ssh.origin.pubkey"],
        temp_path.path(),
    );
    assert_cli_success(&get_output, "config get vault.ssh.origin.pubkey");
    let stdout = String::from_utf8_lossy(&get_output.stdout);
    assert!(stdout.contains("ssh-rsa "), "stdout: {stdout}");
}

#[tokio::test]
#[serial]
async fn test_config_generate_global_ssh_key_is_rejected_without_local_side_effects() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let remote = run_libra_command(
        &["remote", "add", "origin", "git@github.com:example/repo.git"],
        temp_path.path(),
    );
    assert_cli_success(&remote, "remote add origin");

    libra::internal::config::ConfigKv::unset_all("vault.ssh.origin.pubkey")
        .await
        .unwrap();
    libra::internal::config::ConfigKv::unset_all("vault.ssh.origin.privkey")
        .await
        .unwrap();

    let output = run_libra_command(
        &[
            "config",
            "--global",
            "generate-ssh-key",
            "--remote",
            "origin",
        ],
        temp_path.path(),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("generate-ssh-key only supports local scope"),
        "stderr should explain unsupported global SSH key generation, got: {stderr}"
    );
    assert!(
        stderr.contains("run without --global"),
        "stderr should tell users how to run the supported form, got: {stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(129),
        "global generate-ssh-key should be a command usage error, got status: {:?}, stderr: {stderr}",
        output.status,
    );

    assert!(
        libra::internal::config::ConfigKv::get("vault.ssh.origin.pubkey")
            .await
            .unwrap()
            .is_none(),
        "--global generate-ssh-key must not write a local public key"
    );
    assert!(
        libra::internal::config::ConfigKv::get("vault.ssh.origin.privkey")
            .await
            .unwrap()
            .is_none(),
        "--global generate-ssh-key must not write a local private key"
    );
}

#[tokio::test]
#[serial]
async fn test_config_generate_ssh_key_rejects_invalid_remote_name_as_command_usage() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let output = run_libra_command(
        &["config", "generate-ssh-key", "--remote", "bad.name"],
        temp_path.path(),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid remote name 'bad.name'"),
        "stderr should describe the validation failure, got: {stderr}"
    );
    // CLI usage errors map to exit code 129 in coarse mode (Cli category →
    // CliExitCode::Usage). The previous implementation collapsed both the
    // invalid-name and missing-remote branches into `failure` (exit 128),
    // which is the wrong category for a user-supplied bad argument.
    assert_eq!(
        output.status.code(),
        Some(129),
        "invalid remote name must classify as a CLI usage error (exit 129), got status: {:?}, stderr: {stderr}",
        output.status,
    );
}

#[tokio::test]
#[serial]
async fn test_config_generate_ssh_key_rejects_unknown_remote_with_invalid_target_code() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let output = run_libra_command(
        &["config", "generate-ssh-key", "--remote", "no-such-remote"],
        temp_path.path(),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("remote 'no-such-remote' not found"),
        "stderr should describe the missing remote, got: {stderr}"
    );
    // Missing remote is a Fatal failure (exit 128 in coarse mode) — the
    // user-supplied name passed validation but the resource does not exist
    // at the time of execution.
    assert_eq!(
        output.status.code(),
        Some(128),
        "unknown remote must classify as a fatal failure (exit 128), got status: {:?}, stderr: {stderr}",
        output.status,
    );
}

#[tokio::test]
#[serial]
async fn test_config_generate_gpg_key_replaces_vault_generate_gpg_key_flow() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let output = run_libra_command(
        &[
            "config",
            "generate-gpg-key",
            "--name",
            "Config User",
            "--email",
            "config@example.com",
        ],
        temp_path.path(),
    );
    assert_cli_success(&output, "config generate-gpg-key");

    let pubkey = libra::internal::config::ConfigKv::get("vault.gpg.pubkey")
        .await
        .unwrap()
        .expect("config generate-gpg-key should store the signing public key");
    assert!(
        pubkey.value.contains("BEGIN PGP PUBLIC KEY BLOCK"),
        "expected armored PGP public key, got: {}",
        pubkey.value
    );

    let generated_stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        generated_stdout.contains("Config User <config@example.com>"),
        "expected configured user ID in command output, stdout: {generated_stdout}"
    );

    let signing = libra::internal::config::ConfigKv::get("vault.signing")
        .await
        .unwrap()
        .expect("signing key generation should enable vault signing");
    assert_eq!(signing.value, "true");

    let get_output = run_libra_command(&["config", "get", "vault.gpg.pubkey"], temp_path.path());
    assert_cli_success(&get_output, "config get vault.gpg.pubkey");
    let stdout = String::from_utf8_lossy(&get_output.stdout);
    assert!(
        stdout.contains("BEGIN PGP PUBLIC KEY BLOCK"),
        "stdout: {stdout}"
    );
}

#[tokio::test]
#[serial]
async fn test_config_generate_global_gpg_key_is_rejected_without_local_side_effects() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    libra::internal::config::ConfigKv::unset_all("vault.gpg.pubkey")
        .await
        .unwrap();
    libra::internal::config::ConfigKv::unset_all("vault.signing")
        .await
        .unwrap();

    let output = run_libra_command(
        &[
            "config",
            "--global",
            "generate-gpg-key",
            "--name",
            "Global User",
            "--email",
            "global@example.com",
        ],
        temp_path.path(),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("generate-gpg-key only supports local scope"),
        "stderr should explain unsupported global GPG key generation, got: {stderr}"
    );
    assert!(
        stderr.contains("run without --global"),
        "stderr should tell users how to run the supported form, got: {stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(129),
        "global generate-gpg-key should be a command usage error, got status: {:?}, stderr: {stderr}",
        output.status,
    );

    assert!(
        libra::internal::config::ConfigKv::get("vault.gpg.pubkey")
            .await
            .unwrap()
            .is_none(),
        "--global generate-gpg-key must not write a local GPG public key"
    );
    assert!(
        libra::internal::config::ConfigKv::get("vault.signing")
            .await
            .unwrap()
            .is_none(),
        "--global generate-gpg-key must not enable local vault signing"
    );
}

#[tokio::test]
#[serial]
async fn test_config_generate_gpg_key_rejects_invalid_usage() {
    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;

    let output = run_libra_command(
        &["config", "generate-gpg-key", "--usage", "archive"],
        temp_path.path(),
    );
    assert!(
        !output.status.success(),
        "generate-gpg-key should reject unsupported usage"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("invalid value 'archive'"),
        "stderr should explain invalid usage, stderr: {stderr}"
    );
    assert!(
        stderr.contains("signing") && stderr.contains("encrypt"),
        "stderr should list supported usages, stderr: {stderr}"
    );
}

#[tokio::test]
#[serial]
async fn test_config_scope_path_logic() {
    // Test the path logic for different scopes without executing config operations

    // Local scope should return None (uses repository database)
    assert_eq!(config::ConfigScope::Local.get_config_path(), None);

    // Global scope should return a path in the home directory (if available)
    let global_path = config::ConfigScope::Global.get_config_path();
    if dirs::home_dir().is_some() {
        assert!(global_path.is_some());
        let path = global_path.unwrap();
        assert!(path.to_string_lossy().contains(".libra"));
        assert!(path.to_string_lossy().ends_with("config.db"));
    } else {
        // In environments without home directory, should return None
        assert_eq!(global_path, None);
    }
}

#[tokio::test]
#[serial]
async fn test_config_cross_platform_paths() {
    // Test that all scopes return appropriate paths for the current platform

    // Local scope should always return None (uses repository database)
    assert_eq!(config::ConfigScope::Local.get_config_path(), None);

    // Global scope behavior (should work on all platforms with home directory)
    let global_path = config::ConfigScope::Global.get_config_path();
    if dirs::home_dir().is_some() {
        assert!(global_path.is_some());
        let path = global_path.unwrap();
        assert!(path.to_string_lossy().contains(".libra"));
        assert!(path.to_string_lossy().ends_with("config.db"));

        // Verify the path uses the correct separator for the platform
        #[cfg(windows)]
        {
            // On Windows, paths should use backslashes or be properly normalized
            let path_str = path.to_string_lossy();
            assert!(path_str.contains("libra") && path_str.contains("config.db"));
        }
        #[cfg(unix)]
        {
            // On Unix, paths should use forward slashes
            assert!(path.to_string_lossy().contains("/"));
        }
    }
}

/// Regression: a corrupted/incompatible `~/.libra/config.db` must not block
/// identity resolution.
///
/// Reproduced from a real 0.17.500 user report: `libra clone` aborted with
/// "fatal: vault initialization failed: failed to open config database
/// '/home/eli/.libra/config.db'" because the global config DB existed but
/// could not be opened (the only fix path was to delete the file). After
/// v0.17.515 `resolve_user_identity_sources` downgrades that failure to a
/// warning and returns `Ok` with `config_*` set to `None`, letting init
/// fall back to env vars / "Libra User" defaults.
#[tokio::test]
#[serial]
async fn resolve_user_identity_sources_tolerates_corrupt_global_db() {
    use libra::internal::config::{LocalIdentityTarget, resolve_user_identity_sources};

    let temp_dir = tempdir().unwrap();
    let global_db_path = temp_dir.path().join("corrupt_config.db");
    // A non-SQLite payload: opening this file as a sea-orm SQLite connection
    // (or running the schema-compat check on it) is guaranteed to fail.
    std::fs::write(&global_db_path, b"this is not a sqlite database").unwrap();

    let _global = EnvVarGuard::set("LIBRA_CONFIG_GLOBAL_DB", global_db_path.as_os_str());

    // Ensure env-var fallbacks are empty so we can attribute the result to
    // config-read tolerance, not env shadowing.
    let _git_committer_name = EnvVarGuard::set("GIT_COMMITTER_NAME", std::ffi::OsStr::new(""));
    let _git_committer_email = EnvVarGuard::set("GIT_COMMITTER_EMAIL", std::ffi::OsStr::new(""));
    let _git_author_name = EnvVarGuard::set("GIT_AUTHOR_NAME", std::ffi::OsStr::new(""));
    let _git_author_email = EnvVarGuard::set("GIT_AUTHOR_EMAIL", std::ffi::OsStr::new(""));
    let _email = EnvVarGuard::set("EMAIL", std::ffi::OsStr::new(""));
    let _libra_committer_name = EnvVarGuard::set("LIBRA_COMMITTER_NAME", std::ffi::OsStr::new(""));
    let _libra_committer_email =
        EnvVarGuard::set("LIBRA_COMMITTER_EMAIL", std::ffi::OsStr::new(""));

    let sources = resolve_user_identity_sources(LocalIdentityTarget::None)
        .await
        .expect("identity resolution must not propagate global DB read failures");

    assert!(
        sources.config_name.is_none(),
        "expected config_name to be None when global DB is unreadable, got {:?}",
        sources.config_name
    );
    assert!(
        sources.config_email.is_none(),
        "expected config_email to be None when global DB is unreadable, got {:?}",
        sources.config_email
    );
}

/// `resolve_env_for_target` is the shared secret resolver used by provider,
/// D1, R2, and tool credential paths. Per the 12-Factor /
/// docs/development/commands/config.md spec, the priority is
/// **process env > local vault > global vault**
/// so a per-process override like `GEMINI_API_KEY=B libra push` always wins.
/// Local vault is the fallback when env is unset.
#[tokio::test]
#[serial]
async fn resolve_env_for_target_process_env_overrides_local_vault() {
    use libra::internal::config::{ConfigKv, LocalIdentityTarget, resolve_env_for_target};

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _cwd = test::ChangeDirGuard::new(temp_path.path());

    let _env = EnvVarGuard::set(
        "LIBRA_RESOLVE_ENV_PRIORITY_KEY",
        std::ffi::OsStr::new("env-value"),
    );
    let _global = EnvVarGuard::set(
        "LIBRA_CONFIG_GLOBAL_DB",
        std::ffi::OsStr::new("/nonexistent/resolve-env-priority-local.db"),
    );

    ConfigKv::set(
        "vault.env.LIBRA_RESOLVE_ENV_PRIORITY_KEY",
        "vault-value",
        false,
    )
    .await
    .unwrap();

    // env wins; per-process override is sacred (12-Factor).
    let value = resolve_env_for_target(
        "LIBRA_RESOLVE_ENV_PRIORITY_KEY",
        LocalIdentityTarget::CurrentRepo,
    )
    .await
    .unwrap();
    assert_eq!(value.as_deref(), Some("env-value"));

    // …and when the env is unset, the local vault fallback is used.
    drop(_env);
    let value = resolve_env_for_target(
        "LIBRA_RESOLVE_ENV_PRIORITY_KEY",
        LocalIdentityTarget::CurrentRepo,
    )
    .await
    .unwrap();
    assert_eq!(value.as_deref(), Some("vault-value"));
}

/// Same priority chain in the `LocalIdentityTarget::None` mode used by
/// commands that can run outside a Libra worktree (provider/bootstrap path).
/// process env > global vault.
#[tokio::test]
#[serial]
async fn resolve_env_for_target_process_env_overrides_global_vault() {
    use libra::internal::{
        config::{ConfigKv, LocalIdentityTarget, resolve_env_for_target},
        db,
    };

    let _guard = EnvVarGuard::set(
        "LIBRA_RESOLVE_ENV_GLOBAL_PRIORITY_KEY",
        std::ffi::OsStr::new("env-value"),
    );
    let global_dir = tempdir().unwrap();
    let global_db_path = global_dir.path().join("global-config.db");
    let _global = EnvVarGuard::set("LIBRA_CONFIG_GLOBAL_DB", global_db_path.as_os_str());
    let global_conn = db::create_database(global_db_path.to_string_lossy().as_ref())
        .await
        .unwrap();
    ConfigKv::set_with_conn(
        &global_conn,
        "vault.env.LIBRA_RESOLVE_ENV_GLOBAL_PRIORITY_KEY",
        "global-vault-value",
        false,
    )
    .await
    .unwrap();

    // env wins.
    let value = resolve_env_for_target(
        "LIBRA_RESOLVE_ENV_GLOBAL_PRIORITY_KEY",
        LocalIdentityTarget::None,
    )
    .await
    .unwrap();
    assert_eq!(value.as_deref(), Some("env-value"));

    // …and global vault is the fallback when env is unset.
    drop(_guard);
    let value = resolve_env_for_target(
        "LIBRA_RESOLVE_ENV_GLOBAL_PRIORITY_KEY",
        LocalIdentityTarget::None,
    )
    .await
    .unwrap();
    assert_eq!(value.as_deref(), Some("global-vault-value"));
}

/// Process env remains the final fallback when neither local nor global Vault
/// supplies the key.
#[tokio::test]
#[serial]
async fn resolve_env_sync_falls_back_to_process_env_when_vault_missing() {
    use libra::internal::config::resolve_env_sync;

    let _guard = EnvVarGuard::set(
        "LIBRA_RESOLVE_ENV_SYNC_TEST_KEY",
        std::ffi::OsStr::new("env-fallback"),
    );
    let _global = EnvVarGuard::set(
        "LIBRA_CONFIG_GLOBAL_DB",
        std::ffi::OsStr::new("/nonexistent/resolve-env-sync-fallback-path.db"),
    );

    let value = resolve_env_sync("LIBRA_RESOLVE_ENV_SYNC_TEST_KEY").unwrap();
    assert_eq!(value.as_deref(), Some("env-fallback"));
}

/// Absence path: when no process env, no repo, and no global DB layer carries
/// the key, the wrapper returns `Ok(None)` (not an error). A schema-mismatch
/// on the global DB is treated as missing-value here (the underlying
/// `resolve_env_for_target` already downgrades that to `tracing::warn!`),
/// matching the v0.17.515 / v0.17.534 fallback contract.
#[tokio::test]
#[serial]
async fn resolve_env_sync_returns_none_when_no_layer_supplies_value() {
    use libra::internal::config::resolve_env_sync;

    let _guard = EnvVarGuard::unset("LIBRA_RESOLVE_ENV_SYNC_ABSENT_KEY");
    let _global = EnvVarGuard::set(
        "LIBRA_CONFIG_GLOBAL_DB",
        std::ffi::OsStr::new("/nonexistent/resolve-env-sync-absent-path.db"),
    );

    let value = resolve_env_sync("LIBRA_RESOLVE_ENV_SYNC_ABSENT_KEY").unwrap();
    assert!(
        value.is_none(),
        "expected None for an unset key, got {value:?}"
    );
}

/// Regression: `ConfigKv::get_best_effort` must surface a database-open failure
/// as an `Err` rather than panicking. The plain `ConfigKv::get` resolves its
/// connection through `get_db_conn_instance`, which panics when the repository
/// database cannot be opened (missing file or out-of-date schema). During
/// `clone`/`fetch` the SSH transport setup reads config best-effort and may
/// walk up into an *enclosing* repo whose schema this binary no longer
/// supports — that previously dumped a panic to stderr. `get_best_effort` must
/// degrade gracefully instead.
#[tokio::test]
#[serial]
async fn get_best_effort_returns_err_outside_repository() {
    use libra::internal::config::ConfigKv;

    // An empty temp dir with no `.libra/` anywhere up the tree: the database
    // cannot be located/opened, so the call must return Err — never panic.
    let temp_path = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    let result = ConfigKv::get_best_effort("ssh.strictHostKeyChecking").await;
    assert!(
        result.is_err(),
        "expected an Err (not a panic) outside a repository, got {result:?}"
    );
}

/// Happy path: inside a valid repository `get_best_effort` reads the stored
/// value just like `get`, confirming the non-panicking wrapper still resolves
/// the per-repo database correctly.
#[tokio::test]
#[serial]
async fn get_best_effort_reads_value_inside_repository() {
    use libra::internal::config::ConfigKv;

    let temp_path = tempdir().unwrap();
    test::setup_with_new_libra_in(temp_path.path()).await;
    let _guard = test::ChangeDirGuard::new(temp_path.path());

    ConfigKv::set("ssh.strictHostKeyChecking", "yes", false)
        .await
        .unwrap();

    let entry = ConfigKv::get_best_effort("ssh.strictHostKeyChecking")
        .await
        .unwrap();
    assert_eq!(entry.map(|e| e.value).as_deref(), Some("yes"));
}

/// `--remove-section` / `--rename-section` operate on whole sections: rename
/// moves every `old.*` key to `new.*` (siblings untouched), remove deletes all
/// keys under the section, a missing section is exit 128, and renaming to the
/// same name is rejected (exit 2) so the move cannot delete what it just wrote.
#[tokio::test]
#[serial]
async fn test_config_remove_and_rename_section() {
    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let _guard = test::ChangeDirGuard::new(temp.path());
    let p = temp.path();

    let set = |k: &str, v: &str| {
        assert_cli_success(
            &run_libra_command(&["config", "--local", k, v], p),
            "config set",
        );
    };
    set("branch.feature.remote", "origin");
    set("branch.feature.merge", "refs/heads/feature");
    set("branch.other.remote", "upstream");

    // Rename branch.feature -> branch.renamed.
    assert_cli_success(
        &run_libra_command(
            &[
                "config",
                "--local",
                "--rename-section",
                "branch.feature",
                "branch.renamed",
            ],
            p,
        ),
        "rename-section",
    );

    // New keys carry the original values.
    let r1 = run_libra_command(&["config", "--local", "--get", "branch.renamed.remote"], p);
    assert_cli_success(&r1, "get renamed.remote");
    assert!(String::from_utf8_lossy(&r1.stdout).contains("origin"));
    let r2 = run_libra_command(&["config", "--local", "--get", "branch.renamed.merge"], p);
    assert!(String::from_utf8_lossy(&r2.stdout).contains("refs/heads/feature"));

    // The old section is gone; the sibling section is untouched.
    assert!(
        !run_libra_command(&["config", "--local", "--get", "branch.feature.remote"], p)
            .status
            .success(),
        "old section key must be removed by rename"
    );
    let sib = run_libra_command(&["config", "--local", "--get", "branch.other.remote"], p);
    assert_cli_success(&sib, "sibling untouched");
    assert!(String::from_utf8_lossy(&sib.stdout).contains("upstream"));

    // Remove the renamed section.
    assert_cli_success(
        &run_libra_command(
            &["config", "--local", "--remove-section", "branch.renamed"],
            p,
        ),
        "remove-section",
    );
    assert!(
        !run_libra_command(&["config", "--local", "--get", "branch.renamed.remote"], p)
            .status
            .success(),
        "removed section key must be gone"
    );

    // Removing a non-existent section is "No such section" (exit 128).
    assert_eq!(
        run_libra_command(&["config", "--local", "--remove-section", "nope"], p)
            .status
            .code(),
        Some(128),
        "removing a missing section must exit 128"
    );

    // Renaming a section onto itself is rejected (exit 2).
    assert_eq!(
        run_libra_command(
            &[
                "config",
                "--local",
                "--rename-section",
                "branch.other",
                "branch.other"
            ],
            p,
        )
        .status
        .code(),
        Some(2),
        "identical rename must be rejected with exit 2"
    );
}

/// Section ops use Git's exact section/subsection identity, not a raw prefix:
/// `--remove-section branch` removes only the bare-section key `branch.x`, not
/// the subsection key `branch.feature.remote`. Renaming onto a destination
/// section that already has keys is rejected (exit 128) so no merge/flag
/// ambiguity can occur.
#[tokio::test]
#[serial]
async fn test_config_section_ops_exact_git_semantics() {
    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let _guard = test::ChangeDirGuard::new(temp.path());
    let p = temp.path();
    let set = |k: &str, v: &str| {
        assert_cli_success(&run_libra_command(&["config", "--local", k, v], p), "set");
    };

    set("branch.autosetupmerge", "always"); // bare section `branch`
    set("branch.feature.remote", "origin"); // subsection `branch.feature`

    // Removing the bare section must NOT touch the subsection.
    assert_cli_success(
        &run_libra_command(&["config", "--local", "--remove-section", "branch"], p),
        "remove bare section",
    );
    assert!(
        !run_libra_command(&["config", "--local", "--get", "branch.autosetupmerge"], p)
            .status
            .success(),
        "the bare-section key must be removed"
    );
    let kept = run_libra_command(&["config", "--local", "--get", "branch.feature.remote"], p);
    assert_cli_success(
        &kept,
        "subsection key must survive removing the bare section",
    );
    assert!(String::from_utf8_lossy(&kept.stdout).contains("origin"));

    // Renaming onto an existing destination section is rejected; source survives.
    set("dst.x", "1");
    set("src.y", "2");
    assert_eq!(
        run_libra_command(&["config", "--local", "--rename-section", "src", "dst"], p)
            .status
            .code(),
        Some(128),
        "rename onto an existing destination section must be rejected (128)"
    );
    assert!(
        run_libra_command(&["config", "--local", "--get", "src.y"], p)
            .status
            .success(),
        "source must be preserved after a rejected rename"
    );
}

/// `--rename-section` preserves multi-value order (each value is re-added under
/// the new key in its original insertion order).
#[tokio::test]
#[serial]
async fn test_config_rename_section_preserves_multivalue_order() {
    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let _guard = test::ChangeDirGuard::new(temp.path());
    let p = temp.path();

    assert_cli_success(
        &run_libra_command(&["config", "--local", "--add", "mvtest.list", "first"], p),
        "add first",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--local", "--add", "mvtest.list", "second"], p),
        "add second",
    );

    assert_cli_success(
        &run_libra_command(
            &["config", "--local", "--rename-section", "mvtest", "moved"],
            p,
        ),
        "rename multi-value section",
    );

    let g = run_libra_command(&["config", "--local", "--get-all", "moved.list"], p);
    assert_cli_success(&g, "get-all moved.list");
    let out = String::from_utf8_lossy(&g.stdout);
    let first = out.find("first");
    let second = out.find("second");
    assert!(
        first.is_some() && second.is_some() && first < second,
        "multi-value insertion order must be preserved (first before second): {out}"
    );
    // `--get-all` on a now-missing key exits 0 with empty output, so assert the
    // old values are gone rather than expecting a non-zero exit.
    let old = run_libra_command(&["config", "--local", "--get-all", "mvtest.list"], p);
    let old_out = String::from_utf8_lossy(&old.stdout);
    assert!(
        !old_out.contains("first") && !old_out.contains("second"),
        "the old multi-value key must be removed, got: {old_out}"
    );
}

/// `-z` / `--null` NUL-terminates output (`git config -z`): values for
/// `--get`/`--get-all`, and `key\nvalue\0` records for `--get-regexp`/`--list`
/// (`key\0` with `--name-only`).
#[tokio::test]
#[serial]
async fn test_config_null_terminated_output() {
    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let _guard = test::ChangeDirGuard::new(temp.path());
    let p = temp.path();
    let set = |k: &str, v: &str| {
        assert_cli_success(&run_libra_command(&["config", "--local", k, v], p), "set");
    };
    set("alpha.one", "v1");
    set("alpha.two", "v2");

    // --get -z : value\0 (exact bytes).
    let g = run_libra_command(&["config", "--local", "-z", "--get", "alpha.one"], p);
    assert_cli_success(&g, "get -z");
    assert_eq!(
        g.stdout, b"v1\0",
        "get -z must emit value + NUL, got {:?}",
        g.stdout
    );

    // --get-regexp -z : key\nvalue\0 per entry.
    let gr = run_libra_command(&["config", "--local", "-z", "--get-regexp", "^alpha\\."], p);
    assert_cli_success(&gr, "get-regexp -z");
    let grs = String::from_utf8_lossy(&gr.stdout);
    assert!(
        grs.contains("alpha.one\nv1\0") && grs.contains("alpha.two\nv2\0"),
        "get-regexp -z must emit key\\nvalue\\0, got {:?}",
        gr.stdout
    );

    // --list -z : key\nvalue\0 (no '=' separator).
    let l = run_libra_command(&["config", "--local", "-z", "--list"], p);
    assert_cli_success(&l, "list -z");
    let ls = String::from_utf8_lossy(&l.stdout);
    assert!(
        ls.contains("alpha.one\nv1\0")
            && ls.contains("alpha.two\nv2\0")
            && !ls.contains("alpha.one=v1"),
        "list -z must emit key\\nvalue\\0 (no '='), got {:?}",
        l.stdout
    );

    // --name-only -z (subcommand form, -z is a global flag): key\0, no values.
    let ln = run_libra_command(&["config", "--local", "list", "--name-only", "-z"], p);
    assert_cli_success(&ln, "list --name-only -z");
    let lns = String::from_utf8_lossy(&ln.stdout);
    assert!(
        lns.contains("alpha.one\0") && lns.contains("alpha.two\0") && !lns.contains("v1"),
        "list --name-only -z must emit key\\0 with no values, got {:?}",
        ln.stdout
    );

    // `-z` applies to standard config output only: combining it with the
    // Libra-only --ssh-keys/--gpg-keys/--vault views is a usage error (129).
    assert_eq!(
        run_libra_command(&["config", "--local", "list", "--ssh-keys", "-z"], p)
            .status
            .code(),
        Some(129),
        "-z with --ssh-keys must be rejected as a usage error"
    );
}

/// `--type=<bool|int|path>` and the `--bool`/`--int`/`--path` shortcuts
/// canonicalize a value when reading (`git config --type`): bool variants,
/// int k/m/g multipliers, and `~` path expansion. Invalid values error, and
/// the flags are rejected outside get modes / for an unknown type.
#[tokio::test]
#[serial]
async fn test_config_typed_get() {
    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let _guard = test::ChangeDirGuard::new(temp.path());
    let p = temp.path();
    let set = |k: &str, v: &str| {
        assert_cli_success(&run_libra_command(&["config", "--local", k, v], p), "set");
    };
    set("flag.on", "yes");
    set("flag.off", "0");
    set("num.size", "1k");
    set("num.bad", "notanint");
    set("p.home", "~/work");

    // --bool: yes → true, 0 → false.
    let b = run_libra_command(&["config", "--local", "--bool", "--get", "flag.on"], p);
    assert_cli_success(&b, "--bool get");
    assert_eq!(String::from_utf8_lossy(&b.stdout).trim(), "true");
    let b2 = run_libra_command(&["config", "--local", "--bool", "--get", "flag.off"], p);
    assert_eq!(String::from_utf8_lossy(&b2.stdout).trim(), "false");

    // --int and --type=int both apply the k multiplier: 1k → 1024.
    let i = run_libra_command(&["config", "--local", "--int", "--get", "num.size"], p);
    assert_cli_success(&i, "--int get");
    assert_eq!(String::from_utf8_lossy(&i.stdout).trim(), "1024");
    let it = run_libra_command(
        &["config", "--local", "--type", "int", "--get", "num.size"],
        p,
    );
    assert_cli_success(&it, "--type int get");
    assert_eq!(String::from_utf8_lossy(&it.stdout).trim(), "1024");

    // A non-int value with --int errors.
    assert!(
        !run_libra_command(&["config", "--local", "--int", "--get", "num.bad"], p)
            .status
            .success(),
        "non-int value with --int must error"
    );

    // --path expands a leading ~/.
    let pa = run_libra_command(&["config", "--local", "--path", "--get", "p.home"], p);
    assert_cli_success(&pa, "--path get");
    let pout = String::from_utf8_lossy(&pa.stdout);
    assert!(
        !pout.trim().starts_with('~') && pout.trim().ends_with("/work"),
        "--path must expand a leading ~/: {pout}"
    );

    // The type flags are rejected outside get modes and for an unknown type.
    assert_eq!(
        run_libra_command(&["config", "--local", "--bool", "--list"], p)
            .status
            .code(),
        Some(129),
        "--bool with --list must be rejected (129)"
    );
    assert_eq!(
        run_libra_command(
            &["config", "--local", "--type", "frob", "--get", "flag.on"],
            p
        )
        .status
        .code(),
        Some(129),
        "unknown --type must be rejected (129)"
    );

    // Two type selectors at once are mutually exclusive (clap rejects).
    assert!(
        !run_libra_command(
            &["config", "--local", "--bool", "--int", "--get", "flag.on"],
            p
        )
        .status
        .success(),
        "--bool --int together must be rejected"
    );

    // No whitespace trimming: a padded value is not a valid bool (matches Git).
    set("flag.padded", " true ");
    assert!(
        !run_libra_command(&["config", "--local", "--bool", "--get", "flag.padded"], p)
            .status
            .success(),
        "a whitespace-padded bool value must be rejected"
    );

    // An explicit empty value canonicalizes to false (git: `if (!*value) return
    // 0`; only a valueless key is true, which Libra's string storage never has).
    set("flag.empty", "");
    let e = run_libra_command(&["config", "--local", "--bool", "--get", "flag.empty"], p);
    assert_cli_success(&e, "--bool get empty");
    assert_eq!(String::from_utf8_lossy(&e.stdout).trim(), "false");
}

/// `--type=<bool|int|path>` (and the `--bool`/`--int`/`--path` shortcuts) also
/// apply when SETTING: the value is validated and canonicalized before storage,
/// matching `git config --type` (e.g. `yes` → `true`, `1k` → `1024`). An
/// invalid value errors without storing, and `--type` with a non-get/non-set
/// mode is still rejected.
#[tokio::test]
#[serial]
async fn test_config_typed_set() {
    let temp = tempdir().unwrap();
    test::setup_with_new_libra_in(temp.path()).await;
    let _guard = test::ChangeDirGuard::new(temp.path());
    let p = temp.path();

    let get = |k: &str| -> String {
        let out = run_libra_command(&["config", "--local", "--get", k], p);
        assert_cli_success(&out, "get");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    // bool canonicalizes on set (yes → true).
    assert_cli_success(
        &run_libra_command(
            &["config", "--local", "--type", "bool", "flag.on", "yes"],
            p,
        ),
        "typed bool set",
    );
    assert_eq!(get("flag.on"), "true");

    // --bool shortcut likewise (ON → true).
    assert_cli_success(
        &run_libra_command(&["config", "--local", "--bool", "flag.up", "ON"], p),
        "--bool set",
    );
    assert_eq!(get("flag.up"), "true");

    // int with a k multiplier canonicalizes (1k → 1024).
    assert_cli_success(
        &run_libra_command(&["config", "--local", "--type", "int", "num.size", "1k"], p),
        "typed int set",
    );
    assert_eq!(get("num.size"), "1024");

    // path expands ~/ on set.
    assert_cli_success(
        &run_libra_command(&["config", "--local", "--path", "dir.home", "~/work"], p),
        "typed path set",
    );
    assert!(
        get("dir.home").ends_with("/work") && !get("dir.home").starts_with('~'),
        "path is home-expanded: {}",
        get("dir.home")
    );

    // An invalid typed value errors and does NOT store the key.
    let bad = run_libra_command(&["config", "--local", "--type", "int", "n.bad", "abc"], p);
    assert!(!bad.status.success(), "invalid int must error");
    let missing = run_libra_command(&["config", "--local", "--get", "n.bad"], p);
    assert!(
        !missing.status.success(),
        "the invalid value must not be stored"
    );

    // `--type` with a non-get/non-set mode (here `--unset`) is still a usage error.
    let unset = run_libra_command(
        &["config", "--local", "--type", "int", "--unset", "num.size"],
        p,
    );
    assert_eq!(
        unset.status.code(),
        Some(129),
        "--type with --unset is a usage error: {}",
        String::from_utf8_lossy(&unset.stderr)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Reserved `upgrade.*` namespace (plan-20260714 §A.3)
// ─────────────────────────────────────────────────────────────────────────────

/// Settings path used by the spawned binary: `base_libra_command` pins HOME to
/// `<cwd>/.libra-test-home`, so `resolve_libra_home()` lands on
/// `<cwd>/.libra-test-home/.libra`.
fn upgrade_settings_file(cwd: &std::path::Path) -> std::path::PathBuf {
    cwd.join(".libra-test-home")
        .join(".libra")
        .join("upgrade")
        .join("settings.json")
}

#[test]
fn test_config_upgrade_mode_set_get_roundtrip() {
    let temp = tempdir().unwrap();
    let p = temp.path();

    // Missing settings file reads as `off`.
    let get = run_libra_command(&["config", "get", "--global", "upgrade.mode"], p);
    assert_cli_success(&get, "get before set");
    assert_eq!(String::from_utf8_lossy(&get.stdout).trim(), "off");

    // Case-insensitive set (flag-style spelling).
    let set = run_libra_command(&["config", "set", "--global", "upgrade.mode", "AUTO"], p);
    assert_cli_success(&set, "set --global upgrade.mode AUTO");
    assert!(
        String::from_utf8_lossy(&set.stdout).contains("Set global: upgrade.mode"),
        "ack: {}",
        String::from_utf8_lossy(&set.stdout)
    );

    // The value lives in the settings file, canonicalized to lowercase.
    let file = upgrade_settings_file(p);
    let raw = std::fs::read_to_string(&file).expect("settings.json written");
    let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["mode"], "auto");

    // Both get spellings read it back.
    let get = run_libra_command(&["config", "get", "--global", "upgrade.mode"], p);
    assert_cli_success(&get, "get after set");
    assert_eq!(String::from_utf8_lossy(&get.stdout).trim(), "auto");
    let get_flag = run_libra_command(&["config", "--global", "--get", "upgrade.mode"], p);
    assert_cli_success(&get_flag, "--get after set");
    assert_eq!(String::from_utf8_lossy(&get_flag.stdout).trim(), "auto");

    // Legacy positional set form routes through the same file.
    let set2 = run_libra_command(&["config", "--global", "upgrade.mode", "manual"], p);
    assert_cli_success(&set2, "positional set form");
    let raw = std::fs::read_to_string(&file).unwrap();
    assert!(raw.contains("manual"), "positional set persisted: {raw}");

    // A regexp spelling that can match the reserved key fails closed (§A.3).
    let regexp = run_libra_command(&["config", "--global", "--get-regexp", "upgrade."], p);
    assert_eq!(
        regexp.status.code(),
        Some(129),
        "--get-regexp matching upgrade.mode must fail closed: {}",
        String::from_utf8_lossy(&regexp.stderr)
    );
}

#[test]
fn test_config_upgrade_mode_rejects_invalid_value_and_scopes() {
    let temp = tempdir().unwrap();
    let p = temp.path();
    let init = run_libra_command(&["init"], p);
    assert_cli_success(&init, "init");

    // Invalid enum value.
    let bad = run_libra_command(
        &["config", "set", "--global", "upgrade.mode", "sometimes"],
        p,
    );
    assert_eq!(bad.status.code(), Some(129), "invalid value is usage error");

    // Non-global scopes fail closed.
    for args in [
        vec!["config", "set", "upgrade.mode", "auto"],
        vec!["config", "set", "--local", "upgrade.mode", "auto"],
        vec!["config", "set", "--system", "upgrade.mode", "auto"],
    ] {
        let out = run_libra_command(&args, p);
        assert_eq!(
            out.status.code(),
            Some(129),
            "{args:?} must fail closed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("reserved"),
            "{args:?} names the reserved namespace: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Multivalue / typed / section / unset-all operations fail closed.
    for args in [
        vec!["config", "--global", "--add", "upgrade.mode", "auto"],
        vec!["config", "--global", "--get-all", "upgrade.mode"],
        vec!["config", "--global", "--bool", "--get", "upgrade.mode"],
        vec!["config", "--global", "--remove-section", "upgrade"],
        vec!["config", "--global", "--rename-section", "upgrade", "up2"],
        vec!["config", "--global", "--unset-all", "upgrade.mode"],
        vec!["config", "get", "--global", "-d", "auto", "upgrade.mode"],
        vec!["config", "--global", "--unset", "upgrade.other"],
        // Padded values/keys are invalid — no whitespace normalization.
        vec!["config", "set", "--global", "upgrade.mode", " auto "],
        vec!["config", "set", "--global", " upgrade.mode", "auto"],
        // Conflicting action spellings fail closed instead of silently
        // dropping one of them.
        vec!["config", "--global", "--get", "--get-all", "upgrade.mode"],
        vec!["config", "--global", "--get", "--unset", "upgrade.mode"],
        vec!["config", "--global", "--add", "set", "upgrade.mode", "auto"],
        vec!["config", "--global", "--unset-all", "unset", "upgrade.mode"],
        // Raw-spelling preflight: resolution priority must not silently drop
        // a reserved operand or rewrite the intent (round-2 findings).
        vec!["config", "--global", "--list", "--get", "upgrade.mode"],
        vec!["config", "--global", "--list", "--unset", "upgrade.mode"],
        vec!["config", "--global", "--import", "--get", "upgrade.mode"],
        vec!["config", "--global", "--list", "upgrade.mode"],
        vec!["config", "--global", "--unset", "upgrade.mode", "nomatch"],
        vec!["config", "--global", "--get", "upgrade.mode", "pattern"],
        // Round-3: regexp patterns and rename destinations are raw operands
        // that reach the reserved namespace too.
        vec![
            "config",
            "--global",
            "--list",
            "--get-regexp",
            "^upgrade[.]mode$",
        ],
        vec![
            "config",
            "--global",
            "--import",
            "--get-regexp",
            "^upgrade[.]mode$",
        ],
        vec![
            "config",
            "--global",
            "--list",
            "--rename-section",
            "ordinary",
            "upgrade",
        ],
    ] {
        let out = run_libra_command(&args, p);
        assert_eq!(
            out.status.code(),
            Some(129),
            "{args:?} must fail closed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // None of the rejections may create the settings file or the global store.
    assert!(
        !upgrade_settings_file(p).exists(),
        "rejected operations must not write settings.json"
    );
    assert!(
        !p.join(".libra-test-home")
            .join(".libra")
            .join("config.db")
            .exists(),
        "rejected operations must not create the global config store"
    );
}

#[test]
fn test_config_upgrade_mode_unset_resets_off_and_keeps_file() {
    let temp = tempdir().unwrap();
    let p = temp.path();

    let set = run_libra_command(&["config", "set", "--global", "upgrade.mode", "auto"], p);
    assert_cli_success(&set, "set auto");

    let unset = run_libra_command(&["config", "unset", "--global", "upgrade.mode"], p);
    assert_cli_success(&unset, "unset");
    assert!(
        String::from_utf8_lossy(&unset.stdout).contains("upgrade.mode"),
        "unset ack: {}",
        String::from_utf8_lossy(&unset.stdout)
    );

    // §A.3: the file is kept, mode is reset to off.
    let file = upgrade_settings_file(p);
    assert!(file.exists(), "unset must keep the settings file");
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(doc["mode"], "off");

    let get = run_libra_command(&["config", "get", "--global", "upgrade.mode"], p);
    assert_cli_success(&get, "get after unset");
    assert_eq!(String::from_utf8_lossy(&get.stdout).trim(), "off");
}

#[test]
fn test_config_upgrade_mode_corrupt_file_is_strict_error() {
    let temp = tempdir().unwrap();
    let p = temp.path();
    let file = upgrade_settings_file(p);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, b"{ not json").unwrap();

    let get = run_libra_command(&["config", "get", "--global", "upgrade.mode"], p);
    assert!(!get.status.success(), "corrupt settings must fail get");
    let stderr = String::from_utf8_lossy(&get.stderr);
    assert!(
        stderr.contains("LBR-UPGRADE-001"),
        "stable code surfaced: {stderr}"
    );
    assert!(
        stderr.contains("upgrade.mode"),
        "actionable rewrite hint present: {stderr}"
    );

    // list --global inherits the strict failure (same source of truth).
    let list = run_libra_command(&["config", "--list", "--global"], p);
    assert!(!list.status.success(), "corrupt settings must fail list");

    // An invalid stored mode is also strict.
    std::fs::write(&file, br#"{ "schema_version": 1, "mode": "yes" }"#).unwrap();
    let get = run_libra_command(&["config", "get", "--global", "upgrade.mode"], p);
    assert!(!get.status.success(), "invalid mode enum must fail get");

    // A file that exists without a valid mode (missing/null) is damaged
    // state, not `off`.
    for body in [
        br#"{}"#.as_slice(),
        br#"{ "schema_version": 1 }"#.as_slice(),
        br#"{ "schema_version": 1, "mode": null }"#.as_slice(),
    ] {
        std::fs::write(&file, body).unwrap();
        let get = run_libra_command(&["config", "get", "--global", "upgrade.mode"], p);
        assert!(
            !get.status.success(),
            "settings body {:?} must fail get",
            String::from_utf8_lossy(body)
        );
        assert!(
            String::from_utf8_lossy(&get.stderr).contains("LBR-UPGRADE-001"),
            "damaged settings use the upgrade stable code"
        );
    }
}

#[tokio::test]
#[serial]
async fn test_config_upgrade_mode_list_uses_file_and_suppresses_sqlite() {
    let temp = tempdir().unwrap();
    let p = temp.path();

    // Plant a stale legacy `upgrade.mode` row directly into the SQLite store
    // the spawned binary reads (`<home>/.libra/config.db`).
    let db = p.join(".libra-test-home").join(".libra").join("config.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let conn = libra::internal::db::create_database(db.to_str().unwrap())
        .await
        .unwrap();
    libra::internal::config::ConfigKv::set_with_conn(&conn, "upgrade.mode", "manual", false)
        .await
        .unwrap();
    libra::internal::config::ConfigKv::set_with_conn(&conn, "upgrade.legacy", "stale", false)
        .await
        .unwrap();
    drop(conn);

    // Without a settings file, list shows no upgrade.mode at all — the stale
    // SQLite row must be suppressed, not rendered.
    let list = run_libra_command(&["config", "--list", "--global"], p);
    assert_cli_success(&list, "list with stale row only");
    assert!(
        !String::from_utf8_lossy(&list.stdout).contains("upgrade.mode"),
        "stale SQLite row must be suppressed: {}",
        String::from_utf8_lossy(&list.stdout)
    );
    assert!(
        !String::from_utf8_lossy(&list.stdout).contains("upgrade.legacy"),
        "every stale upgrade.* row must be suppressed: {}",
        String::from_utf8_lossy(&list.stdout)
    );

    // With a settings file, exactly one file-backed entry appears.
    let set = run_libra_command(&["config", "set", "--global", "upgrade.mode", "auto"], p);
    assert_cli_success(&set, "set auto");
    let list = run_libra_command(&["config", "--list", "--global"], p);
    assert_cli_success(&list, "list with settings file");
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert_eq!(
        stdout.matches("upgrade.mode").count(),
        1,
        "exactly one upgrade.mode entry: {stdout}"
    );
    assert!(
        stdout.contains("upgrade.mode=auto"),
        "value shown: {stdout}"
    );

    // --show-origin renders the `file:{path}` origin (§A.3 table).
    let origin = run_libra_command(&["config", "--list", "--show-origin", "--global"], p);
    assert_cli_success(&origin, "list --show-origin");
    let stdout = String::from_utf8_lossy(&origin.stdout);
    let line = stdout
        .lines()
        .find(|l| l.contains("upgrade.mode"))
        .unwrap_or_else(|| panic!("upgrade.mode line present: {stdout}"));
    assert!(
        line.contains("file:") && line.contains("settings.json"),
        "origin is file:{{path}}: {line}"
    );

    // A regexp pattern that can match the reserved key fails closed …
    let regexp = run_libra_command(&["config", "--global", "--get-regexp", "upgrade."], p);
    assert_eq!(
        regexp.status.code(),
        Some(129),
        "matching --get-regexp must fail closed: {}",
        String::from_utf8_lossy(&regexp.stderr)
    );
    // … while a non-matching pattern proceeds and still suppresses stale
    // SQLite `upgrade.*` rows (defense in depth).
    let regexp = run_libra_command(
        &["config", "--global", "--get-regexp", r"upgrade\.legacy"],
        p,
    );
    assert_cli_success(&regexp, "non-matching --get-regexp");
    assert_eq!(
        String::from_utf8_lossy(&regexp.stdout).trim(),
        "",
        "stale upgrade.legacy row must be suppressed"
    );
}

#[test]
fn test_config_upgrade_mode_import_skips_reserved() {
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("skipped (git binary not available)");
        return;
    }
    let temp = tempdir().unwrap();
    let p = temp.path();
    let home = p.join(".libra-test-home");
    std::fs::create_dir_all(&home).unwrap();

    // Write a Git global config containing a reserved key, using the same
    // isolated HOME the spawned libra binary sees.
    for kv in [
        ["user.name", "Import Reserved User"],
        ["upgrade.mode", "auto"],
    ] {
        let out = Command::new("git")
            .args(["config", "--global", kv[0], kv[1]])
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .output()
            .unwrap();
        assert!(out.status.success(), "git config {kv:?}");
    }

    let import = run_libra_command(&["config", "--global", "import"], p);
    assert_cli_success(&import, "import");
    let stderr = String::from_utf8_lossy(&import.stderr);
    assert!(
        stderr.contains("reserved upgrade.*"),
        "import warns about skipped reserved keys: {stderr}"
    );

    // The normal key imported; the reserved key did not touch file or SQLite.
    let get = run_libra_command(&["config", "get", "--global", "user.name"], p);
    assert_cli_success(&get, "imported user.name");
    assert_eq!(
        String::from_utf8_lossy(&get.stdout).trim(),
        "Import Reserved User"
    );
    assert!(
        !upgrade_settings_file(p).exists(),
        "import must not create settings.json"
    );
    let get = run_libra_command(&["config", "get", "--global", "upgrade.mode"], p);
    assert_cli_success(&get, "upgrade.mode after import");
    assert_eq!(String::from_utf8_lossy(&get.stdout).trim(), "off");
}

#[test]
fn test_config_upgrade_mode_json_spellings() {
    let temp = tempdir().unwrap();
    let p = temp.path();

    // JSON set ack.
    let set = run_libra_command(
        &[
            "config",
            "--json",
            "set",
            "--global",
            "upgrade.mode",
            "auto",
        ],
        p,
    );
    assert_cli_success(&set, "json set");
    let ack: serde_json::Value =
        serde_json::from_slice(&set.stdout).expect("json set emits a JSON ack");
    assert_eq!(ack["ok"], true, "{ack}");
    assert_eq!(ack["data"]["key"], "upgrade.mode", "{ack}");

    // JSON get reads the file, with a file origin.
    let get = run_libra_command(&["config", "--json", "get", "--global", "upgrade.mode"], p);
    assert_cli_success(&get, "json get");
    let doc: serde_json::Value = serde_json::from_slice(&get.stdout).unwrap();
    assert_eq!(doc["data"]["value"], "auto", "{doc}");
    let origin = doc["data"]["origin"].as_str().unwrap_or_default();
    assert!(
        origin.starts_with("file:") && origin.ends_with("settings.json"),
        "json get origin is file:{{path}}: {doc}"
    );

    // JSON unset resets to off, keeping the file.
    let unset = run_libra_command(
        &["config", "--json", "unset", "--global", "upgrade.mode"],
        p,
    );
    assert_cli_success(&unset, "json unset");
    let doc: serde_json::Value = serde_json::from_slice(&unset.stdout).unwrap();
    assert_eq!(doc["data"]["reset_to"], "off", "{doc}");
    assert!(upgrade_settings_file(p).exists());

    // The JSON spelling still cannot write into SQLite: the store has no
    // upgrade.* rows afterwards (a matching regexp is fail-closed, a
    // non-matching one returns nothing).
    let regexp = run_libra_command(
        &[
            "config",
            "--json",
            "--global",
            "--get-regexp",
            r"upgrade\.legacy",
        ],
        p,
    );
    assert_cli_success(&regexp, "json get-regexp non-matching");
    let doc: serde_json::Value = serde_json::from_slice(&regexp.stdout).unwrap();
    assert_eq!(
        doc["data"]["entries"].as_array().map(Vec::len),
        Some(0),
        "{doc}"
    );

    // JSON rejection envelope carries the usage stable code.
    let add = run_libra_command(
        &["config", "--json", "--global", "--add", "upgrade.mode", "x"],
        p,
    );
    assert_eq!(add.status.code(), Some(129));
    let envelope: serde_json::Value =
        serde_json::from_slice(&add.stderr).expect("usage rejection emits a JSON envelope");
    assert_eq!(envelope["error_code"], "LBR-CLI-002", "{envelope}");
    assert_eq!(envelope["ok"], false, "{envelope}");

    // JSON corrupt-file envelope carries LBR-UPGRADE-001.
    std::fs::write(upgrade_settings_file(p), b"{ corrupt").unwrap();
    let get = run_libra_command(&["config", "--json", "get", "--global", "upgrade.mode"], p);
    assert!(!get.status.success());
    let envelope: serde_json::Value =
        serde_json::from_slice(&get.stderr).expect("corrupt settings emits a JSON envelope");
    assert_eq!(envelope["error_code"], "LBR-UPGRADE-001", "{envelope}");
    assert_eq!(envelope["category"], "config", "{envelope}");
}

#[test]
#[cfg(unix)]
fn test_config_upgrade_mode_unreadable_file_uses_upgrade_code() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempdir().unwrap();
    let p = temp.path();
    let file = upgrade_settings_file(p);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, br#"{ "schema_version": 1, "mode": "auto" }"#).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();

    let get = run_libra_command(&["config", "get", "--global", "upgrade.mode"], p);
    // Restore permissions before asserting so the tempdir can be cleaned up.
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!get.status.success(), "unreadable settings must fail get");
    let stderr = String::from_utf8_lossy(&get.stderr);
    assert!(
        stderr.contains("LBR-UPGRADE-001"),
        "unreadable settings use the upgrade stable code: {stderr}"
    );
}

#[tokio::test]
#[serial]
async fn test_config_upgrade_mode_isolated_by_global_db_override() {
    // A test environment that isolates the global config DB via
    // LIBRA_CONFIG_GLOBAL_DB (without setting LIBRA_HOME) must also isolate
    // the upgrade settings instead of touching the real user's home.
    let temp_dir = tempdir().unwrap();
    let _guard = test::ChangeDirGuard::new(temp_dir.path());
    let store = tempdir().unwrap();
    let _libra_home = EnvVarGuard::unset("LIBRA_HOME");
    let _scoped = ScopedConfigPathGuard::new(&store.path().join("config.db"));

    let result = exec_config(vec!["config", "set", "--global", "upgrade.mode", "manual"]).await;
    assert!(result.is_ok(), "{result:?}");

    let isolated = store.path().join("upgrade").join("settings.json");
    assert!(
        isolated.exists(),
        "settings must be written next to the isolated global DB: {}",
        isolated.display()
    );
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&isolated).unwrap()).unwrap();
    assert_eq!(doc["mode"], "manual");
}

// ─────────────────────────────────────────────────────────────────────────────
// CT1-01 (plan-20260729): bare `libra config <key>` reads for ordinary keys.
//
// Git's `git config <key>` is a read. Libra previously routed a single-positional
// call with no value into set mode, which reported "missing value for key" with
// LBR-INTERNAL-001 / exit 2. Ordinary keys now read; PROTECTED keys deliberately
// keep Libra's interactive secure-assignment path (intentional divergence, see
// COMPATIBILITY.md and docs/development/commands/config.md).
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[serial]
fn config_bare_read_single_value() {
    let temp = tempdir().unwrap();
    let p = temp.path();
    init_repo_via_cli(p);
    assert_cli_success(
        &run_libra_command(&["config", "user.name", "A U Thor"], p),
        "seed user.name",
    );

    let bare = run_libra_command(&["config", "user.name"], p);
    assert_cli_success(&bare, "bare read");
    let via_get = run_libra_command(&["config", "get", "user.name"], p);
    assert_cli_success(&via_get, "config get");
    assert_eq!(
        bare.stdout, via_get.stdout,
        "bare read must be byte-identical to `config get`"
    );
    assert_eq!(String::from_utf8_lossy(&bare.stdout).trim(), "A U Thor");
}

#[test]
#[serial]
fn config_bare_read_multi_value_last_wins() {
    let temp = tempdir().unwrap();
    let p = temp.path();
    init_repo_via_cli(p);
    assert_cli_success(
        &run_libra_command(&["config", "--add", "multi.key", "one"], p),
        "add first",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--add", "multi.key", "two"], p),
        "add second",
    );

    let bare = run_libra_command(&["config", "multi.key"], p);
    assert_cli_success(&bare, "bare read of multi-valued key");
    assert_eq!(
        String::from_utf8_lossy(&bare.stdout).trim(),
        "two",
        "Git returns the LAST value for a multi-valued key"
    );
}

#[test]
#[serial]
fn config_bare_read_missing_key_exit1() {
    let temp = tempdir().unwrap();
    let p = temp.path();
    init_repo_via_cli(p);

    let bare = run_libra_command(&["config", "nosuch.key"], p);
    assert_eq!(
        bare.status.code(),
        Some(1),
        "missing key exits 1, not the old 2: {}",
        String::from_utf8_lossy(&bare.stderr)
    );
}

#[test]
#[serial]
fn config_bare_read_missing_key_error_code() {
    let temp = tempdir().unwrap();
    let p = temp.path();
    init_repo_via_cli(p);

    let bare = run_libra_command(&["config", "nosuch.key"], p);
    let stderr = String::from_utf8_lossy(&bare.stderr);
    assert!(
        stderr.contains("LBR-CLI-002"),
        "missing key must be a CLI error, not LBR-INTERNAL-001: {stderr}"
    );
    assert!(
        !stderr.contains("LBR-INTERNAL-001"),
        "user input must never surface as an internal error: {stderr}"
    );
}

#[test]
#[serial]
fn config_bare_read_sensitive_key_keeps_interactive_set() {
    let temp = tempdir().unwrap();
    let p = temp.path();
    init_repo_via_cli(p);

    // Protected keys keep the interactive secure-assignment path. Under the test
    // harness that path is non-interactive, so it reports the protected-key error
    // rather than reading. This is the pre-existing behaviour and must not change.
    let bare = run_libra_command(&["config", "user.password"], p);
    let stderr = String::from_utf8_lossy(&bare.stderr);
    assert!(
        stderr.contains("missing value for protected key"),
        "sensitive key must stay on the interactive-assignment path: {stderr}"
    );
    assert_eq!(
        bare.status.code(),
        Some(2),
        "protected-key path keeps exit 2"
    );
}

#[test]
#[serial]
fn config_bare_read_upgrade_namespace_fail_closed() {
    let temp = tempdir().unwrap();
    let p = temp.path();
    init_repo_via_cli(p);

    let bare = run_libra_command(&["config", "upgrade.mode"], p);
    assert!(
        !bare.status.success(),
        "reserved upgrade.* namespace must stay fail-closed on a bare read"
    );
    let stderr = String::from_utf8_lossy(&bare.stderr);
    assert!(
        stderr.contains("upgrade.*"),
        "error must name the reserved namespace: {stderr}"
    );
}

#[test]
#[serial]
fn config_bare_read_scope_cascade() {
    let temp = tempdir().unwrap();
    let p = temp.path();
    init_repo_via_cli(p);
    assert_cli_success(
        &run_libra_command(&["config", "--global", "cascade.key", "from-global"], p),
        "seed global",
    );
    let global_only = run_libra_command(&["config", "cascade.key"], p);
    assert_cli_success(&global_only, "cascade read (global only)");
    assert_eq!(
        String::from_utf8_lossy(&global_only.stdout).trim(),
        "from-global"
    );

    // Local must win over global, matching `config get`.
    assert_cli_success(
        &run_libra_command(&["config", "--local", "cascade.key", "from-local"], p),
        "seed local",
    );
    let bare = run_libra_command(&["config", "cascade.key"], p);
    assert_cli_success(&bare, "cascade read (local wins)");
    let via_get = run_libra_command(&["config", "get", "cascade.key"], p);
    assert_eq!(
        bare.stdout, via_get.stdout,
        "bare read cascade must match `config get` cascade"
    );
}

#[test]
#[serial]
fn config_bare_read_output_shape_human() {
    let temp = tempdir().unwrap();
    let p = temp.path();
    init_repo_via_cli(p);
    assert_cli_success(
        &run_libra_command(&["config", "shape.key", "v"], p),
        "seed shape.key",
    );

    let ok = run_libra_command(&["config", "shape.key"], p);
    assert_cli_success(&ok, "human success");
    assert_eq!(String::from_utf8_lossy(&ok.stdout).trim(), "v");

    let err = run_libra_command(&["config", "shape.missing"], p);
    assert_eq!(err.status.code(), Some(1), "human failure exit code");
    assert!(
        String::from_utf8_lossy(&err.stdout).trim().is_empty(),
        "human failure must not print a value on stdout"
    );
}

#[test]
#[serial]
fn config_bare_read_output_shape_json() {
    let temp = tempdir().unwrap();
    let p = temp.path();
    init_repo_via_cli(p);
    assert_cli_success(
        &run_libra_command(&["config", "shape.key", "v"], p),
        "seed shape.key",
    );

    let ok = run_libra_command(&["--json", "config", "shape.key"], p);
    assert_cli_success(&ok, "json success");
    let body = String::from_utf8_lossy(&ok.stdout);
    // `--json` pretty-prints, so match on the fields rather than a compact spelling.
    assert!(
        body.contains("\"ok\": true") && body.contains("\"value\": \"v\""),
        "json success payload: {body}"
    );

    let err = run_libra_command(&["--json", "config", "shape.missing"], p);
    assert_eq!(err.status.code(), Some(1), "json failure exit code");
    let ebody = String::from_utf8_lossy(&err.stderr);
    assert!(
        ebody.contains("\"ok\": false") && ebody.contains("LBR-CLI-002"),
        "json failure payload: {ebody}"
    );
    assert!(
        ebody.contains("\"exit_code\": 1"),
        "json failure must carry exit_code 1: {ebody}"
    );
}

#[test]
#[serial]
fn config_bare_read_output_shape_machine() {
    let temp = tempdir().unwrap();
    let p = temp.path();
    init_repo_via_cli(p);
    assert_cli_success(
        &run_libra_command(&["config", "shape.key", "v"], p),
        "seed shape.key",
    );

    let ok = run_libra_command(&["--machine", "config", "shape.key"], p);
    assert_cli_success(&ok, "machine success");
    // `--machine` emits a compact single-line JSON envelope (not a bare value).
    let body = String::from_utf8_lossy(&ok.stdout);
    assert!(
        body.contains("\"ok\":true") && body.contains("\"value\":\"v\""),
        "machine success payload: {body}"
    );
    assert_eq!(
        body.lines().count(),
        1,
        "machine output is single-line: {body}"
    );

    let err = run_libra_command(&["--machine", "config", "shape.missing"], p);
    assert_eq!(err.status.code(), Some(1), "machine failure exit code");
}

/// CT1-01 AC 9–11: an ORDINARY key whose stored value happens to be encrypted
/// still bare-reads, and it reads through `config get`'s `reveal=false`
/// rendering path — `<REDACTED>`, never the plaintext, never the ciphertext.
///
/// Before this card the stored `encrypted` flag was folded into the
/// protected-input decision, so `libra config ordinary.blob` reported
/// "missing value for protected key" with exit 2 instead of reading. Only an
/// explicit assignment (`config set <key>` / `--add`) may draw that inference
/// now; see `handle_set`.
#[test]
#[serial]
fn config_bare_read_encrypted_is_redacted() {
    const SENTINEL: &str = "ct101-plaintext-sentinel-must-never-print";

    let temp = tempdir().unwrap();
    let p = temp.path();
    init_repo_via_cli(p);
    assert_cli_success(
        &run_libra_command(
            &[
                "config",
                "set",
                "--local",
                "--encrypt",
                "ordinary.blob",
                SENTINEL,
            ],
            p,
        ),
        "seed an encrypted value under an ordinary (non-protected) key",
    );
    // `is_sensitive_key` classifies by the key's LAST segment, so the key here
    // must not spell secret/token/password/... — otherwise the test would be
    // exercising the protected path it is meant to stay clear of.

    let bare = run_libra_command(&["config", "ordinary.blob"], p);
    assert_cli_success(&bare, "bare read of an encrypted ordinary key");
    let stdout = String::from_utf8_lossy(&bare.stdout);
    let stderr = String::from_utf8_lossy(&bare.stderr);

    // Redacted, and byte-identical to the explicit read: same rendering path.
    assert_eq!(stdout.trim(), "<REDACTED>", "bare read stdout: {stdout}");
    let via_get = run_libra_command(&["config", "get", "ordinary.blob"], p);
    assert_cli_success(&via_get, "config get of the same key");
    assert_eq!(
        bare.stdout, via_get.stdout,
        "bare read must reuse `config get`'s reveal=false rendering"
    );

    // Does not decrypt.
    assert!(
        !stdout.contains(SENTINEL) && !stderr.contains(SENTINEL),
        "bare read must not decrypt the value: stdout={stdout} stderr={stderr}"
    );

    // Does not echo the ciphertext. Compare against the row as actually stored,
    // so this cannot pass by the value merely being spelled differently.
    let ciphertext = stored_config_value(p, "ordinary.blob");
    assert!(
        !ciphertext.is_empty() && ciphertext != SENTINEL,
        "the seeded row should hold ciphertext, not the plaintext: {ciphertext}"
    );
    assert!(
        !stdout.contains(&ciphertext) && !stderr.contains(&ciphertext),
        "bare read must not echo the stored ciphertext: stdout={stdout} stderr={stderr}"
    );
}

/// CT1-01 AC 1, with `-z` in play: "byte-identical to `config get`" has to
/// hold for the documented read flags too. The bare form used to hardcode
/// newline termination, so `config -z <key>` emitted `v\n` where
/// `config -z get <key>` emitted `v\0`.
#[test]
#[serial]
fn config_bare_read_null_terminated_matches_get() {
    let temp = tempdir().unwrap();
    let p = temp.path();
    init_repo_via_cli(p);
    assert_cli_success(
        &run_libra_command(&["config", "zed.key", "v"], p),
        "seed zed.key",
    );

    let bare = run_libra_command(&["config", "-z", "zed.key"], p);
    assert_cli_success(&bare, "bare read with -z");
    let via_get = run_libra_command(&["config", "-z", "get", "zed.key"], p);
    assert_cli_success(&via_get, "config get with -z");
    assert_eq!(
        bare.stdout, via_get.stdout,
        "-z bare read must be byte-identical to -z `config get`"
    );
    assert_eq!(bare.stdout, b"v\0", "-z terminates with NUL, not newline");
}

/// CT1-01 AC 7: the protected-key divergence must not become a disclosure
/// channel. The pinned `config_bare_read_sensitive_key_keeps_interactive_set`
/// asserts the error and exit code but seeds no secret, so nothing there would
/// notice a value leaking into the message. Seed one and check all three
/// output shapes, on both streams.
#[test]
#[serial]
fn config_bare_read_sensitive_key_never_leaks_value() {
    const SENTINEL: &str = "ct101-protected-sentinel-must-never-print";

    let temp = tempdir().unwrap();
    let p = temp.path();
    init_repo_via_cli(p);
    assert_cli_success(
        &run_libra_command(&["config", "user.password", SENTINEL], p),
        "seed a protected key",
    );

    for shape in [
        vec!["config", "user.password"],
        vec!["--json", "config", "user.password"],
        vec!["--machine", "config", "user.password"],
    ] {
        let out = run_libra_command(&shape, p);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stdout.contains(SENTINEL),
            "{shape:?} leaked the stored secret on stdout: {stdout}"
        );
        assert!(
            !stderr.contains(SENTINEL),
            "{shape:?} leaked the stored secret on stderr: {stderr}"
        );
        // Pin the failure class too: a shape that started SUCCEEDING would
        // also satisfy the two assertions above while having quietly turned
        // the protected key into a readable one.
        assert_eq!(
            out.status.code(),
            Some(2),
            "{shape:?} must stay on the protected-key path: {stderr}"
        );
        assert!(
            stderr.contains("missing value for protected key"),
            "{shape:?} must report the protected-key error: {stderr}"
        );
    }
}

/// CT1-01: the interactive secure-assignment branch a protected key keeps
/// (`src/command/config.rs`, the `rpassword` prompt) is unreachable from the
/// ordinary harness — `base_libra_command` always sets `LIBRA_TEST=1` and the
/// pipe is not a terminal, both of which short-circuit to the protected-key
/// error. Drive it on a real pty instead, which is what the plan's manual
/// evidence item called for.
#[test]
#[serial]
fn config_bare_read_protected_key_interactive_pty() {
    const SENTINEL: &str = "ct101-typed-sentinel-must-not-echo";

    let temp = tempdir().unwrap();
    let p = temp.path();
    init_repo_via_cli(p);

    let (ok, transcript) =
        run_config_in_pty(&["config", "user.password"], p, &format!("{SENTINEL}\n"));
    assert!(ok, "interactive assignment should succeed: {transcript}");
    assert!(
        transcript.contains("Enter value for user.password:"),
        "the no-echo prompt must appear: {transcript}"
    );
    assert!(
        !transcript.contains(SENTINEL),
        "the typed value must not be echoed by the terminal: {transcript}"
    );

    // The value did land, and reading it back stays redacted.
    let via_get = run_libra_command(&["config", "get", "user.password"], p);
    assert_cli_success(&via_get, "read back the interactively assigned value");
    let stdout = String::from_utf8_lossy(&via_get.stdout);
    assert_eq!(stdout.trim(), "<REDACTED>", "read back: {stdout}");
    assert!(
        !stdout.contains(SENTINEL),
        "read back must not reveal the value: {stdout}"
    );
}

/// The raw `config_kv` row as stored on disk, so a test can assert on the
/// ciphertext itself rather than on a re-rendering of it. Uses python3's
/// bundled sqlite3 — `sqlite3(1)` is not installed on every dev machine.
fn stored_config_value(repo: &std::path::Path, key: &str) -> String {
    let db = repo.join(".libra/libra.db");
    let out = Command::new("python3")
        .arg("-c")
        .arg(format!(
            "import sqlite3\nc = sqlite3.connect({db:?})\nr = c.execute('SELECT value FROM \
             config_kv WHERE key = ?', ({key:?},)).fetchone()\nprint(r[0] if r else '')\n"
        ))
        .output()
        .expect("read config_kv through python3 sqlite3");
    assert!(
        out.status.success(),
        "sqlite read failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Run a libra command on a pty and feed it `input`, so branches gated on
/// `stdin().is_terminal()` are reachable. `LIBRA_TEST` is deliberately NOT set:
/// it is the other half of the interactive short-circuit. Returns whether the
/// child exited successfully plus everything the pty saw.
fn run_config_in_pty(args: &[&str], cwd: &std::path::Path, input: &str) -> (bool, String) {
    use std::io::{Read, Write};

    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    let home = cwd.join(".libra-test-home");
    let config_home = home.join(".config");
    std::fs::create_dir_all(&config_home).expect("isolated config dir");

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open pty");

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_libra"));
    for arg in args {
        cmd.arg(arg);
    }
    cmd.cwd(cwd);
    cmd.env_clear();
    cmd.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    cmd.env("HOME", &home);
    cmd.env("USERPROFILE", &home);
    cmd.env("XDG_CONFIG_HOME", &config_home);
    cmd.env(
        "LIBRA_CONFIG_GLOBAL_DB",
        home.join(".libra").join("config.db"),
    );
    cmd.env("LANG", "C");
    cmd.env("LC_ALL", "C");
    cmd.env("TERM", "dumb");

    let mut child = pair.slave.spawn_command(cmd).expect("spawn under pty");
    drop(pair.slave);

    // Drain the master, or the child blocks once the pty buffer fills. The sink
    // is shared so the main thread can wait for the prompt before typing.
    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let sink = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let drain_sink = std::sync::Arc::clone(&sink);
    let drain = std::thread::spawn(move || {
        let mut buf = [0_u8; 4096];
        while let Ok(read) = reader.read(&mut buf) {
            if read == 0 {
                break;
            }
            drain_sink
                .lock()
                .expect("pty sink")
                .extend_from_slice(&buf[..read]);
        }
    });

    // Type only once the prompt is on screen. A fixed sleep would race the
    // child's switch out of echo mode, and typing into a still-echoing pty
    // would look exactly like the leak this test exists to catch.
    let prompt_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let seen = String::from_utf8_lossy(&sink.lock().expect("pty sink")).into_owned();
        if seen.contains("Enter value for") {
            break;
        }
        assert!(
            std::time::Instant::now() < prompt_deadline,
            "no interactive prompt within 30s on a pty; saw: {seen}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    {
        let mut writer = pair.master.take_writer().expect("pty writer");
        writer.write_all(input.as_bytes()).expect("type into pty");
        writer.flush().expect("flush pty");
    }

    // Watchdog: a regression that never reads stdin would otherwise hang CI.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let status = loop {
        match child.try_wait().expect("poll the pty child") {
            Some(status) => break status,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                panic!(
                    "`libra {}` did not exit within 60s on a pty",
                    args.join(" ")
                );
            }
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    };
    drop(pair.master);
    let _ = drain.join();
    let output = sink.lock().expect("pty sink").clone();
    (
        status.success(),
        String::from_utf8_lossy(&output).into_owned(),
    )
}
