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

/// Extract the task id from an H3 heading. Both historical shapes are
/// recognized: `### P0-01 …` (pre-2026-07-25 plans) and the restructured
/// task-card form `### Task P0-01: …` / `### Task CG-01: …`.
fn heading_id(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("### ")?;
    let mut tokens = rest
        .split([' ', '：', ':', '\t'])
        .map(str::trim)
        .filter(|t| !t.is_empty());
    let first = tokens.next()?;
    if first == "Task" {
        return Some(tokens.next().unwrap_or(""));
    }
    Some(first)
}

/// Expand inline task-id ranges (`P0-01..P0-12`, `A0-02..A0-11`, or the
/// shorthand `P0-01..12`) found in plan prose. The 2026-07-25 restructure of
/// plan-20260708 collapsed the per-subtask H3 headings into aggregate task
/// cards (`### Task P0: …`) whose bodies enumerate the governed subtask ids
/// as ranges, so the governing-number registry must recover them from text.
fn expand_id_ranges(text: &str, ids: &mut BTreeSet<String>) {
    const MAX_RANGE_END: u32 = 99;
    for fragment in text.split(|c: char| !(c.is_ascii_alphanumeric() || "-.".contains(c))) {
        let Some((start, end)) = fragment.split_once("..") else {
            continue;
        };
        if !is_task_id(start) {
            continue;
        }
        // INVARIANT: is_task_id guarantees exactly one usable split point.
        let Some((head, start_tail)) = start.split_once('-') else {
            continue;
        };
        let end_tail = match end.split_once('-') {
            // Full end id (`P0-01..P0-12`): heads must agree.
            Some((end_head, tail)) if end_head == head => tail,
            Some(_) => continue,
            // Bare numeric shorthand (`P0-01..12`).
            None => end,
        };
        let (Ok(lo), Ok(hi)) = (start_tail.parse::<u32>(), end_tail.parse::<u32>()) else {
            continue;
        };
        if lo > hi || hi > MAX_RANGE_END {
            continue;
        }
        let width = start_tail.len();
        for n in lo..=hi {
            ids.insert(format!("{head}-{n:0width$}"));
        }
    }
}

/// Commands the plan's P0/P1 task text still names. With the restructured
/// (condensed) plan this recovers only the commands the aggregate cards keep
/// in backticks — the authoritative graded-command registry is the
/// `GRADED_COMMANDS` pin, which the grading matrix is checked against
/// bidirectionally; this derivation only guards against the matrix dropping a
/// command the plan text explicitly names.
pub fn plan_p0_p1_touched_commands(cli: &BTreeSet<String>) -> BTreeSet<String> {
    let plan = read_repo_file("docs/development/plan/plan-20260708.md");
    let mut touched = BTreeSet::new();
    let mut in_p0_p1_task = false;
    for line in plan.lines() {
        if let Some(id) = heading_id(line) {
            // Pre-restructure per-subtask cards (### P0-01 …) and the
            // restructured aggregate cards (### Task P0: …) both count.
            in_p0_p1_task = ((id.starts_with("P0-") || id.starts_with("P1-")) && is_task_id(id))
                || id == "P0"
                || id == "P1";
            continue;
        }
        if !in_p0_p1_task {
            continue;
        }
        if !(line.contains("**范围**")
            || line.contains("**覆盖**")
            || line.contains("**覆盖命令**")
            || line.contains("**Description:**"))
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
    // plan-20260708: task-card headings plus the subtask-id ranges its
    // restructured aggregate cards (### Task P0: …) enumerate inline.
    let plan_20260708 = read_repo_file("docs/development/plan/plan-20260708.md");
    for line in plan_20260708.lines() {
        if let Some(id) = heading_id(line)
            && is_task_id(id)
        {
            ids.insert(id.to_string());
        }
    }
    expand_id_ranges(&plan_20260708, &mut ids);
    // plan-20260714 Part D inherited plan-20260708's deferred residuals
    // (PD-00..PD-10), so its task cards are valid governing targets too.
    for line in read_repo_file("docs/development/plan/plan-20260714.md").lines() {
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
