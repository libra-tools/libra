use git_internal::internal::object::commit::Commit;
use regex::Regex;

use super::RevListArgs;
use crate::{
    command::log,
    internal::log::date_parser::parse_date,
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        util,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ParentCountFilter {
    pub(super) min: usize,
    pub(super) max: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RevListTimeWindow {
    since: Option<i64>,
    until: Option<i64>,
}

#[derive(Debug)]
pub(super) struct RevListMessageFilter {
    patterns: Vec<Regex>,
}

pub(super) fn parent_count_filter(args: &RevListArgs) -> ParentCountFilter {
    let no_merges = args.no_merges || args.cherry;
    let min = if args.no_min_parents {
        0
    } else {
        args.min_parents
            .unwrap_or(0)
            .max(usize::from(args.merges) * 2)
    };
    let max = if args.no_max_parents {
        None
    } else {
        match (args.max_parents, no_merges) {
            (Some(explicit), true) => Some(explicit.min(1)),
            (Some(explicit), false) => Some(explicit),
            (None, true) => Some(1),
            (None, false) => None,
        }
    };

    ParentCountFilter { min, max }
}

pub(super) fn rev_list_time_window(args: &RevListArgs) -> CliResult<RevListTimeWindow> {
    Ok(RevListTimeWindow {
        since: parse_rev_list_date_arg(args.since.as_deref(), "--since")?,
        until: parse_rev_list_date_arg(args.until.as_deref(), "--until")?,
    })
}

pub(super) fn rev_list_author_filter(args: &RevListArgs) -> Option<String> {
    args.author.as_ref().map(|pattern| pattern.to_lowercase())
}

pub(super) fn rev_list_committer_filter(args: &RevListArgs) -> Option<String> {
    args.committer
        .as_ref()
        .map(|pattern| pattern.to_lowercase())
}

pub(super) fn rev_list_message_filter(
    args: &RevListArgs,
) -> CliResult<Option<RevListMessageFilter>> {
    if args.grep.is_empty() {
        return Ok(None);
    }

    let patterns = args
        .grep
        .iter()
        .map(|pattern| {
            Regex::new(pattern).map_err(|error| {
                CliError::fatal(format!("invalid --grep pattern '{pattern}': {error}"))
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
                    .with_hint("use a valid regular expression or escape metacharacters")
            })
        })
        .collect::<CliResult<Vec<_>>>()?;

    Ok(Some(RevListMessageFilter { patterns }))
}

pub(super) fn commit_matches_author(commit: &Commit, author_filter: Option<&str>) -> bool {
    signature_matches_filter(&commit.author.name, &commit.author.email, author_filter)
}

pub(super) fn commit_matches_committer(commit: &Commit, committer_filter: Option<&str>) -> bool {
    signature_matches_filter(
        &commit.committer.name,
        &commit.committer.email,
        committer_filter,
    )
}

fn signature_matches_filter(name: &str, email: &str, filter: Option<&str>) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    if filter.is_empty() {
        return true;
    }

    let name = name.to_lowercase();
    let email = email.to_lowercase();
    name.contains(filter) || email.contains(filter) || format!("{name} <{email}>").contains(filter)
}

pub(super) fn commit_matches_message(
    commit: &Commit,
    filter: Option<&RevListMessageFilter>,
) -> bool {
    let Some(filter) = filter else {
        return true;
    };

    filter
        .patterns
        .iter()
        .any(|pattern| pattern.is_match(&commit.message))
}

pub(super) async fn filter_commits_by_pathspecs(
    commits: Vec<Commit>,
    pathspecs: &[String],
) -> CliResult<Vec<Commit>> {
    if pathspecs.is_empty() {
        return Ok(commits);
    }

    let filters = pathspecs
        .iter()
        .map(util::to_workdir_path)
        .collect::<Vec<_>>();
    let mut filtered = Vec::new();

    for commit in commits {
        let changes = log::get_changed_files_for_commit(&commit, &filters).await?;
        if !changes.is_empty() {
            filtered.push(commit);
        }
    }

    Ok(filtered)
}

pub(super) fn commit_matches_parent_count(commit: &Commit, filter: ParentCountFilter) -> bool {
    let parent_count = commit.parent_commit_ids.len(); // SHALLOW-DISPLAY: recorded parents, not a walk
    parent_count >= filter.min && filter.max.is_none_or(|max| parent_count <= max)
}

pub(super) fn commit_matches_time_window(commit: &Commit, window: RevListTimeWindow) -> bool {
    let commit_ts = i64::try_from(commit.committer.timestamp).unwrap_or(i64::MAX);

    if let Some(since) = window.since
        && commit_ts < since
    {
        return false;
    }
    if let Some(until) = window.until
        && commit_ts > until
    {
        return false;
    }
    true
}

pub(super) fn sort_rev_list_commits(commits: &mut [Commit]) {
    commits.sort_by_key(|commit| std::cmp::Reverse(commit.committer.timestamp));
}

fn parse_rev_list_date_arg(value: Option<&str>, flag: &str) -> CliResult<Option<i64>> {
    value.map(parse_date).transpose().map_err(|error| {
        CliError::fatal(format!("invalid {flag} date: {error}"))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint(r#"supported formats: YYYY-MM-DD, "N days ago", unix timestamp"#)
    })
}
