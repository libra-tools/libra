//! CF-14 / CF-22 / CF-94: `libra config --edit` editor round-trip.

use std::fs;

use serial_test::serial;
use tempfile::tempdir;

use crate::command::{assert_cli_success, run_libra_command, run_libra_command_with_env};

fn editor_script(dir: &std::path::Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    path.to_str().unwrap().to_string()
}

#[test]
#[serial(cwd, env)]
fn test_cf_94_predicates() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(&run_libra_command(&["init"], &repo), "init");
    assert_cli_success(
        &run_libra_command(&["config", "--local", "a.b", "c"], &repo),
        "seed",
    );
    let script = editor_script(
        temp.path(),
        "bad-editor.sh",
        "printf '[a]\\nbad line\\n' > \"$1\"",
    );
    let out = run_libra_command_with_env(
        &["config", "--edit"],
        &repo,
        &[("GIT_EDITOR", script.as_str())],
    );
    assert!(!out.status.success(), "syntax error exits nonzero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("failed to parse edited config"), "{stderr}");
    let kept = stderr
        .split("edits kept at ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .expect("temp path is reported");
    assert!(
        std::path::Path::new(kept).exists(),
        "kept temp file: {kept}"
    );
    assert!(
        fs::read_to_string(kept).unwrap().contains("bad line"),
        "kept edits are intact"
    );
    let listed = run_libra_command(&["config", "--local", "--list"], &repo);
    assert_cli_success(&listed, "list after failed edit");
    assert!(
        String::from_utf8_lossy(&listed.stdout)
            .lines()
            .any(|line| line == "a.b=c"),
        "syntax error keeps the seeded value"
    );
    let _ = fs::remove_file(kept);
}

#[test]
#[serial(cwd, env)]
fn test_config_edit_roundtrip_matrix() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault=false"], &repo),
        "init",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--local", "user.name", "Before"], &repo),
        "seed",
    );
    let script = editor_script(temp.path(), "edit.sh", "sed -i 's/Before/After/' \"$1\"");
    let envs = [("GIT_EDITOR", script.as_str())];
    for args in [
        vec!["config", "--edit"],
        vec!["config", "-e"],
        vec!["config", "edit"],
    ] {
        let out = run_libra_command_with_env(&args, &repo, &envs);
        assert_cli_success(&out, &format!("{args:?}"));
    }
    let value = run_libra_command(&["config", "--local", "--get", "user.name"], &repo);
    assert_cli_success(&value, "get edited value");
    assert_eq!(String::from_utf8_lossy(&value.stdout).trim(), "After");

    let global = run_libra_command_with_env(&["config", "--global", "--edit"], &repo, &envs);
    assert_cli_success(&global, "--global --edit");
    let global_sub = run_libra_command_with_env(&["config", "edit", "--global"], &repo, &envs);
    assert_cli_success(&global_sub, "config edit --global");
}

#[test]
#[serial(cwd, env)]
fn test_config_edit_tempfile_mode() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault=false"], &repo),
        "init",
    );
    let log = temp.path().join("mode.log");
    let script = editor_script(
        temp.path(),
        "mode.sh",
        &format!("stat -c %a \"$1\" > {}; exit 0", log.display()),
    );
    let out = run_libra_command_with_env(
        &["config", "--edit"],
        &repo,
        &[("GIT_EDITOR", script.as_str())],
    );
    assert_cli_success(&out, "edit");
    assert_eq!(fs::read_to_string(&log).unwrap().trim(), "600");

    let failing = editor_script(temp.path(), "fail.sh", "exit 1");
    let out = run_libra_command_with_env(
        &["config", "--edit"],
        &repo,
        &[("GIT_EDITOR", failing.as_str())],
    );
    assert!(!out.status.success(), "failing editor propagates");
}
