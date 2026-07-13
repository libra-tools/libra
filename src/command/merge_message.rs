//! Default merge-message rendering, including the `merge.log` shortlog block.

use std::collections::{HashSet, VecDeque};

use git_internal::{hash::ObjectHash, internal::object::commit::Commit};

use crate::{command::load_object, common_utils::parse_commit_msg};

pub(crate) fn default_message(
    current: ObjectHash,
    target: ObjectHash,
    upstream: &str,
    head_name: &str,
    log_limit: usize,
) -> Result<String, String> {
    append_shortlog(
        format!("Merge {upstream} into {head_name}"),
        current,
        target,
        upstream,
        log_limit,
    )
}

/// Append the Git-style target-side shortlog to an already-resolved message.
/// This is used when an explicit CLI `-m` is combined with `--log`; config-only
/// `merge.log` is suppressed by the caller for explicit messages.
pub(crate) fn append_shortlog(
    mut message: String,
    current: ObjectHash,
    target: ObjectHash,
    upstream: &str,
    log_limit: usize,
) -> Result<String, String> {
    if log_limit == 0 {
        return Ok(message);
    }

    let excluded = reachable_ids(current)?;
    let subjects = unique_side_subjects(target, &excluded, log_limit)?;
    if subjects.is_empty() {
        return Ok(message);
    }
    message.push_str(&format!("\n\n* {upstream}:"));
    for subject in subjects {
        message.push_str("\n  ");
        message.push_str(&subject);
    }
    Ok(message)
}

fn reachable_ids(start: ObjectHash) -> Result<HashSet<ObjectHash>, String> {
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([start]);
    while let Some(commit_id) = queue.pop_front() {
        if !seen.insert(commit_id) {
            continue;
        }
        let commit = load_commit(commit_id)?;
        queue.extend(commit.parent_commit_ids);
    }
    Ok(seen)
}

fn unique_side_subjects(
    start: ObjectHash,
    excluded: &HashSet<ObjectHash>,
    limit: usize,
) -> Result<Vec<String>, String> {
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([start]);
    let mut subjects = Vec::new();
    while let Some(commit_id) = queue.pop_front() {
        if excluded.contains(&commit_id) || !seen.insert(commit_id) {
            continue;
        }
        let commit = load_commit(commit_id)?;
        let subject = parse_commit_msg(&commit.message)
            .0
            .lines()
            .next()
            .unwrap_or_default()
            .to_string();
        subjects.push(subject);
        if subjects.len() == limit {
            break;
        }
        queue.extend(commit.parent_commit_ids);
    }
    Ok(subjects)
}

fn load_commit(commit_id: ObjectHash) -> Result<Commit, String> {
    load_object(&commit_id).map_err(|error| format!("failed to load commit {commit_id}: {error}"))
}
