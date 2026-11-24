//! `libra check-mailmap` — resolve `Name <email>` contacts through the
//! repository `.mailmap`, a focused subset of `git check-mailmap`.
//!
//! This first version only parses `.mailmap` and answers queries; wiring the
//! same resolver into `log` / `blame` author display is a documented follow-up.

use std::{fs, io::Read};

use clap::Parser;
use serde::Serialize;

use crate::utils::{
    error::{CliError, CliResult, StableErrorCode},
    output::{OutputConfig, emit_json_data},
    util,
};

pub const CHECK_MAILMAP_EXAMPLES: &str = "\
EXAMPLES:
    libra check-mailmap 'Bob <bob@old.example>'    Resolve one contact via .mailmap
    printf 'Bob <bob@old.example>\\n' | libra check-mailmap --stdin
    libra --json check-mailmap 'Bob <bob@x>'       Structured { contacts: [...] }";

/// Resolve `Name <email>` contacts through `.mailmap`.
#[derive(Parser, Debug)]
#[command(after_help = CHECK_MAILMAP_EXAMPLES)]
pub struct CheckMailmapArgs {
    /// Read contacts (one `Name <email>` per line) from stdin.
    #[clap(long)]
    pub stdin: bool,

    /// Contacts to resolve, each `Name <email>`.
    #[clap(value_name = "CONTACT")]
    pub contacts: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CheckMailmapOutput {
    contacts: Vec<String>,
}

/// One parsed `.mailmap` entry. `old_*` is the key to match; `new_*` is what to
/// emit (an empty `new_name` keeps the looked-up name).
#[derive(Debug)]
pub(crate) struct MailmapEntry {
    new_name: String,
    new_email: String,
    old_name: Option<String>,
    old_email: String,
}

pub async fn execute(args: CheckMailmapArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

pub async fn execute_safe(args: CheckMailmapArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    let usage = |message: String| {
        CliError::command_usage(message)
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_exit_code(128)
    };

    let mailmap = load_mailmap()?;

    let inputs = if args.stdin {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|error| {
                CliError::fatal(format!("failed to read contacts from stdin: {error}"))
                    .with_exit_code(128)
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })?;
        buffer
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    } else {
        args.contacts.clone()
    };

    if inputs.is_empty() {
        return Err(usage(
            "no contacts given (pass `Name <email>` arguments or --stdin)".to_string(),
        ));
    }

    let mut resolved = Vec::with_capacity(inputs.len());
    for input in &inputs {
        let (name, email) = parse_contact(input).map_err(usage)?;
        let (out_name, out_email) = resolve(&mailmap, &name, &email);
        resolved.push(format_contact(&out_name, &out_email));
    }

    if output.is_json() {
        emit_json_data(
            "check-mailmap",
            &CheckMailmapOutput { contacts: resolved },
            output,
        )
    } else {
        for contact in &resolved {
            println!("{contact}");
        }
        Ok(())
    }
}

/// Load and parse mailmap sources from the working tree: `.mailmap` first, then
/// `mailmap.file` (whose entries override the worktree file). Absent → empty.
fn load_mailmap() -> CliResult<Vec<MailmapEntry>> {
    let workdir = util::working_dir();
    load_mailmap_from(&workdir)
}

/// Crate-wide mailmap loader (single source of truth for `check-mailmap` and
/// `shortlog`).
pub(crate) fn load_mailmap_from(workdir: &std::path::Path) -> CliResult<Vec<MailmapEntry>> {
    let mut entries = Vec::new();
    append_mailmap_file(&mut entries, &workdir.join(".mailmap"))?;
    if let Some(configured) = util::optional_cascaded_config_path("mailmap.file", workdir) {
        append_mailmap_file(&mut entries, &configured)?;
    }
    Ok(entries)
}

fn append_mailmap_file(entries: &mut Vec<MailmapEntry>, path: &std::path::Path) -> CliResult<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(CliError::fatal(format!(
                "failed to read mailmap '{}': {error}",
                path.display()
            ))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::IoReadFailed));
        }
    };
    entries.extend(text.lines().filter_map(parse_mailmap_line));
    Ok(())
}

