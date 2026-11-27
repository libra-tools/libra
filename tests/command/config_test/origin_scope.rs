//! Origin and scope rendering tests for `libra config`.

use std::fs;

use serial_test::serial;
use tempfile::tempdir;

use crate::command::{assert_cli_success, run_libra_command, run_libra_command_with_env};

#[test]
#[serial(cwd, env)]
fn test_cf_53_predicates() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(&run_libra_command(&["init"], &repo), "init");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let xdg = home.join("xdg");
    let global = temp.path().join("global-dir").join("global.db");
    let system = temp.path().join("system-dir").join("system.db");
    fs::create_dir_all(global.parent().unwrap()).unwrap();
    fs::create_dir_all(system.parent().unwrap()).unwrap();
    let home_env = ("HOME", home.to_str().unwrap());
    let xdg_env = ("XDG_CONFIG_HOME", xdg.to_str().unwrap());
    let global_envs = [
        home_env,
        xdg_env,
        ("LIBRA_CONFIG_GLOBAL_DB", global.to_str().unwrap()),
    ];
    let system_envs = [
        home_env,
        xdg_env,
        ("LIBRA_CONFIG_SYSTEM_DB", system.to_str().unwrap()),
    ];
    assert_cli_success(
        &run_libra_command_with_env(
            &["config", "--local", "origin.key", "v"],
            &repo,
            &global_envs,
        ),
        "local",
    );
    assert_cli_success(
        &run_libra_command_with_env(
            &["config", "--global", "origin.key", "v"],
            &repo,
            &global_envs,
        ),
        "global",
    );
    assert_cli_success(
        &run_libra_command_with_env(
            &["config", "--system", "origin.key", "v"],
            &repo,
            &system_envs,
        ),
        "system",
    );
    let local = run_libra_command_with_env(
        &["config", "--local", "--show-origin", "--list"],
        &repo,
        &global_envs,
    );
    assert_cli_success(&local, "local origin");
    let line = String::from_utf8_lossy(&local.stdout)
        .lines()
        .find(|line| line.contains("origin.key=v"))
        .expect("local key")
        .to_string();
    assert!(line.starts_with("file:"), "{line}");
    assert!(line.contains('\t'), "{line}");

    for (scope, envs) in [("--global", &global_envs), ("--system", &system_envs)] {
        let out =
            run_libra_command_with_env(&["config", scope, "--show-origin", "--list"], &repo, envs);
        assert_cli_success(&out, scope);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("origin.key=v"), "{scope}: {text}");
    }
    let sub = run_libra_command_with_env(
        &["config", "list", "--local", "--show-origin"],
        &repo,
        &global_envs,
    );
    assert_cli_success(&sub, "list subcommand origin");
    assert!(
        String::from_utf8_lossy(&sub.stdout)
            .lines()
            .any(|line| line.starts_with("file:") && line.contains("origin.key=v"))
    );
}

#[test]
#[serial(cwd, env)]
fn test_cf_54_predicates() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo space");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(&run_libra_command(&["init"], &repo), "init");
    assert_cli_success(
        &run_libra_command(&["config", "--local", "space.key", "value"], &repo),
        "set",
    );
    let get = run_libra_command(
        &[
            "config",
            "--local",
            "-z",
            "--show-origin",
            "--get",
            "space.key",
        ],
        &repo,
    );
    assert_cli_success(&get, "nul get");
    assert!(get.stdout.starts_with(b"file:\""), "{:?}", get.stdout);
    assert!(get.stdout.ends_with(b"\0value\0"), "{:?}", get.stdout);
    let list = run_libra_command(
        &["config", "--local", "-z", "--show-origin", "--list"],
        &repo,
    );
    assert_cli_success(&list, "nul list");
    assert!(
        list.stdout
            .windows(b"\0space.key\nvalue\0".len())
            .any(|window| window == b"\0space.key\nvalue\0"),
        "{:?}",
        list.stdout
    );
    let default = run_libra_command(
        &[
            "config",
            "--local",
            "--show-origin",
            "--default",
            "fallback",
            "--get",
            "no.such",
        ],
        &repo,
    );
    assert_cli_success(&default, "default origin");
    assert_eq!(
        String::from_utf8_lossy(&default.stdout).trim(),
        "command line:\tfallback"
    );

    let quoted = repo.join("a\"b.cfg");
    fs::write(&quoted, "[a]\n\tb = c\n").unwrap();
    let file = run_libra_command(
        &[
            "config",
            "--file",
            quoted.to_str().unwrap(),
            "--show-origin",
            "--list",
        ],
        &repo,
    );
    assert_cli_success(&file, "quoted file origin");
    assert!(
        String::from_utf8_lossy(&file.stdout).contains("a\\\"b.cfg\""),
        "{}",
        String::from_utf8_lossy(&file.stdout)
    );
}

