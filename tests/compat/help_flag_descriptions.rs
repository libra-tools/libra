//! `tests/compat/help_flag_descriptions.rs` — surface contract that every
//! visible flag rendered by `libra <cmd> --help` carries a non-empty
//! description line.
//!
//! Background: clap renders each option as
//!
//!     -f, --flag <VALUE>
//!         <description line indented underneath>
//!
//! When the originating `pub flag: ...` field has no `///` doc comment,
//! the description line is missing and the help output reads like a
//! flag with no documentation at all (e.g. `--bare` with nothing under
//! it). v0.17.886/v0.17.887 landed several such repairs (tag `--force`,
//! push `--set-upstream`, all of `init`'s flags); this guard prevents
//! the regression from re-appearing on any command's visible flags
//! AND on positional `Arguments:` (extended v0.17.889 after
//! `describe [COMMIT]` was found to have no description).
//!
//! Approach: scan the `<cmd> --help` output of every visible command
//! for the `Options:` and `Arguments:` sections, then walk each
//! flag/argument line and the line below it. If the next non-empty
//! line is another entry (or the end-of-section) instead of an
//! indented description, fail with a list of the empty entries.

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

/// Commands we cover. Mirrors `VISIBLE_COMMANDS` in
/// `help_examples_banner.rs` minus subcommand-style families whose
/// `--help` lists sub-commands rather than flags (those are covered by
/// their own per-subcommand tests).
const COMMANDS: &[&str] = &[
    "init",
    "clone",
    "status",
    "add",
    "rm",
    "mv",
    "restore",
    "clean",
    "am",
    "log",
    "shortlog",
    "show",
    "show-ref",
    "ls-remote",
    "ls-files",
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
    "open",
    "graph",
    "sandbox",
    "usage",
];

fn extract_section<'a>(help: &'a str, heading: &str) -> Option<&'a str> {
    let start = help.find(heading)?;
    let after = &help[start..];
    let mut end = after.len();
    for (idx, line) in after.lines().enumerate() {
        if idx == 0 {
            continue;
        }
        let trimmed = line.trim_end();
        let is_heading = !trimmed.is_empty()
            && !trimmed.starts_with(' ')
            && !trimmed.starts_with('\t')
            && trimmed != heading;
        if is_heading {
            let pos: usize = after.lines().take(idx).map(|l| l.len() + 1).sum();
            end = pos;
            break;
        }
    }
    Some(&after[..end])
}

/// Returns true if `line` looks like a clap flag/option line at the
/// canonical two-space indent. Examples:
///   `  -h, --help`
///   `  -J, --json[=<FORMAT>]    Emit machine-readable JSON…`
///   `      --bare`
fn is_option_line(line: &str) -> bool {
    if (!line.starts_with("  ") || line.starts_with("    "))
        && !line.starts_with("      -")
        && !line.starts_with("      --")
    {
        return false;
    }
    let trimmed = line.trim_start();
    trimmed.starts_with('-')
}

/// Returns true if `line` looks like a clap positional argument line
/// at the canonical two-space indent. Examples:
///   `  [COMMIT]`
///   `  [PATH]...    Files to hash`
///   `  <REPOSITORY>`
fn is_positional_line(line: &str) -> bool {
    if !line.starts_with("  ") || line.starts_with("    ") {
        return false;
    }
    let trimmed = line.trim_start();
    trimmed.starts_with('[') || trimmed.starts_with('<')
}

/// Returns true if `tail` is *only* a clap-generated annotation such
/// as `[default: ...]`, `[possible values: ...]`, `[aliases: ...]` —
/// none of which describe what the flag actually does. We treat these
/// as "no description" so the guard catches `[REPO_DIRECTORY]  [default: .]`
/// (init's positional argument prior to v0.17.891).
fn is_only_clap_annotation(tail: &str) -> bool {
    let trimmed = tail.trim();
    if trimmed.is_empty() {
        return false;
    }
    let mut rest = trimmed;
    while !rest.is_empty() {
        if !rest.starts_with('[') {
            return false;
        }
        let Some(end) = rest.find(']') else {
            return false;
        };
        let body = &rest[1..end];
        let kind = body.split(':').next().unwrap_or("").trim();
        if !matches!(
            kind,
            "default" | "possible values" | "aliases" | "alias" | "env"
        ) {
            return false;
        }
        rest = rest[end + 1..].trim_start();
    }
    true
}

fn entry_has_inline_or_next_line_description(
    lines: &[&str],
    i: usize,
    is_entry_line: fn(&str) -> bool,
) -> bool {
    let line = lines[i];
    let trimmed = line.trim_end();
    let after_entry = trimmed.trim_start_matches([' ']);
    let two_space = after_entry.find("  ");
    let has_inline_desc = match two_space {
        Some(pos) => {
            let tail = &after_entry[pos..];
            tail.trim().chars().any(|c| !c.is_whitespace()) && !is_only_clap_annotation(tail)
        }
        None => false,
    };
    if has_inline_desc {
        return true;
    }

    let mut j = i + 1;
    while j < lines.len() {
        let next = lines[j];
        if next.trim().is_empty() {
            j += 1;
            continue;
        }
        if is_entry_line(next) {
            return false;
        }
        if next.starts_with("        ") || next.starts_with("    ") {
            return true;
        }
        return false;
    }
    false
}