/// Parse a single `.mailmap` line into an entry, skipping comments/blanks and
/// malformed lines.
fn parse_mailmap_line(line: &str) -> Option<MailmapEntry> {
    let line = match line.split_once('#') {
        Some((before, _)) => before,
        None => line,
    };
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    // Find the first and optional second `<...>` email span.
    let first_open = line.find('<')?;
    let first_close = line[first_open..].find('>')? + first_open;
    let new_email = line[first_open + 1..first_close].trim().to_string();
    let new_name = line[..first_open].trim().to_string();

    let rest = &line[first_close + 1..];
    if let Some(second_open) = rest.find('<') {
        let second_close = rest[second_open..].find('>')? + second_open;
        let old_email = rest[second_open + 1..second_close].trim().to_string();
        let old_name = rest[..second_open].trim();
        Some(MailmapEntry {
            new_name,
            new_email,
            old_name: (!old_name.is_empty()).then(|| old_name.to_string()),
            old_email,
        })
    } else {
        // One email: the entry keys on that same email.
        Some(MailmapEntry {
            new_email: new_email.clone(),
            new_name,
            old_name: None,
            old_email: new_email,
        })
    }
}

/// Resolve `(name, email)` through the mailmap. A `(name, email)` entry wins
/// over an email-only entry, matching Git.
fn resolve(mailmap: &[MailmapEntry], name: &str, email: &str) -> (String, String) {
    let email_lc = email.to_ascii_lowercase();

    // Later entries override earlier ones, and names/emails match ASCII
    // case-insensitively (Git's `strcasecmp`).
    let pick = |with_name: bool| {
        mailmap.iter().rev().find(|e| {
            e.old_email.eq_ignore_ascii_case(&email_lc)
                && match (&e.old_name, with_name) {
                    (Some(old), true) => old.eq_ignore_ascii_case(name),
                    (None, false) => true,
                    _ => false,
                }
        })
    };

    if let Some(entry) = pick(true).or_else(|| pick(false)) {
        let out_name = if entry.new_name.is_empty() {
            name.to_string()
        } else {
            entry.new_name.clone()
        };
        return (out_name, entry.new_email.clone());
    }
    (name.to_string(), email.to_string())
}

/// Apply a loaded mailmap to one identity (used by `shortlog`); an empty map is
/// an identity function.
pub(crate) fn map_identity(entries: &[MailmapEntry], name: &str, email: &str) -> (String, String) {
    resolve(entries, name, email)
}

/// Parse a `Name <email>` contact.
fn parse_contact(input: &str) -> Result<(String, String), String> {
    let open = input
        .find('<')
        .ok_or_else(|| format!("invalid contact '{input}': expected `Name <email>`"))?;
    let close = input[open..]
        .find('>')
        .map(|i| i + open)
        .ok_or_else(|| format!("invalid contact '{input}': missing closing `>`"))?;
    let name = input[..open].trim().to_string();
    let email = input[open + 1..close].trim().to_string();
    if email.is_empty() {
        return Err(format!("invalid contact '{input}': empty email"));
    }
    Ok((name, email))
}

fn format_contact(name: &str, email: &str) -> String {
    if name.is_empty() {
        format!("<{email}>")
    } else {
        format!("{name} <{email}>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(lines: &str) -> Vec<MailmapEntry> {
        lines.lines().filter_map(parse_mailmap_line).collect()
    }

    #[test]
    fn maps_commit_email_to_proper_name_and_email() {
        let m = map("Proper Name <proper@example.com> <commit@example.com>\n");
        let (n, e) = resolve(&m, "Whoever", "commit@example.com");
        assert_eq!(n, "Proper Name");
        assert_eq!(e, "proper@example.com");
    }

    #[test]
    fn name_plus_email_entry_wins_over_email_only() {
        let m = map("Email Only <eo@x> <c@x>\nName Plus <np@x> Commit Name <c@x>\n");
        let (n, e) = resolve(&m, "Commit Name", "c@x");
        assert_eq!((n.as_str(), e.as_str()), ("Name Plus", "np@x"));
        // A different name with the same email falls back to the email-only rule.
        let (n2, e2) = resolve(&m, "Someone Else", "c@x");
        assert_eq!((n2.as_str(), e2.as_str()), ("Email Only", "eo@x"));
    }

    #[test]
    fn unmatched_contact_is_unchanged() {
        let m = map("Proper <p@x> <c@x>\n");
        let (n, e) = resolve(&m, "Nobody", "nobody@x");
        assert_eq!((n.as_str(), e.as_str()), ("Nobody", "nobody@x"));
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let m = map("# comment\n\nProper <p@x> <c@x>\n");
        assert_eq!(m.len(), 1);
    }
}