#[test]
#[serial(cwd, env)]
fn test_cf_55_predicates() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(&run_libra_command(&["init"], &repo), "init");
    assert_cli_success(
        &run_libra_command(&["config", "--local", "up.key", "value"], &repo),
        "set",
    );
    let get = run_libra_command(
        &["config", "--local", "--show-origin", "--get", "up.key"],
        &repo,
    );
    assert_cli_success(&get, "get origin");
    let line = String::from_utf8_lossy(&get.stdout);
    assert!(line.starts_with("file:"), "{line}");
    assert!(line.trim_end().ends_with("\tvalue"), "{line}");
    let list = run_libra_command(
        &[
            "config",
            "--local",
            "--show-origin",
            "--list",
            "--name-only",
        ],
        &repo,
    );
    assert_cli_success(&list, "name-only origin");
    assert!(
        String::from_utf8_lossy(&list.stdout)
            .lines()
            .any(|line| line.starts_with("file:") && line.ends_with("\tup.key"))
    );
    let default = run_libra_command(
        &[
            "config",
            "--local",
            "--show-origin",
            "--default",
            "d",
            "--get",
            "up.missing",
        ],
        &repo,
    );
    assert_cli_success(&default, "default");
    assert_eq!(
        String::from_utf8_lossy(&default.stdout).trim(),
        "command line:\td"
    );
}

#[test]
#[serial(cwd, env)]
fn test_cf_56_predicates() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(&run_libra_command(&["init"], &repo), "init");
    assert_cli_success(
        &run_libra_command(&["config", "--local", "scope.key", "local"], &repo),
        "local",
    );
    let local = run_libra_command(
        &["config", "--local", "--show-scope", "--get", "scope.key"],
        &repo,
    );
    assert_eq!(
        String::from_utf8_lossy(&local.stdout).trim(),
        "local\tlocal"
    );

    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let global = temp.path().join("global-dir").join("config.db");
    let system = temp.path().join("system-dir").join("config.db");
    fs::create_dir_all(global.parent().unwrap()).unwrap();
    fs::create_dir_all(system.parent().unwrap()).unwrap();
    let home_env = ("HOME", home.to_str().unwrap());
    let xdg = home.join("xdg");
    let global_envs = [
        home_env,
        ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
        ("LIBRA_CONFIG_GLOBAL_DB", global.to_str().unwrap()),
    ];
    let system_envs = [
        home_env,
        ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
        ("LIBRA_CONFIG_SYSTEM_DB", system.to_str().unwrap()),
    ];
    assert_cli_success(
        &run_libra_command_with_env(
            &["config", "--global", "scope.key", "global"],
            &repo,
            &global_envs,
        ),
        "global",
    );
    let global_out = run_libra_command_with_env(
        &["config", "--global", "--show-scope", "--get", "scope.key"],
        &repo,
        &global_envs,
    );
    assert_eq!(
        String::from_utf8_lossy(&global_out.stdout).trim(),
        "global\tglobal"
    );
    let system_repo = temp.path().join("system-repo");
    fs::create_dir_all(&system_repo).unwrap();
    assert_cli_success(&run_libra_command(&["init"], &system_repo), "system init");
    assert_cli_success(
        &run_libra_command_with_env(
            &["config", "--system", "scope.key", "system"],
            &system_repo,
            &system_envs,
        ),
        "system",
    );
    let system_out = run_libra_command_with_env(
        &["config", "--system", "--show-scope", "--get", "scope.key"],
        &system_repo,
        &system_envs,
    );
    assert_eq!(
        String::from_utf8_lossy(&system_out.stdout).trim(),
        "system\tsystem"
    );
}

#[test]
#[serial(cwd, env)]
fn test_show_origin_matrix() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault=false"], &repo),
        "init",
    );
    for value in ["one", "two"] {
        assert_cli_success(
            &run_libra_command(&["config", "--add", "origin.multi", value], &repo),
            "add",
        );
    }
    for args in [
        vec!["config", "--get", "origin.multi", "two"],
        vec!["config", "--get-all", "origin.multi"],
        vec!["config", "--get-regexp", "^origin\\."],
        vec!["config", "--list"],
    ] {
        let mut full: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        full.insert(2, "--show-origin".to_string());
        let refs: Vec<&str> = full.iter().map(String::as_str).collect();
        let out = run_libra_command(&refs, &repo);
        assert_cli_success(&out, &format!("{full:?}"));
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            assert!(line.starts_with("file:"), "{line}");
            assert!(line.contains('\t'), "{line}");
        }
    }
}

