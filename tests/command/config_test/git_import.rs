//! Git global-import scenarios with caller-configuration isolation.

use super::*;

const CALLER_GIT_CONFIG: &[u8] = b"[fixture]\n\tpreserved = yes\n";

pub(super) async fn import_global_from_git_fixture() {
    // Given a valid writable global override owned by a different caller.
    let caller = tempdir().unwrap();
    let caller_config = caller.path().join("caller.gitconfig");
    std::fs::write(&caller_config, CALLER_GIT_CONFIG).unwrap();
    let inherited = std::env::var_os("GIT_CONFIG_GLOBAL");
    {
        let _incoming = EnvVarGuard::set("GIT_CONFIG_GLOBAL", caller_config.as_os_str());
        {
            let temp_dir = tempdir().unwrap();
            let _guard = test::ChangeDirGuard::new(temp_dir.path());

            let global_db_dir = tempdir().unwrap();
            let _scoped =
                ScopedConfigPathGuard::new(&global_db_dir.path().join("global_config_import.db"));

            let fake_home = tempdir().unwrap();
            let _home_guard = EnvVarGuard::set("HOME", fake_home.path().as_os_str());
            let _xdg_guard = EnvVarGuard::set(
                "XDG_CONFIG_HOME",
                fake_home.path().join(".config").as_os_str(),
            );
            let _git_global_guard = EnvVarGuard::set(
                "GIT_CONFIG_GLOBAL",
                fake_home.path().join(".gitconfig").as_os_str(),
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

            assert_eq!(
                std::fs::read(&caller_config).unwrap(),
                CALLER_GIT_CONFIG,
                "Git fixture writes must not modify the caller global configuration"
            );

            let result = exec_config(vec!["config", "--global", "import"]).await;
            assert!(result.is_ok());

            let imported_name = config::ScopedConfig::get(config::ConfigScope::Global, "user.name")
                .await
                .unwrap();
            let imported_email =
                config::ScopedConfig::get(config::ConfigScope::Global, "user.email")
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
        // Then the fixture restores its caller and leaves the caller's file intact.
        assert_eq!(
            std::env::var_os("GIT_CONFIG_GLOBAL").as_deref(),
            Some(caller_config.as_os_str())
        );
        assert_eq!(std::fs::read(&caller_config).unwrap(), CALLER_GIT_CONFIG);
    }
    assert!(
        std::env::var_os("GIT_CONFIG_GLOBAL") == inherited,
        "the original Git global override must be restored"
    );
}

pub(super) fn import_reserved_from_git_fixture() {
    let temp = tempdir().unwrap();
    let p = temp.path();
    if Command::new("git")
        .arg("--version")
        .current_dir(p)
        .output()
        .is_err()
    {
        eprintln!("skipped (git binary not available)");
        return;
    }
    let home = p.join(".libra-test-home");
    std::fs::create_dir_all(&home).unwrap();
    let caller_config = p.join("caller.gitconfig");
    std::fs::write(&caller_config, CALLER_GIT_CONFIG).unwrap();

    // Write a Git global config containing a reserved key, using the same
    // isolated HOME the spawned libra binary sees.
    for kv in [
        ["user.name", "Import Reserved User"],
        ["upgrade.mode", "auto"],
    ] {
        let mut writer = Command::new("git");
        writer
            .args(["config", "--global", kv[0], kv[1]])
            .current_dir(p)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("GIT_CONFIG_GLOBAL", &caller_config);
        let out = run_git_global_writer(&mut writer).unwrap();
        assert!(out.status.success(), "git config {kv:?}");
    }
    assert_eq!(
        std::fs::read(&caller_config).unwrap(),
        CALLER_GIT_CONFIG,
        "Git fixture writes must not modify the caller global configuration"
    );

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
    assert_eq!(std::fs::read(&caller_config).unwrap(), CALLER_GIT_CONFIG);
}

fn run_git_global_writer(command: &mut Command) -> std::io::Result<std::process::Output> {
    command.env_remove("GIT_CONFIG_GLOBAL").output()
}