fn scan_section(
    cmd: &str,
    section: Option<&str>,
    is_entry_line: fn(&str) -> bool,
    empty: &mut Vec<(String, String)>,
) {
    let Some(section) = section else {
        return;
    };
    let lines: Vec<&str> = section.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if !is_entry_line(line) {
            continue;
        }
        if !entry_has_inline_or_next_line_description(&lines, i, is_entry_line) {
            empty.push(((*cmd).to_string(), line.trim_end().trim().to_string()));
        }
    }
}

/// Subcommand pairs we cover in addition to the top-level COMMANDS list.
///
/// Each entry is `(parent, sub)`. Generated by walking the subcommands of
/// `agent`, `automation`, `cloud`, `lfs`, `publish`, `remote`, `stash`,
/// `worktree`, `db`, `agent rpc`, and `agent checkpoint` / `agent session`
/// (one level deep — clap's own `help` pseudo-subcommand is filtered).
const SUBCOMMANDS: &[&[&str]] = &[
    // agent — entire.md Phase 4 surface
    &["agent", "status"],
    &["agent", "enable"],
    &["agent", "disable"],
    &["agent", "clean"],
    &["agent", "doctor"],
    &["agent", "push"],
    &["automation", "list"],
    &["automation", "run"],
    &["automation", "history"],
    // cloud
    &["cloud", "sync"],
    &["cloud", "restore"],
    &["cloud", "status"],
    // lfs
    &["lfs", "track"],
    &["lfs", "untrack"],
    &["lfs", "locks"],
    &["lfs", "lock"],
    &["lfs", "unlock"],
    &["lfs", "ls-files"],
    // publish
    &["publish", "init"],
    &["publish", "sync"],
    &["publish", "status"],
    &["publish", "deploy"],
    &["publish", "unpublish"],
    // stash — every stash subcommand
    &["stash", "push"],
    &["stash", "pop"],
    &["stash", "list"],
    &["stash", "apply"],
    &["stash", "drop"],
    &["stash", "show"],
    &["stash", "branch"],
    &["stash", "clear"],
    // worktree
    &["worktree", "add"],
    &["worktree", "list"],
    &["worktree", "lock"],
    &["worktree", "unlock"],
    &["worktree", "move"],
    &["worktree", "prune"],
    &["worktree", "remove"],
    &["worktree", "repair"],
    // agent sub-subcommands (2 levels deep) — added v0.17.902 after
    // checkpoint/rewind / session/* were found to ship empty positional
    // arg descriptions despite the parent-level guard passing.
    &["agent", "session", "list"],
    &["agent", "session", "show"],
    &["agent", "session", "stop"],
    &["agent", "session", "resume"],
    &["agent", "session", "promote"],
    &["agent", "session", "derive-tool-calls"],
    &["agent", "checkpoint", "list"],
    &["agent", "checkpoint", "show"],
    &["agent", "checkpoint", "rewind"],
    &["agent", "rpc", "list"],
    &["agent", "rpc", "invoke"],
    // remote subcommands — added v0.17.904. Each remote operation
    // takes 1–2 positional NAME / URL args and a handful of flags
    // (set-url has --add / --delete / --push / --all).
    &["remote", "add"],
    &["remote", "remove"],
    &["remote", "rename"],
    &["remote", "show"],
    &["remote", "get-url"],
    &["remote", "set-url"],
    &["remote", "prune"],
    // config subcommands — added v0.17.904. Includes vault-aware
    // key generation flows that take destination paths.
    &["config", "set"],
    &["config", "get"],
    &["config", "list"],
    &["config", "unset"],
    &["config", "import"],
    &["config", "path"],
    &["config", "generate-ssh-key"],
    &["config", "generate-gpg-key"],
];

#[test]
fn every_visible_flag_has_a_description() {
    let mut empty: Vec<(String, String)> = Vec::new();

    for cmd in COMMANDS {
        let output = run(&[cmd, "--help"]);
        assert!(
            output.status.success(),
            "`libra {cmd} --help` should succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        scan_section(
            cmd,
            extract_section(&stdout, "Options:"),
            is_option_line,
            &mut empty,
        );
        scan_section(
            cmd,
            extract_section(&stdout, "Arguments:"),
            is_positional_line,
            &mut empty,
        );
    }

    for sub in SUBCOMMANDS {
        let mut args: Vec<&str> = sub.to_vec();
        args.push("--help");
        let output = run(&args);
        assert!(
            output.status.success(),
            "`libra {} --help` should succeed; stderr: {}",
            sub.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let label = sub.join(" ");
        scan_section(
            &label,
            extract_section(&stdout, "Options:"),
            is_option_line,
            &mut empty,
        );
        scan_section(
            &label,
            extract_section(&stdout, "Arguments:"),
            is_positional_line,
            &mut empty,
        );
    }

    assert!(
        empty.is_empty(),
        "The following flags/arguments are visible in `libra <cmd> --help` \
         but have no description line (clap renders a blank line under \
         them). Add a `///` doc comment on the corresponding `pub <field>: \
         ...` in `src/command/<cmd>.rs` describing what the flag/argument \
         does.\n\
         Found: {empty:#?}"
    );
}
