//! CF-09..CF-11 (M-BOS / M-COLOR / M-EXPIRY): `--type` extension tests.

use std::fs;

use serial_test::serial;
use tempfile::tempdir;

use crate::command::{assert_cli_success, run_libra_command, run_libra_command_with_env};

/// CF-10 (M-COLOR C1..C5): `--type=color` renders Git's ANSI sequences,
/// validates writes, and filters `--list`.
#[test]
#[serial(cwd, env)]
fn test_type_color_matrix() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo),
        "init",
    );

    // C1: ANSI sequences match Git.
    let cases = [
        ("red", "\x1b[31m"),
        ("bold red blue", "\x1b[1;31;44m"),
        ("255", "\x1b[38;5;255m"),
        ("#ff0000", "\x1b[38;2;255;0;0m"),
        ("reverse", "\x1b[7m"),
        ("nobold", "\x1b[22m"),
        ("normal", ""),
        ("reset", "\x1b[m"),
    ];
    for (input, expected) in cases {
        assert_cli_success(
            &run_libra_command(&["config", "--local", "t.c", input], &repo),
            "set color",
        );
        let out = run_libra_command(
            &["config", "--local", "--type=color", "--get", "t.c"],
            &repo,
        );
        assert_cli_success(&out, "get color");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("{expected}\n"),
            "unexpected ANSI for {input}"
        );
    }

    // C2: writes store the original spelling, not the escape sequence.
    assert_cli_success(
        &run_libra_command(&["config", "--local", "t.color", "red"], &repo),
        "set t.color red",
    );
    let raw = run_libra_command(&["config", "--local", "--get", "t.color"], &repo);
    assert_cli_success(&raw, "get raw color");
    assert_eq!(String::from_utf8_lossy(&raw.stdout).trim(), "red");

    // C3: invalid color values fail get and set (set writes nothing).
    let invalid_get = run_libra_command(
        &["config", "--local", "--type=color", "--get", "t.bad"],
        &repo,
    );
    assert_eq!(invalid_get.status.code(), Some(1), "missing key exits 1");

    assert_cli_success(
        &run_libra_command(&["config", "--local", "t.bad", "bogus"], &repo),
        "set t.bad bogus",
    );
    let invalid_get = run_libra_command(
        &["config", "--local", "--type=color", "--get", "t.bad"],
        &repo,
    );
    assert_eq!(
        invalid_get.status.code(),
        Some(129),
        "invalid color get exits 129"
    );
    assert!(
        String::from_utf8_lossy(&invalid_get.stderr).contains("invalid color value"),
        "invalid color wording: {}",
        String::from_utf8_lossy(&invalid_get.stderr)
    );

    let invalid_set = run_libra_command(
        &["config", "--local", "--type=color", "t.news", "not-a-color"],
        &repo,
    );
    assert_eq!(
        invalid_set.status.code(),
        Some(129),
        "invalid color set exits 129"
    );
    let stored = run_libra_command(&["config", "--local", "--get", "t.news"], &repo);
    assert_eq!(
        stored.status.code(),
        Some(1),
        "invalid color set writes nothing"
    );

    // C4: `--list --type=color` keeps only values that parse.
    assert_cli_success(
        &run_libra_command(&["config", "--local", "list.valid", "red"], &repo),
        "set valid",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--local", "list.invalid", "nope"], &repo),
        "set invalid",
    );
    let listed = run_libra_command(&["config", "--local", "--list", "--type=color"], &repo);
    assert_cli_success(&listed, "list --type=color");
    let listed_stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(
        listed_stdout.contains("list.valid=\x1b[31m"),
        "valid color listed with ANSI: {listed_stdout:?}"
    );
    assert!(
        !listed_stdout.contains("list.invalid"),
        "invalid color must be filtered: {listed_stdout:?}"
    );

    // C5: JSON carries the ANSI sequence as the value.
    let json = run_libra_command(
        &[
            "--machine",
            "config",
            "--local",
            "--type=color",
            "--get",
            "t.color",
        ],
        &repo,
    );
    assert_cli_success(&json, "machine get color");
    let parsed: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json");
    assert_eq!(parsed["data"]["value"], "\u{1b}[31m");
}

/// CF-10 (M-COLOR C2): upstream t1300 `--type=color` get/set/default ports.
#[test]
#[serial(cwd, env)]
fn test_t1300_type_color() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo),
        "init",
    );

    // t1300:1146 — `git config --get --type=color foo.color` (red).
    assert_cli_success(
        &run_libra_command(&["config", "--local", "foo.color", "red"], &repo),
        "set foo.color",
    );
    let get = run_libra_command(
        &["config", "--local", "--get", "--type=color", "foo.color"],
        &repo,
    );
    assert_cli_success(&get, "get --type=color");
    assert_eq!(get.stdout, b"\x1b[31m\n");

    // t1300:1157 — default only (`--default=red`).
    let with_default = run_libra_command(
        &[
            "config",
            "--local",
            "--type=color",
            "--default",
            "red",
            "--get",
            "no.such",
        ],
        &repo,
    );
    assert_cli_success(&with_default, "get color default");
    assert_eq!(with_default.stdout, b"\x1b[31m\n");

    // t1300:1174 — set validates and stores the original spelling.
    assert_cli_success(
        &run_libra_command(
            &["config", "--local", "--type=color", "foo.new", "red"],
            &repo,
        ),
        "set --type=color",
    );
    let raw = run_libra_command(&["config", "--local", "--get", "foo.new"], &repo);
    assert_eq!(String::from_utf8_lossy(&raw.stdout).trim(), "red");

    // t1300:1184/1189 — non-color get/set fail.
    assert_cli_success(
        &run_libra_command(&["config", "--local", "foo.bar", "not-a-color"], &repo),
        "set foo.bar",
    );
    let bad_get = run_libra_command(
        &["config", "--local", "--get", "--type=color", "foo.bar"],
        &repo,
    );
    assert_eq!(bad_get.status.code(), Some(129));
    let bad_set = run_libra_command(
        &[
            "config",
            "--local",
            "--type=color",
            "foo.color2",
            "not-a-color",
        ],
        &repo,
    );
    assert_eq!(bad_set.status.code(), Some(129));
    assert!(
        String::from_utf8_lossy(&bad_set.stderr).contains("invalid color value")
            || String::from_utf8_lossy(&bad_set.stderr).contains("cannot parse color"),
        "invalid set wording: {}",
        String::from_utf8_lossy(&bad_set.stderr)
    );

    // t1300:2638 — `--list --type=color` filters unparseable values.
    assert_cli_success(
        &run_libra_command(&["config", "--local", "t1300.good", "blue"], &repo),
        "set t1300.good",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--local", "t1300.bad", "definitely-not"], &repo),
        "set t1300.bad",
    );
    let list = run_libra_command(&["config", "--local", "--list", "--type=color"], &repo);
    assert_cli_success(&list, "list --type=color");
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_stdout.contains("t1300.good=\x1b[34m"),
        "{list_stdout:?}"
    );
    assert!(!list_stdout.contains("t1300.bad"), "{list_stdout:?}");

    let _ = run_libra_command_with_env(&["config", "--local", "--list"], &repo, &[]);
}
