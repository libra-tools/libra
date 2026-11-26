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

#[test]
#[serial(cwd)]
fn test_cf_102_predicates() {
    let temp = tempdir().unwrap();
    let sub = temp.path().join("sub");
    let legacy = temp.path().join("legacy");
    for repo in [&sub, &legacy] {
        fs::create_dir_all(repo).unwrap();
        assert_cli_success(
            &run_libra_command(&["init", "--vault", "false"], repo),
            "init",
        );
        assert_cli_success(
            &run_libra_command(&["config", "--local", "old.k", "v"], repo),
            "set old.k",
        );
    }
    assert_cli_success(
        &run_libra_command(&["config", "rename-section", "--local", "old", "new"], &sub),
        "rename-section subcommand",
    );
    assert_cli_success(
        &run_libra_command(
            &["config", "--local", "--rename-section", "old", "new"],
            &legacy,
        ),
        "rename-section legacy",
    );
    for repo in [&sub, &legacy] {
        let out = run_libra_command(&["config", "--local", "--get", "new.k"], repo);
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "v");
    }
    assert_cli_success(
        &run_libra_command(&["config", "remove-section", "--local", "new"], &sub),
        "remove-section subcommand",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--local", "--remove-section", "new"], &legacy),
        "remove-section legacy",
    );
    for repo in [&sub, &legacy] {
        let out = run_libra_command(&["config", "--local", "--get", "new.k"], repo);
        assert_eq!(out.status.code(), Some(1));
    }
}

#[test]
#[serial(cwd)]
fn test_cf_100_predicates() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");
    for (legacy, sub) in [("sub.legacy", "sub.command")] {
        for key in [legacy, sub] {
            for value in ["one", "two"] {
                assert_cli_success(
                    &run_libra_command(&["config", "--add", key, value], repo.path()),
                    "add",
                );
            }
        }
    }
    let legacy = run_libra_command(&["config", "--unset", "sub.legacy", "^one$"], repo.path());
    let sub = run_libra_command(
        &["config", "unset", "sub.command", "--value=^one$"],
        repo.path(),
    );
    assert_eq!(
        legacy.status.code(),
        sub.status.code(),
        "patterned unset exit"
    );
    for key in ["sub.legacy", "sub.command"] {
        let left = run_libra_command(&["config", "--get-all", key], repo.path());
        assert_cli_success(&left, "remaining");
        assert_eq!(String::from_utf8_lossy(&left.stdout).trim(), "two");
    }

    for key in ["sub.legacy", "sub.command"] {
        for value in ["a", "b"] {
            assert_cli_success(
                &run_libra_command(&["config", "--add", key, value], repo.path()),
                "re-add",
            );
        }
    }
    let legacy_all = run_libra_command(&["config", "--unset-all", "sub.legacy"], repo.path());
    let sub_all = run_libra_command(&["config", "unset", "--all", "sub.command"], repo.path());
    assert_eq!(
        legacy_all.status.code(),
        sub_all.status.code(),
        "unset-all exit"
    );
    for key in ["sub.legacy", "sub.command"] {
        let gone = run_libra_command(&["config", "--get-all", key], repo.path());
        assert_eq!(gone.status.code(), Some(1), "all values removed");
    }

    let legacy_missing = run_libra_command(&["config", "--unset", "missing.legacy"], repo.path());
    let sub_missing = run_libra_command(&["config", "unset", "missing.command"], repo.path());
    assert_eq!(
        legacy_missing.status.code(),
        sub_missing.status.code(),
        "missing key exit"
    );
}

#[test]
#[serial(cwd)]
fn test_cf_101_predicates() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");
    for (key, value) in [("list.one", "1"), ("list.two", "2")] {
        assert_cli_success(
            &run_libra_command(&["config", "--local", key, value], repo.path()),
            "set",
        );
    }
    for flag in ["--show-scope", "--show-origin", "--name-only"] {
        let legacy = run_libra_command(&["config", "--local", "--list", flag], repo.path());
        let sub = run_libra_command(&["config", "list", "--local", flag], repo.path());
        assert_cli_success(&legacy, flag);
        assert_cli_success(&sub, flag);
        assert_eq!(legacy.stdout, sub.stdout, "{flag} parity");
    }
}

#[test]
#[serial(cwd)]
fn test_cf_99_predicates() {
    let repo = tempdir().unwrap();
    assert_cli_success(&run_libra_command(&["init"], repo.path()), "init");

    assert_cli_success(
        &run_libra_command(
            &["config", "set", "--local", "legacy.k", "one"],
            repo.path(),
        ),
        "legacy all seed",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--add", "legacy.k", "two"], repo.path()),
        "legacy all add",
    );
    assert_cli_success(
        &run_libra_command(
            &["config", "--replace-all", "legacy.k", "three"],
            repo.path(),
        ),
        "legacy replace",
    );

    assert_cli_success(
        &run_libra_command(&["config", "set", "--local", "sub.k", "one"], repo.path()),
        "sub all seed",
    );
    assert_cli_success(
        &run_libra_command(
            &["config", "set", "--local", "--append", "sub.k", "two"],
            repo.path(),
        ),
        "append",
    );
    assert_cli_success(
        &run_libra_command(
            &["config", "set", "--local", "--all", "sub.k", "three"],
            repo.path(),
        ),
        "set all",
    );
    for key in ["legacy.k", "sub.k"] {
        let values = run_libra_command(&["config", "--get-all", key], repo.path());
        assert_eq!(
            String::from_utf8_lossy(&values.stdout).trim(),
            "three",
            "{key}"
        );
    }

    assert_cli_success(
        &run_libra_command(&["config", "--add", "sub.k", "four"], repo.path()),
        "add four",
    );
    assert_cli_success(
        &run_libra_command(
            &[
                "config",
                "set",
                "--local",
                "--value=^three$",
                "sub.k",
                "five",
            ],
            repo.path(),
        ),
        "set value",
    );
    let values = run_libra_command(&["config", "--get-all", "sub.k"], repo.path());
    assert_eq!(String::from_utf8_lossy(&values.stdout).trim(), "five\nfour");

    let ambiguous = run_libra_command(&["config", "set", "--local", "sub.k", "six"], repo.path());
    assert_eq!(ambiguous.status.code(), Some(5), "multi set exits 5");
    assert!(
        String::from_utf8_lossy(&ambiguous.stderr)
            .contains("Use --value=<pattern>, --append or --all"),
        "hint: {}",
        String::from_utf8_lossy(&ambiguous.stderr)
    );
}
