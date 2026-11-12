//! CF-04 (M-FMT F1..F8): Output format, ordering, and default scope tests for `libra config`.

use std::fs;

use serial_test::serial;
use tempfile::tempdir;

use crate::command::{assert_cli_success, run_libra_command};

/// CF-04 (M-FMT F1..F7): Comprehensive test matrix for `--list` and `--get-regexp`
/// ordering, formatting, scopes, and `--name-only`.
#[test]
#[serial(cwd, env)]
fn test_list_and_get_regexp_order_format_matrix() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    // Init repository
    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo),
        "init repo",
    );

    // Set global values in isolated environment
    assert_cli_success(
        &run_libra_command(&["config", "--global", "global.first", "g1"], &repo),
        "set global.first",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--global", "global.second", "g2"], &repo),
        "set global.second",
    );

    // Set local values in non-alphabetical insertion order: zz, aa, mm
    assert_cli_success(
        &run_libra_command(&["config", "--local", "zz.key", "1"], &repo),
        "set zz.key",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--local", "aa.key", "2"], &repo),
        "set aa.key 2",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--local", "--add", "aa.key", "4"], &repo),
        "add aa.key 4",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--local", "mm.key", "3"], &repo),
        "set mm.key",
    );

    // ─────────────────────────────────────────────────────────────────────────
    // F1: Default `--list` lists all scopes: global first, then local in insertion order
    // ─────────────────────────────────────────────────────────────────────────
    let out_list = run_libra_command(&["config", "--list"], &repo);
    assert_cli_success(&out_list, "config --list");
    let stdout_list = String::from_utf8_lossy(&out_list.stdout);
    let list_lines: Vec<&str> = stdout_list.lines().collect();

    // Global entries appear before local entries
    let g1_idx = list_lines
        .iter()
        .position(|l| l.starts_with("global.first="))
        .expect("global.first");
    let g2_idx = list_lines
        .iter()
        .position(|l| l.starts_with("global.second="))
        .expect("global.second");
    let zz_idx = list_lines
        .iter()
        .position(|l| l.starts_with("zz.key="))
        .expect("zz.key");
    let aa2_idx = list_lines
        .iter()
        .position(|l| *l == "aa.key=2")
        .expect("aa.key=2");
    let aa4_idx = list_lines
        .iter()
        .position(|l| *l == "aa.key=4")
        .expect("aa.key=4");
    let mm_idx = list_lines
        .iter()
        .position(|l| l.starts_with("mm.key="))
        .expect("mm.key");

    assert!(g1_idx < g2_idx, "global entries in insertion order");
    assert!(g2_idx < zz_idx, "global entries precede local entries");
    assert!(zz_idx < aa2_idx, "zz precedes aa (insertion order)");
    assert!(
        aa2_idx < aa4_idx,
        "aa.key=2 precedes aa.key=4 (multi-value insertion order)"
    );
    assert!(aa4_idx < mm_idx, "aa precedes mm (insertion order)");

    // `config list` subcommand has identical behavior
    let out_sub_list = run_libra_command(&["config", "list"], &repo);
    assert_cli_success(&out_sub_list, "config list");
    assert_eq!(out_sub_list.stdout, out_list.stdout);

    // ─────────────────────────────────────────────────────────────────────────
    // F2: `--get-regexp` formats as `key value` (space-delimited) and preserves order
    // ─────────────────────────────────────────────────────────────────────────
    let out_re = run_libra_command(&["config", "--get-regexp", r"\.key$"], &repo);
    assert_cli_success(&out_re, "config --get-regexp");
    let stdout_re = String::from_utf8_lossy(&out_re.stdout);
    let re_lines: Vec<&str> = stdout_re.lines().collect();
    assert_eq!(
        re_lines,
        vec!["zz.key 1", "aa.key 2", "aa.key 4", "mm.key 3"]
    );

    // ─────────────────────────────────────────────────────────────────────────
    // F3: `-z --list` and `-z --get-regexp` output `key\nvalue\0`
    // ─────────────────────────────────────────────────────────────────────────
    let out_z_re = run_libra_command(&["config", "-z", "--get-regexp", r"\.key$"], &repo);
    assert_cli_success(&out_z_re, "-z --get-regexp");
    assert_eq!(
        out_z_re.stdout,
        b"zz.key\n1\0aa.key\n2\0aa.key\n4\0mm.key\n3\0"
    );

    // ─────────────────────────────────────────────────────────────────────────
    // F4: `--name-only` outputs only keys; `-z` outputs `key\0`
    // ─────────────────────────────────────────────────────────────────────────
    let out_name_re =
        run_libra_command(&["config", "--name-only", "--get-regexp", r"\.key$"], &repo);
    assert_cli_success(&out_name_re, "--name-only --get-regexp");
    let name_re_str = String::from_utf8_lossy(&out_name_re.stdout);
    let name_re_lines: Vec<&str> = name_re_str.lines().collect();
    assert_eq!(name_re_lines, vec!["zz.key", "aa.key", "aa.key", "mm.key"]);

    let out_name_z_re = run_libra_command(
        &["config", "-z", "--name-only", "--get-regexp", r"\.key$"],
        &repo,
    );
    assert_cli_success(&out_name_z_re, "-z --name-only --get-regexp");
    assert_eq!(out_name_z_re.stdout, b"zz.key\0aa.key\0aa.key\0mm.key\0");

    let out_name_list = run_libra_command(&["config", "--name-only", "--local", "--list"], &repo);
    assert_cli_success(&out_name_list, "--name-only --local --list");
    let name_list_str = String::from_utf8_lossy(&out_name_list.stdout);
    let name_list_lines: Vec<&str> = name_list_str
        .lines()
        .filter(|l| l.ends_with(".key"))
        .collect();
    assert_eq!(
        name_list_lines,
        vec!["zz.key", "aa.key", "aa.key", "mm.key"]
    );

    // ─────────────────────────────────────────────────────────────────────────
    // F5: Explicit scope `--local --list` outputs only local entries
    // ─────────────────────────────────────────────────────────────────────────
    let out_local_list = run_libra_command(&["config", "--local", "--list"], &repo);
    assert_cli_success(&out_local_list, "--local --list");
    let stdout_local = String::from_utf8_lossy(&out_local_list.stdout);
    assert!(!stdout_local.contains("global.first"));
    assert!(stdout_local.contains("zz.key=1"));

    // ─────────────────────────────────────────────────────────────────────────
    // F6: JSON output has entries with `scope` field matching human order
    // ─────────────────────────────────────────────────────────────────────────
    let out_json_list = run_libra_command(&["--machine", "config", "--list"], &repo);
    assert_cli_success(&out_json_list, "--machine config --list");
    let json_list: serde_json::Value =
        serde_json::from_slice(&out_json_list.stdout).expect("parse JSON list");
    assert_eq!(json_list["ok"], true);
    assert_eq!(json_list["data"]["scope"], "all");
    let entries = json_list["data"]["entries"]
        .as_array()
        .expect("entries array");
    assert!(
        entries
            .iter()
            .any(|e| e["scope"] == "global" && e["key"] == "global.first")
    );
    assert!(
        entries
            .iter()
            .any(|e| e["scope"] == "local" && e["key"] == "zz.key")
    );

    let out_json_re = run_libra_command(&["--machine", "config", "--get-regexp", r"\.key$"], &repo);
    assert_cli_success(&out_json_re, "--machine config --get-regexp");
    let json_re: serde_json::Value =
        serde_json::from_slice(&out_json_re.stdout).expect("parse JSON get-regexp");
    assert_eq!(json_re["ok"], true);
    assert_eq!(json_re["data"]["scope"], "all");
    let re_entries = json_re["data"]["entries"]
        .as_array()
        .expect("re entries array");
    assert_eq!(re_entries.len(), 4);
    assert_eq!(re_entries[0]["key"], "zz.key");
    assert_eq!(re_entries[0]["value"], "1");
    assert_eq!(re_entries[0]["scope"], "local");
}

