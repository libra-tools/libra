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

/// CF-11 (M-EXPIRY E1..E6): `--type=expiry-date` renders timestamps,
/// validates values, and filters `--list`.
#[test]
#[serial(cwd, env)]
fn test_type_expiry_date_matrix() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo),
        "init",
    );

    // E1: relative forms produce a timestamp within 2s of Git's `now - delta`.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    for (value, delta) in [
        ("2.weeks.ago", 14 * 24 * 60 * 60),
        ("3.days.ago", 3 * 24 * 60 * 60),
        ("1.hour.ago", 60 * 60),
    ] {
        assert_cli_success(
            &run_libra_command(&["config", "--local", "d.v", value], &repo),
            "set expiry",
        );
        let out = run_libra_command(
            &["config", "--local", "--type=expiry-date", "--get", "d.v"],
            &repo,
        );
        assert_cli_success(&out, "get expiry");
        let ts: i64 = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("ts");
        assert!(
            ((now - delta) - ts).abs() <= 2,
            "{value}: expected ~{}, got {ts}",
            now - delta
        );
    }

    // E2: special words.
    assert_cli_success(
        &run_libra_command(&["config", "--local", "d.never", "never"], &repo),
        "set never",
    );
    let never = run_libra_command(
        &[
            "config",
            "--local",
            "--type=expiry-date",
            "--get",
            "d.never",
        ],
        &repo,
    );
    assert_eq!(String::from_utf8_lossy(&never.stdout).trim(), "0");

    assert_cli_success(
        &run_libra_command(&["config", "--local", "d.now", "now"], &repo),
        "set now",
    );
    let now_out = run_libra_command(
        &["config", "--local", "--type=expiry-date", "--get", "d.now"],
        &repo,
    );
    assert_eq!(
        String::from_utf8_lossy(&now_out.stdout).trim(),
        i64::MAX.to_string(),
        "now maps to the far-future sentinel"
    );

    // E3: absolute dates.
    assert_cli_success(
        &run_libra_command(
            &["config", "--local", "d.iso", "2017-11-11T23:11:11"],
            &repo,
        ),
        "set iso",
    );
    let iso = run_libra_command(
        &["config", "--local", "--type=expiry-date", "--get", "d.iso"],
        &repo,
    );
    assert_cli_success(&iso, "get iso");
    assert!(
        String::from_utf8_lossy(&iso.stdout)
            .trim()
            .parse::<i64>()
            .unwrap()
            > 0
    );

    // E4: invalid values fail the read with Git-shaped wording.
    assert_cli_success(
        &run_libra_command(&["config", "--local", "d.bad", "abc"], &repo),
        "set bad",
    );
    let bad = run_libra_command(
        &["config", "--local", "--type=expiry-date", "--get", "d.bad"],
        &repo,
    );
    assert_eq!(bad.status.code(), Some(129));
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("not a valid timestamp"),
        "invalid expiry wording: {}",
        String::from_utf8_lossy(&bad.stderr)
    );

    // E5: `--list --type=expiry-date` keeps only parseable values.
    let list = run_libra_command(
        &["config", "--local", "--list", "--type=expiry-date"],
        &repo,
    );
    assert_cli_success(&list, "list --type=expiry-date");
    let listed = String::from_utf8_lossy(&list.stdout);
    assert!(listed.contains("d.never=0"), "{listed:?}");
    assert!(
        !listed.contains("d.bad"),
        "invalid expiry filtered: {listed:?}"
    );

    // E6: `--expiry-date` shortcut equals `--type=expiry-date`; invalid set writes nothing.
    let shortcut = run_libra_command(
        &["config", "--local", "--expiry-date", "--get", "d.never"],
        &repo,
    );
    assert_cli_success(&shortcut, "--expiry-date shortcut");
    assert_eq!(String::from_utf8_lossy(&shortcut.stdout).trim(), "0");

    let invalid_set = run_libra_command(
        &["config", "--local", "--expiry-date", "d.news", "not-a-date"],
        &repo,
    );
    assert_eq!(invalid_set.status.code(), Some(129), "invalid set rejected");
    let stored = run_libra_command(&["config", "--local", "--get", "d.news"], &repo);
    assert_eq!(stored.status.code(), Some(1), "invalid set writes nothing");
}

/// CF-11 (M-EXPIRY E4): upstream t1300 `--expiry-date` get/list ports.
#[test]
#[serial(cwd, env)]
fn test_t1300_expiry_date() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo),
        "init",
    );

    // t1300:1115 — `never` → 0, and an invalid value fails.
    assert_cli_success(
        &run_libra_command(&["config", "--local", "date.valid5", "never"], &repo),
        "set never",
    );
    let valid5 = run_libra_command(
        &["config", "--local", "--expiry-date", "date.valid5"],
        &repo,
    );
    assert_cli_success(&valid5, "get never");
    assert_eq!(String::from_utf8_lossy(&valid5.stdout).trim(), "0");

    assert_cli_success(
        &run_libra_command(&["config", "--local", "date.invalid1", "abc"], &repo),
        "set invalid",
    );
    let invalid = run_libra_command(
        &["config", "--local", "--expiry-date", "date.invalid1"],
        &repo,
    );
    assert_eq!(invalid.status.code(), Some(129), "invalid expiry fails");

    // t1300:2627 — `--list --type=expiry-date` filters unparseable values.
    assert_cli_success(
        &run_libra_command(&["config", "--local", "date.good", "yesterday"], &repo),
        "set yesterday",
    );
    let list = run_libra_command(
        &["config", "--local", "--list", "--type=expiry-date"],
        &repo,
    );
    assert_cli_success(&list, "list expiry");
    let listed = String::from_utf8_lossy(&list.stdout);
    assert!(listed.contains("date.good="), "{listed:?}");
    assert!(!listed.contains("date.invalid1"), "{listed:?}");
}
