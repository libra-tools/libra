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
