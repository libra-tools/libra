use std::collections::BTreeSet;

use crate::support::doc::{GOVERNANCE_MD, backtick_tokens, read_repo_file};

fn pascal_to_kebab(name: &str) -> String {
    let mut out = String::new();
    let mut prev_is_lower_or_digit = false;
    for ch in name.chars() {
        if ch.is_ascii_uppercase() {
            if !out.is_empty() && prev_is_lower_or_digit {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
            prev_is_lower_or_digit = false;
        } else {
            out.push(ch);
            prev_is_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    out
}

pub fn cli_commands() -> BTreeSet<String> {
    let cli_rs = read_repo_file("src/cli.rs");
    let mut in_commands = false;
    let mut commands = BTreeSet::new();
    for line in cli_rs.lines() {
        if line.trim() == "enum Commands {" {
            in_commands = true;
            continue;
        }
        if in_commands && line == "}" {
            break;
        }
        if !in_commands {
            continue;
        }
        let trimmed = line.trim_start();
        let Some(first) = trimmed.chars().next() else {
            continue;
        };
        if !first.is_ascii_uppercase() {
            continue;
        }
        let ident_end = trimmed
            .find(|ch: char| !ch.is_ascii_alphanumeric())
            .unwrap_or(trimmed.len());
        if trimmed[ident_end..].starts_with('(') {
            commands.insert(pascal_to_kebab(&trimmed[..ident_end]));
        }
    }
    commands
}

fn is_task_id(s: &str) -> bool {
    let Some((head, tail)) = s.split_once('-') else {
        return false;
    };
    !head.is_empty()
        && head.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && head
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        && !tail.is_empty()
        && tail.chars().all(|c| c.is_ascii_digit())
}

fn is_d_number(s: &str) -> bool {
    s.strip_prefix('D')
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

fn heading_id(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("### ")?;
    Some(
        rest.split([' ', '：', ':', '\t'])
            .next()
            .unwrap_or("")
            .trim(),
    )
}

pub fn plan_p0_p1_touched_commands(cli: &BTreeSet<String>) -> BTreeSet<String> {
    let plan = read_repo_file("docs/development/plan/plan-20260708.md");
    let mut touched = BTreeSet::new();
    let mut in_p0_p1_task = false;
    for line in plan.lines() {
        if let Some(id) = heading_id(line) {
            in_p0_p1_task = (id.starts_with("P0-") || id.starts_with("P1-")) && is_task_id(id);
            continue;
        }
        if !in_p0_p1_task {
            continue;
        }
        if !(line.contains("**范围**")
            || line.contains("**覆盖**")
            || line.contains("**覆盖命令**"))
        {
            continue;
        }
        for token in backtick_tokens(line) {
            let base = token
                .split_whitespace()
                .next()
                .unwrap_or("")
                .split('/')
                .next()
                .unwrap_or("");
            if cli.contains(base) {
                touched.insert(base.to_string());
            }
        }
    }
    touched
}

pub fn valid_governing_numbers() -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for line in read_repo_file("docs/development/plan/plan-20260708.md").lines() {
        if let Some(id) = heading_id(line)
            && is_task_id(id)
        {
            ids.insert(id.to_string());
        }
    }
    for line in read_repo_file(GOVERNANCE_MD).lines() {
        if let Some(id) = heading_id(line)
            && is_d_number(id)
        {
            ids.insert(id.to_string());
        }
    }
    ids
}
