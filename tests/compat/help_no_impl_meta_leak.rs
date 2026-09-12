//! `tests/compat/help_no_impl_meta_leak.rs` — surface contract that
//! `libra <cmd> --help` never leaks implementation-detail rustdoc
//! into the user-facing help body.
//!
//! Background: clap derives `long_about` from the second-line-onward
//! of an `Args` struct's `///` doc comment. If a contributor adds a
//! contributor-facing note as a second `///` line — e.g.
//!
//!     /// Stage file contents for the next commit.
//!     ///
//!     /// See `libra add --help` for the same EXAMPLES rendered through clap.
//!     #[derive(Parser, ...)]
//!     pub struct AddArgs { ... }
//!
//! that second line is shown verbatim in `libra add --help`. The
//! reader is *already in* `--help`, so the meta-commentary is
//! useless to them and obscures the actual command summary. v0.17.888
//! repaired the same class of leak on `libra worktree --help`
//! ("CLI arguments for the `worktree` subcommand. This type is wired
//! into the top-level CLI…"). v0.17.894 repaired three more
//! (`add`, `status`, `pull`).
//!
//! This guard codifies the rule across the entire visible-command
//! surface: any `<cmd> --help` body that contains one of the known
//! meta-leak phrases fails the test with a pointer at the
//! `src/command/<cmd>.rs` doc comment to fix.

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

/// Mirrors `tests/compat/help_examples_banner.rs::VISIBLE_COMMANDS`.
/// Kept hand-maintained so this file does not silently lose coverage
/// when a command is hidden or removed — a divergence will trip the
/// banner guard before it trips this one.
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
    "worktree",
    "am",
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
    "mergetool",
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
    "automation",
    "usage",
    "graph",
    "sandbox",
    "agent",
];

/// Phrases that should NEVER appear in any `<cmd> --help` body. Each
/// entry is paired with a short explanation that becomes the failure
/// message so contributors know what to fix and where.
const FORBIDDEN_PHRASES: &[(&str, &str)] = &[
    (
        "for the same EXAMPLES rendered through clap",
        "meta-commentary about how EXAMPLES are wired. Move it from `///` \
         to `//` so clap doesn't pick it up as long_about. See \
         src/command/add.rs / status.rs / pull.rs for the pattern \
         applied in v0.17.894.",
    ),
    (
        "for the same examples rendered through clap",
        "meta-commentary about how examples are wired. Move it from `///` \
         to `//` so clap doesn't pick it up as long_about.",
    ),
    (
        "CLI arguments for the",
        "impl-detail rustdoc body leaked from a wrapper struct doc \
         (e.g. WorktreeArgs). Either rewrite the docstring as user- \
         facing prose or set `#[command(long_about = \"…\")]` on the \
         struct. See src/command/worktree.rs (v0.17.888) for the fix \
         pattern.",
    ),
    (
        "type is wired into the top-level CLI",
        "impl-detail rustdoc body leaked from a wrapper struct doc \
         (e.g. WorktreeArgs). Either rewrite the docstring as user- \
         facing prose or set `#[command(long_about = \"…\")]`.",
    ),
    (
        "Codex pass-",
        "contributor pass-tag (e.g. 'Codex pass-8 P2: documented in \
         publish.md / docs/commands') leaked from a `///` doc comment \
         into the user-facing flag description. Move the tag to a `//` \
         non-doc comment so clap stops rendering it — see \
         src/command/publish.rs for the v0.17.901 cleanup pattern.",
    ),
    (
        "```text ",
        "raw rustdoc code fence ('```text ...```') leaked into clap's \
         long_about because clap does not render markdown. Move the \
         examples to `#[command(after_help = \"EXAMPLES:\\n    …\")]` \
         (or `<CMD>_EXAMPLES` const) and shrink the rustdoc to one \
         summary line — see src/command/clone.rs for the v0.17.911 \
         cleanup pattern.",
    ),
    (
        "# Examples",
        "raw rustdoc markdown heading ('# Examples') leaked into \
         clap's long_about because clap does not render markdown. \
         Move the examples to `#[command(after_help = …)]` and shrink \
         the rustdoc to one summary line — see src/command/clone.rs \
         for the v0.17.911 cleanup pattern.",
    ),
];

#[test]
fn no_visible_command_help_leaks_impl_meta() {
    let mut leaks: Vec<(String, String, String)> = Vec::new();

    for cmd in VISIBLE_COMMANDS {
        let output = run(&[cmd, "--help"]);
        if !output.status.success() {
            // Subcommand families like `hooks` may not be visible on
            // every build target. Skip silently — `compat_help_examples_banner`
            // catches missing commands separately.
            continue;
        }
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        for (phrase, fix_hint) in FORBIDDEN_PHRASES {
            if stdout.contains(phrase) {
                leaks.push((
                    (*cmd).to_string(),
                    (*phrase).to_string(),
                    (*fix_hint).to_string(),
                ));
            }
        }
    }

    assert!(
        leaks.is_empty(),
        "The following commands leak impl-detail rustdoc into `<cmd> --help`. \
         clap derives long_about from a `pub struct <Cmd>Args` doc comment \
         (everything after the first paragraph). If you need a \
         contributor-facing note, use a `//` non-doc comment instead of \
         `///`.\n\
         Found:\n{leaks:#?}"
    );
}
