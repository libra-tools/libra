//! `tests/compat/format_patch_flag_surface.rs` — surface contract for
//! `libra format-patch`.
//!
//! Full integration tests live in `tests/command/format_patch_test.rs`. This
//! file pins the contract guaranteed by [`COMPATIBILITY.md`](../../COMPATIBILITY.md):
//!
//! - `libra format-patch --help` lists the implemented flags.
//! - The EXAMPLES banner is emitted (proves `FORMAT_PATCH_EXAMPLES` is wired).

use std::process::Command;

fn libra_bin() -> &'static str {
    env!("CARGO_BIN_EXE_libra")
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(libra_bin())
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", "/tmp")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .output()
        .expect("failed to spawn libra binary")
}

#[test]
fn format_patch_help_lists_expected_flags() {
    let output = run(&["format-patch", "--help"]);
    assert!(
        output.status.success(),
        "format-patch --help should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Flags that MUST appear in --help
    for flag in [
        "--output-directory",
        "-1",
        "--root",
        "--stdout",
        "--numbered",
        "--start-number",
        "--subject-prefix",
        "--cover-letter",
        "--cover-from-description",
        "--attach",
        "--inline",
        "--no-attach",
        "--thread",
        "--no-thread",
        "--in-reply-to",
        "--reroll-count",
        "--signoff",
        "--no-signoff",
        "--full-index",
        "--full-diff",
        "--minimal",
        "--histogram",
        "--numstat",
        "--ignore-if-in-upstream",
        "--src-prefix",
        "--dst-prefix",
        "--no-stat",
        "--keep-subject",
        "--suffix",
        "--zero-commit",
        "--signature",
        "--no-signature",
        "--numbered-files",
        "--to",
        "--cc",
        "--no-to",
        "--no-cc",
        "--from",
        "--no-from",
        "--relative",
        "--no-relative",
        "--base",
        "revision-range",
    ] {
        assert!(
            stdout.contains(flag),
            "format-patch --help must list `{flag}`; stdout: {stdout}"
        );
    }

    assert!(
        stdout.contains("Examples:"),
        "format-patch --help must include Examples banner; stdout: {stdout}"
    );
}
