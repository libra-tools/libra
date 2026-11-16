//! CF-15 (M-SUB Q1..Q8): Git 2.46+ subcommand spellings and legacy parity.

use std::fs;

use serial_test::serial;
use tempfile::tempdir;

use crate::command::{assert_cli_success, run_libra_command};

/// CF-15 (M-SUB Q1..Q7): subcommand spelling matrix.
#[test]
#[serial(cwd, env)]
fn test_subcommand_spellings_matrix() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo),
        "init",
    );

    // Q1: get --all --value / --fixed-value.
    assert_cli_success(
        &run_libra_command(&["config", "--local", "--add", "s.k", "one"], &repo),
        "add one",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--local", "--add", "s.k", "two"], &repo),
        "add two",
    );
    let all = run_libra_command(&["config", "get", "--local", "--all", "s.k"], &repo);
    assert_cli_success(&all, "get --all");
    assert_eq!(String::from_utf8_lossy(&all.stdout).trim(), "one\ntwo");

    let by_value = run_libra_command(&["config", "get", "--local", "--value=one", "s.k"], &repo);
    assert_cli_success(&by_value, "get --value=one");
    assert_eq!(String::from_utf8_lossy(&by_value.stdout).trim(), "one");

    let fixed = run_libra_command(
        &[
            "config",
            "get",
            "--local",
            "--all",
            "--fixed-value",
            "--value=two",
            "s.k",
        ],
        &repo,
    );
    assert_cli_success(&fixed, "get --fixed-value");
    assert_eq!(String::from_utf8_lossy(&fixed.stdout).trim(), "two");

    // Q2: get --regexp --show-origin / --show-scope.
    let with_origin = run_libra_command(
        &[
            "config",
            "get",
            "--local",
            "--regexp",
            "--show-origin",
            "^s",
        ],
        &repo,
    );
    assert_cli_success(&with_origin, "get --regexp --show-origin");
    let origin_out = String::from_utf8_lossy(&with_origin.stdout);
    assert!(origin_out.contains("file:"), "{origin_out:?}");
    assert!(origin_out.contains("s.k"), "{origin_out:?}");

    let with_scope = run_libra_command(&["config", "get", "--local", "--show-scope", "s.k"], &repo);
    assert_cli_success(&with_scope, "get --show-scope");
    assert!(
        String::from_utf8_lossy(&with_scope.stdout).contains("local"),
        "{}",
        String::from_utf8_lossy(&with_scope.stdout)
    );

    // Q3: set --all / --value / --append.
    assert_cli_success(
        &run_libra_command(
            &["config", "set", "--local", "--all", "s.k", "three"],
            &repo,
        ),
        "set --all",
    );
    let after_all = run_libra_command(&["config", "get", "--local", "--all", "s.k"], &repo);
    assert_eq!(String::from_utf8_lossy(&after_all.stdout).trim(), "three");

    assert_cli_success(
        &run_libra_command(
            &["config", "set", "--local", "--append", "s.k", "four"],
            &repo,
        ),
        "set --append",
    );
    let after_append = run_libra_command(&["config", "get", "--local", "--all", "s.k"], &repo);
    assert!(String::from_utf8_lossy(&after_append.stdout).contains("four"));

    assert_cli_success(
        &run_libra_command(
            &["config", "set", "--local", "--value=four", "s.k", "FOUR"],
            &repo,
        ),
        "set --value=four",
    );
    let after_value = run_libra_command(&["config", "get", "--local", "--all", "s.k"], &repo);
    let value_out = String::from_utf8_lossy(&after_value.stdout);
    assert!(value_out.contains("FOUR"), "{value_out:?}");
    assert!(value_out.contains("three"), "{value_out:?}");

    // Q4: a bare set on a multi-valued key exits 5 and names the subcommand flags.
    let multi = run_libra_command(&["config", "set", "--local", "s.k", "nope"], &repo);
    assert_eq!(multi.status.code(), Some(5));

    // Q5: unset --value / --all.
    assert_cli_success(
        &run_libra_command(
            &["config", "unset", "--local", "--value=FOUR", "s.k"],
            &repo,
        ),
        "unset --value",
    );
    let after_unset = run_libra_command(&["config", "get", "--local", "--all", "s.k"], &repo);
    assert!(!String::from_utf8_lossy(&after_unset.stdout).contains("FOUR"));

    assert_cli_success(
        &run_libra_command(&["config", "unset", "--local", "--all", "s.k"], &repo),
        "unset --all",
    );
    let gone = run_libra_command(&["config", "get", "--local", "s.k"], &repo);
    assert_eq!(gone.status.code(), Some(1));

    // Q6: list --show-scope / --show-origin / --name-only.
    assert_cli_success(
        &run_libra_command(&["config", "--local", "list.key", "v"], &repo),
        "seed list.key",
    );
    let scope_list = run_libra_command(&["config", "list", "--local", "--show-scope"], &repo);
    assert_cli_success(&scope_list, "list --show-scope");
    assert!(String::from_utf8_lossy(&scope_list.stdout).contains("local\t"));

    let origin_list = run_libra_command(&["config", "list", "--local", "--show-origin"], &repo);
    assert_cli_success(&origin_list, "list --show-origin");
    assert!(String::from_utf8_lossy(&origin_list.stdout).contains("file:"));

    let name_list = run_libra_command(&["config", "list", "--local", "--name-only"], &repo);
    assert_cli_success(&name_list, "list --name-only");
    assert!(String::from_utf8_lossy(&name_list.stdout).contains("list.key"));

    // Q7: rename-section / remove-section subcommands.
    assert_cli_success(
        &run_libra_command(
            &["config", "rename-section", "--local", "list", "renamed"],
            &repo,
        ),
        "rename-section",
    );
    let renamed = run_libra_command(&["config", "--local", "--get", "renamed.key"], &repo);
    assert_cli_success(&renamed, "read renamed");
    assert_eq!(String::from_utf8_lossy(&renamed.stdout).trim(), "v");

    assert_cli_success(
        &run_libra_command(&["config", "remove-section", "--local", "renamed"], &repo),
        "remove-section",
    );
    let gone_section = run_libra_command(&["config", "--local", "--get", "renamed.key"], &repo);
    assert_eq!(gone_section.status.code(), Some(1));
}