/// CF-04 (M-FMT F8): Upstream Git t1300 ports covering --get-regexp and --name-only.
#[test]
#[serial(cwd, env)]
fn test_t1300_get_regexp_name_only() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();

    assert_cli_success(
        &run_libra_command(&["init", "--vault", "false"], &repo),
        "init repo",
    );

    // t1300:580-640
    assert_cli_success(
        &run_libra_command(&["config", "--local", "section.val", "hello"], &repo),
        "set section.val",
    );
    assert_cli_success(
        &run_libra_command(&["config", "--local", "section.empty", ""], &repo),
        "set section.empty",
    );

    let out = run_libra_command(&["config", "--local", "--get-regexp", r"section\."], &repo);
    assert_cli_success(&out, "get-regexp");
    let stdout_str = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout_str.lines().collect();
    assert_eq!(lines, vec!["section.val hello", "section.empty "]);

    let out_name = run_libra_command(
        &[
            "config",
            "--local",
            "--name-only",
            "--get-regexp",
            r"section\.",
        ],
        &repo,
    );
    assert_cli_success(&out_name, "--name-only --get-regexp");
    let name_str = String::from_utf8_lossy(&out_name.stdout);
    let name_lines: Vec<&str> = name_str.lines().collect();
    assert_eq!(name_lines, vec!["section.val", "section.empty"]);

    // t1300:1266 --null --get-regexp
    let out_null = run_libra_command(
        &["config", "--local", "--null", "--get-regexp", r"section\."],
        &repo,
    );
    assert_cli_success(&out_null, "--null --get-regexp");
    assert_eq!(out_null.stdout, b"section.val\nhello\0section.empty\n\0");

    let out_null_name = run_libra_command(
        &[
            "config",
            "--local",
            "--null",
            "--name-only",
            "--get-regexp",
            r"section\.",
        ],
        &repo,
    );
    assert_cli_success(&out_null_name, "--null --name-only --get-regexp");
    assert_eq!(out_null_name.stdout, b"section.val\0section.empty\0");
}
