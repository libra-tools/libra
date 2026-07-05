//! `tests/compat/help_examples_banner.rs` — surface contract for the
//! cross-cutting `--help` EXAMPLES rollout (docs/development/commands/_general.md
//! item B, marked ✅ at v0.17.837).
//!
//! Per-command tests live in `tests/command/<name>_test.rs` (each
//! pinning specific invocation prefixes). This file pins the
//! cross-cutting guarantee:
//!
//! - Every visible (non-`hide=true`) command in `Commands` renders an
//!   `EXAMPLES:` section in its `<cmd> --help` output.
//! - Future visible commands that ship without EXAMPLES will fail this
//!   test with a pointer at the missing surface and a reminder that the
//!   rollout was sealed at v0.17.836.
//!
//! Spawning the real binary (rather than only inspecting the
//! `#[command(after_help = …)]` literal at compile time) ensures the
//! contract holds end-to-end through clap's help renderer — catches
//! regressions where someone adds the constant but forgets to wire it
//! into the `#[command(after_help = ...)]` attribute, or wires it on
//! a struct that clap does not actually present as the help target.

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

/// Curated allowlist of intentionally-hidden commands that do not need
/// an EXAMPLES section. Mirrors the `hide = true` attributes in
/// `src/cli.rs::Commands` plus internal pseudo-commands clap registers
/// (`help`).
///
/// Keep this in sync with `src/cli.rs::tests::HIDDEN_COMMANDS`.
const HIDDEN_OR_HELP_ONLY: &[&str] = &["index-pack", "hooks", "help"];

/// The set of visible commands as of v0.17.840. Hard-coded so the test
/// itself does not need to discover the clap tree at runtime — if this
/// list drifts from `src/cli.rs::Commands`, a separate cli unit test
/// (`cli::tests::root_after_help_lists_every_visible_command`) flags
/// it.
const VISIBLE_COMMANDS: &[&str] = &[
    "init",
    "clone",
    "config",
    "status",
    "add",
    "rm",
    "mv",
    "restore",
    "clean",
    "stash",
    "lfs",
    "ls-files",
    "worktree",
    "log",
    "shortlog",
    "show",
    "show-ref",
    "ls-remote",
    "symbolic-ref",
    "rev-parse",
    "rev-list",
    "diff",
    "grep",
    "blame",
    "describe",
    "cat-file",
    "hash-object",
    "verify-pack",
    "commit",
    "branch",
    "switch",
    "checkout",
    "tag",
    "merge",
    "rebase",
    "reset",
    "cherry-pick",
    "push",
    "fetch",
    "pull",
    "fsck",
    "revert",
    "reflog",
    "bisect",
    "remote",
    "open",
    "cloud",
    "publish",
    "code",
    "code-control",
    "automation",
    "usage",
    "graph",
    "sandbox",
    "agent",
    "review",
    "maintenance",
    "completions",
    "logfile",
    "cache",
    "metadata",
    "dirty",
    "service",
    "revision",
    "auth",
];

#[test]
fn every_visible_command_help_renders_examples_section() {
    let mut missing: Vec<String> = Vec::new();

    for cmd in VISIBLE_COMMANDS {
        if HIDDEN_OR_HELP_ONLY.contains(cmd) {
            continue;
        }
        let output = run(&[cmd, "--help"]);
        assert!(
            output.status.success(),
            "`libra {cmd} --help` should succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Accept either the uppercase `EXAMPLES:` banner (current
        // canonical form) or the `Examples:` heading clap generates
        // when a command uses rustdoc-style examples instead of an
        // after_help constant. Both render in --help output as a
        // discoverable examples section.
        if !stdout.contains("EXAMPLES:") && !stdout.contains("Examples:") {
            missing.push((*cmd).to_string());
        }
    }

    assert!(
        missing.is_empty(),
        "Every visible command in src/cli.rs::Commands must render an \
         EXAMPLES section in `<cmd> --help` (cross-cutting item B in \
         docs/development/commands/_general.md, sealed at v0.17.836). Missing the \
         section: {missing:?}. Fix by adding `pub const <CMD>_EXAMPLES` \
         and `#[command(after_help = <CMD>_EXAMPLES)]` on the Args struct \
         (or wiring after_help on the subcommand binding in src/cli.rs \
         for subcommand-style commands; see e.g. Stash / Remote / Lfs)."
    );

    // The `media` command (lore.md §6) is a cfg-gated `Commands` variant absent
    // from the static `VISIBLE_COMMANDS` list, so it is only checkable when the
    // `fastcdc` feature is on. Under `--features fastcdc` the compiled `libra`
    // binary exposes it, so assert its EXAMPLES banner here (feature-off, this
    // block compiles out and `media` does not exist).
    #[cfg(feature = "fastcdc")]
    {
        let output = run(&["media", "--help"]);
        assert!(
            output.status.success(),
            "`libra media --help` should succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("EXAMPLES:") || stdout.contains("Examples:"),
            "`libra media --help` must render an EXAMPLES section (feature fastcdc)"
        );
    }
}