/// CF-15 (M-SUB Q8): legacy flags and subcommands produce identical output and
/// exit codes for the same semantics.
#[test]
#[serial(cwd, env)]
fn test_legacy_and_subcommand_parity_table() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo),
        "init",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--local", "parity.k", "v1"], &repo),
        "seed",
    );

    let pairs: [(&[&str], &[&str]); 5] = [
        (
            &["config", "--local", "--get", "parity.k"],
            &["config", "get", "--local", "parity.k"],
        ),
        (
            &["config", "--local", "--get-all", "parity.k"],
            &["config", "get", "--local", "--all", "parity.k"],
        ),
        (
            &["config", "--local", "--list", "--name-only"],
            &["config", "list", "--local", "--name-only"],
        ),
        (
            &["config", "--local", "--get", "missing.key"],
            &["config", "get", "--local", "missing.key"],
        ),
        (
            &["config", "--local", "--unset", "missing.key"],
            &["config", "unset", "--local", "missing.key"],
        ),
    ];

    for (legacy, subcommand) in pairs {
        let legacy_out = run_libra_command(legacy, &repo);
        let sub_out = run_libra_command(subcommand, &repo);
        assert_eq!(
            legacy_out.status.code(),
            sub_out.status.code(),
            "{legacy:?} vs {subcommand:?} exit"
        );
        assert_eq!(
            legacy_out.stdout, sub_out.stdout,
            "{legacy:?} vs {subcommand:?} stdout"
        );
    }
}