#[test]
#[serial(cwd, env)]
fn test_show_scope_matrix() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault=false"], &repo),
        "init",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--local", "scope.key", "local"], &repo),
        "local",
    );
    let get = run_libra_command(
        &["config", "--local", "--show-scope", "--get", "scope.key"],
        &repo,
    );
    assert_eq!(String::from_utf8_lossy(&get.stdout).trim(), "local\tlocal");
    let default = run_libra_command(
        &[
            "config",
            "--local",
            "--show-scope",
            "--default",
            "d",
            "--get",
            "scope.missing",
        ],
        &repo,
    );
    assert_eq!(
        String::from_utf8_lossy(&default.stdout).trim(),
        "command\td"
    );

    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let xdg = home.join("xdg");
    let global = temp.path().join("global-dir").join("config.db");
    fs::create_dir_all(global.parent().unwrap()).unwrap();
    let global_envs = [
        ("HOME", home.to_str().unwrap()),
        ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
        ("LIBRA_CONFIG_GLOBAL_DB", global.to_str().unwrap()),
    ];
    assert_cli_success(
        &run_libra_command_with_env(
            &["config", "--global", "scope.key", "global"],
            &repo,
            &global_envs,
        ),
        "global",
    );
    let list =
        run_libra_command_with_env(&["config", "--show-scope", "--list"], &repo, &global_envs);
    assert_cli_success(&list, "show-scope list");
    let list_text = String::from_utf8_lossy(&list.stdout);
    let lines: Vec<&str> = list_text.lines().collect();
    let global_idx = lines
        .iter()
        .position(|line| line.contains("scope.key=global"))
        .expect("global line");
    let local_idx = lines
        .iter()
        .position(|line| line.contains("scope.key=local"))
        .expect("local line");
    assert!(global_idx < local_idx, "global before local: {lines:?}");
    assert!(lines.iter().all(|line| line.contains('\t')));
    let combined = run_libra_command_with_env(
        &[
            "config",
            "--local",
            "--show-scope",
            "--show-origin",
            "--get",
            "scope.key",
        ],
        &repo,
        &global_envs,
    );
    let combined_text = String::from_utf8_lossy(&combined.stdout);
    let fields: Vec<&str> = combined_text.trim().split('\t').collect();
    assert_eq!(fields.len(), 3, "{combined_text}");
    assert_eq!(fields[0], "local");
    assert!(fields[1].starts_with("file:"));
    assert_eq!(fields[2], "local");
    let nul = run_libra_command_with_env(
        &["config", "-z", "--show-scope", "--list"],
        &repo,
        &global_envs,
    );
    assert!(
        nul.stdout
            .windows(b"global\0scope.key\nglobal\0".len())
            .any(|window| window == b"global\0scope.key\nglobal\0"),
        "{:?}",
        nul.stdout
    );
}

#[test]
#[serial(cwd, env)]
fn test_t1300_show_scope() {
    let repo = tempdir().unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault=false"], repo.path()),
        "init",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--local", "t.k", "v"], repo.path()),
        "set",
    );
    let out = run_libra_command(
        &["config", "--local", "--show-scope", "--get", "t.k"],
        repo.path(),
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "local\tv");
}

#[test]
#[serial(cwd, env)]
fn test_show_origin_json_and_upgrade_mode() {
    let repo = tempdir().unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault=false"], repo.path()),
        "init",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--local", "json.key", "v"], repo.path()),
        "set",
    );
    let get = run_libra_command(
        &[
            "--json",
            "config",
            "--local",
            "--show-origin",
            "--get",
            "json.key",
        ],
        repo.path(),
    );
    assert_cli_success(&get, "json get");
    let parsed: serde_json::Value = serde_json::from_slice(&get.stdout).expect("json");
    assert!(
        parsed["data"]["origin"]
            .as_str()
            .unwrap()
            .starts_with("file:"),
        "{parsed}"
    );
    assert_eq!(parsed["data"]["scope"], "local");

    let list = run_libra_command(
        &["--json", "config", "--local", "--show-origin", "--list"],
        repo.path(),
    );
    let parsed: serde_json::Value = serde_json::from_slice(&list.stdout).expect("json");
    let entry = parsed["data"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["key"] == "json.key")
        .expect("json.key entry");
    assert!(entry["origin"].as_str().unwrap().starts_with("file:"));
    assert_eq!(entry["scope"], "local");

    assert_cli_success(
        &run_libra_command(&["config", "--global", "upgrade.mode", "auto"], repo.path()),
        "upgrade mode",
    );
    let mode = run_libra_command(
        &["--json", "config", "get", "--global", "upgrade.mode"],
        repo.path(),
    );
    assert_cli_success(&mode, "upgrade mode get");
    let parsed: serde_json::Value = serde_json::from_slice(&mode.stdout).expect("json");
    assert!(
        parsed["data"]["origin"]
            .as_str()
            .unwrap()
            .starts_with("file:"),
        "{parsed}"
    );
}

#[test]
#[serial(cwd, env)]
fn test_t1300_show_origin() {
    let repo = tempdir().unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault=false"], repo.path()),
        "init",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--local", "t.key", "v"], repo.path()),
        "set",
    );
    let get = run_libra_command(&["config", "--show-origin", "--get", "t.key"], repo.path());
    assert_cli_success(&get, "show origin");
    assert!(
        String::from_utf8_lossy(&get.stdout)
            .trim_end()
            .ends_with("\tv")
    );
}
