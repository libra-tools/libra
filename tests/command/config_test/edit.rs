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
    let temp_log = temp.path().join("path.log");
    let script = editor_script(
        temp.path(),
        "mode.sh",
        &format!(
            "stat -c %a \"$1\" > {}; printf '%s' \"$1\" > {}; exit 0",
            log.display(),
            temp_log.display()
        ),
    );
    let out = run_libra_command_with_env(
        &["config", "--edit"],
        &repo,
        &[("GIT_EDITOR", script.as_str())],
    );
    assert_cli_success(&out, "edit");
    assert_eq!(fs::read_to_string(&log).unwrap().trim(), "600");
    let success_temp = fs::read_to_string(&temp_log).unwrap();
    assert!(
        !std::path::Path::new(&success_temp).exists(),
        "successful edit deletes {success_temp}"
    );

    let fail_log = temp.path().join("fail-path.log");
    let failing = editor_script(
        temp.path(),
        "fail.sh",
        &format!("printf '%s' \"$1\" > {}; exit 1", fail_log.display()),
    );
    let out = run_libra_command_with_env(
        &["config", "--edit"],
        &repo,
        &[("GIT_EDITOR", failing.as_str())],
    );
    assert!(!out.status.success(), "failing editor propagates");
    let failed_temp = fs::read_to_string(&fail_log).unwrap();
    assert!(
        !std::path::Path::new(&failed_temp).exists(),
        "failed editor deletes {failed_temp}"
    );
}

#[test]
#[serial(cwd, env)]
fn test_t1300_edit() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault=false"], &repo),
        "init",
    );
    let script = editor_script(
        temp.path(),
        "add.sh",
        "printf '\\n[t1300]\\n\\tedit = works\\n' >> \"$1\"",
    );
    let out = run_libra_command_with_env(
        &["config", "--edit"],
        &repo,
        &[("GIT_EDITOR", script.as_str())],
    );
    assert_cli_success(&out, "--edit works");
    let value = run_libra_command(&["config", "--local", "--get", "t1300.edit"], &repo);
    assert_cli_success(&value, "edited addition");
    assert_eq!(String::from_utf8_lossy(&value.stdout).trim(), "works");
}

#[test]
#[serial(cwd, env)]
fn test_cf_95_predicates() {
    const SENTINEL: &str = "cf95-encrypted-sentinel";
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(&run_libra_command(&["init"], &repo), "init");
    assert_cli_success(
        &run_libra_command(
            &[
                "config",
                "set",
                "--local",
                "--encrypt",
                "vault.env.CF95",
                SENTINEL,
            ],
            &repo,
        ),
        "encrypted seed",
    );
    let capture = temp.path().join("capture.log");
    let script = editor_script(
        temp.path(),
        "delete-placeholder.sh",
        &format!(
            "grep '<REDACTED>' \"$1\" > {}; grep -v '<REDACTED>' \"$1\" > \"$1.tmp\"; mv \"$1.tmp\" \"$1\"",
            capture.display()
        ),
    );
    let out = run_libra_command_with_env(
        &["config", "--edit"],
        &repo,
        &[("GIT_EDITOR", script.as_str())],
    );
    assert_cli_success(&out, "edit with placeholder deletion");
    let captured = fs::read_to_string(&capture).unwrap();
    assert!(
        captured.contains("#"),
        "placeholder is a comment: {captured}"
    );
    assert!(
        captured.contains("<REDACTED>"),
        "placeholder exported: {captured}"
    );
    let reveal = run_libra_command(
        &["config", "get", "--local", "--reveal", "vault.env.CF95"],
        &repo,
    );
    assert_cli_success(&reveal, "reveal encrypted");
    assert_eq!(String::from_utf8_lossy(&reveal.stdout).trim(), SENTINEL);
}

#[test]
#[serial(cwd, env)]
fn test_cf_97_predicates() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault=false"], &repo),
        "init",
    );
    let script = editor_script(
        temp.path(),
        "core-editor.sh",
        r#"printf '\n[core "editor"]\n\tran = yes\n' >> "$1""#,
    );
    assert_cli_success(
        &run_libra_command(&["config", "--local", "core.editor", &script], &repo),
        "set core.editor",
    );
    let out = run_libra_command_with_env(
        &["config", "--edit"],
        &repo,
        &[("GIT_EDITOR", "/definitely/missing-editor")],
    );
    assert_cli_success(&out, "core.editor takes precedence");
    let value = run_libra_command(&["config", "--local", "--get", "core.editor.ran"], &repo);
    assert_cli_success(&value, "core.editor script ran");
    assert_eq!(String::from_utf8_lossy(&value.stdout).trim(), "yes");

    let before = run_libra_command(&["config", "--local", "--list"], &repo);
    let missing = run_libra_command_with_env(
        &["config", "--edit"],
        &repo,
        &[
            ("GIT_EDITOR", ""),
            ("VISUAL", ""),
            ("EDITOR", ""),
            ("PATH", "/definitely/missing"),
        ],
    );
    assert!(!missing.status.success(), "no editor fails closed");
    let after = run_libra_command(&["config", "--local", "--list"], &repo);
    assert_eq!(before.stdout, after.stdout, "no editor writes nothing");
}

#[test]
#[serial(cwd, env)]
fn test_cf_96_predicates() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault=false"], &repo),
        "init",
    );
    let cfg = repo.join("explicit.cfg");
    fs::write(&cfg, "[a]\nb = c\n").unwrap();
    let script = editor_script(temp.path(), "file-edit.sh", "sed -i 's/= c/= d/' \"$1\"");
    let out = run_libra_command_with_env(
        &["config", "--file", "explicit.cfg", "--edit"],
        &repo,
        &[("GIT_EDITOR", script.as_str())],
    );
    assert_cli_success(&out, "file edit");
    assert_eq!(fs::read_to_string(&cfg).unwrap(), "[a]\nb = d\n");

    let stdin = run_libra_command_with_env(
        &["config", "--file", "-", "--edit"],
        &repo,
        &[("GIT_EDITOR", script.as_str())],
    );
    assert!(!stdin.status.success());
    assert!(String::from_utf8_lossy(&stdin.stderr).contains("editing stdin is not supported"));
    let blob = run_libra_command_with_env(
        &["config", "--blob", "HEAD:explicit.cfg", "--edit"],
        &repo,
        &[("GIT_EDITOR", script.as_str())],
    );
    assert!(!blob.status.success());
    assert!(String::from_utf8_lossy(&blob.stderr).contains("editing blobs is not supported"));
}
