use std::collections::{HashMap, HashSet};

use git_internal::{hash::ObjectHash, internal::object::commit::Commit};
use serde::Serialize;

use crate::{
    command::{load_object, log},
    internal::shallow::ShallowSet,
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        util::{self, CommitBaseError},
    },
};

pub(super) struct RevListSelection {
    pub(super) input: String,
    pub(super) inputs: Vec<String>,
    pub(super) commits: Vec<Commit>,
    pub(super) sides: HashMap<String, RevListSide>,
    /// The full reachability closure of the EXPLICITLY excluded tips (`^spec`,
    /// the start of `A..B`, and the merge base of `A...B`) — i.e. the
    /// "uninteresting" commits. Used by `--objects` to mark the objects reachable
    /// from the excluded side so they are not re-emitted. Distinct from commits
    /// merely omitted by `--max-count`/`--skip`/`--grep`/etc., which are NOT here.
    pub(super) excluded: HashSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum RevListSide {
    Left,
    Right,
}

enum RevisionTerm<'a> {
    Include(&'a str),
    Exclude(&'a str),
    Range { start: &'a str, end: &'a str },
    Symmetric { left: &'a str, right: &'a str },
}

pub(super) async fn resolve_revision_selection(
    specs: &[String],
    first_parent: bool,
    default_to_head: bool,
) -> CliResult<RevListSelection> {
    let input_terms = normalized_inputs(specs, default_to_head);
    let mut included = Vec::<Commit>::new();
    let mut included_ids = HashSet::<String>::new();
    let mut excluded = HashSet::<String>::new();
    let mut sides = HashMap::<String, RevListSide>::new();

    for input in &input_terms {
        match parse_revision_term(input) {
            RevisionTerm::Include(spec) => {
                include_reachable(spec, first_parent, &mut included, &mut included_ids).await?
            }
            RevisionTerm::Exclude(spec) => {
                exclude_reachable(spec, first_parent, &mut excluded).await?
            }
            RevisionTerm::Range { start, end } => {
                include_reachable(end, first_parent, &mut included, &mut included_ids).await?;
                exclude_reachable(start, first_parent, &mut excluded).await?;
            }
            RevisionTerm::Symmetric { left, right } => {
                let left_commits = reachable_commits(left, first_parent).await?;
                let right_commits = reachable_commits(right, first_parent).await?;
                let left_ids = commit_id_set(&left_commits);
                let right_ids = commit_id_set(&right_commits);
                let common_ids = left_ids
                    .intersection(&right_ids)
                    .cloned()
                    .collect::<HashSet<_>>();
                excluded.extend(common_ids.iter().cloned());
                insert_commits_with_side(
                    left_commits,
                    RevListSide::Left,
                    &common_ids,
                    &mut included,
                    &mut included_ids,
                    &mut sides,
                );
                insert_commits_with_side(
                    right_commits,
                    RevListSide::Right,
                    &common_ids,
                    &mut included,
                    &mut included_ids,
                    &mut sides,
                );
            }
        }
    }

    let commits = included
        .into_iter()
        .filter(|commit| !excluded.contains(&commit.id.to_string()))
        .collect::<Vec<_>>();
    let input = input_terms.join(" ");

    Ok(RevListSelection {
        input,
        inputs: input_terms,
        commits,
        sides,
        excluded,
    })
}

fn normalized_inputs(specs: &[String], default_to_head: bool) -> Vec<String> {
    if specs.is_empty() && default_to_head {
        // Bare `rev-list` walks HEAD; but `--all` supplies the ref set as the
        // input, so an empty set there must stay empty (not fall back to HEAD).
        vec!["HEAD".to_string()]
    } else {
        specs.to_vec()
    }
}

fn parse_revision_term(input: &str) -> RevisionTerm<'_> {
    if let Some(spec) = input.strip_prefix('^')
        && !spec.is_empty()
    {
        return RevisionTerm::Exclude(spec);
    }
    if let Some((left, right)) = split_range(input, "...") {
        return RevisionTerm::Symmetric {
            left: default_head(left),
            right: default_head(right),
        };
    }
    if let Some((start, end)) = split_range(input, "..") {
        return RevisionTerm::Range {
            start: default_head(start),
            end: default_head(end),
        };
    }
    RevisionTerm::Include(input)
}

fn split_range<'a>(input: &'a str, separator: &str) -> Option<(&'a str, &'a str)> {
    let index = input.find(separator)?;
    let left = &input[..index];
    let right = &input[index + separator.len()..];
    Some((left, right))
}

fn default_head(input: &str) -> &str {
    if input.is_empty() { "HEAD" } else { input }
}

async fn include_reachable(
    spec: &str,
    first_parent: bool,
    included: &mut Vec<Commit>,
    included_ids: &mut HashSet<String>,
) -> CliResult<()> {
    insert_commits(
        reachable_commits(spec, first_parent).await?,
        included,
        included_ids,
    );
    Ok(())
}

async fn exclude_reachable(
    spec: &str,
    first_parent: bool,
    excluded: &mut HashSet<String>,
) -> CliResult<()> {
    excluded.extend(
        reachable_commits(spec, first_parent)
            .await?
            .into_iter()
            .map(|commit| commit.id.to_string()),
    );
    Ok(())
}

async fn reachable_commits(spec: &str, first_parent: bool) -> CliResult<Vec<Commit>> {
    let commit = resolve_commit(spec).await?;
    if first_parent {
        first_parent_reachable_commits(commit)
    } else {
        log::get_reachable_commits(commit.to_string(), None).await
    }
}

fn first_parent_reachable_commits(start: ObjectHash) -> CliResult<Vec<Commit>> {
    let mut commits = Vec::new();
    let mut current = Some(start);
    let shallow = ShallowSet::load().map_err(|error| {
        CliError::fatal(error.to_string())
            .with_stable_code(StableErrorCode::RepoCorrupt)
            .with_hint(error.hint())
    })?;

    while let Some(commit_id) = current {
        let commit = load_object::<Commit>(&commit_id).map_err(|error| {
            CliError::fatal(format!("storage broken, object not found: {error}"))
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint("run 'libra fsck' to inspect missing history")
        })?;
        current = shallow
            .parents_for_walk(&commit.id, &commit.parent_commit_ids)
            .first()
            .copied();
        commits.push(commit);
    }

    Ok(commits)
}

async fn resolve_commit(spec: &str) -> CliResult<ObjectHash> {
    util::get_commit_base_typed(spec)
        .await
        .map_err(|err| rev_list_target_error(spec, err))
}

fn insert_commits(
    commits: Vec<Commit>,
    included: &mut Vec<Commit>,
    included_ids: &mut HashSet<String>,
) {
    for commit in commits {
        if included_ids.insert(commit.id.to_string()) {
            included.push(commit);
        }
    }
}

fn insert_commits_with_side(
    commits: Vec<Commit>,
    side: RevListSide,
    common_ids: &HashSet<String>,
    included: &mut Vec<Commit>,
    included_ids: &mut HashSet<String>,
    sides: &mut HashMap<String, RevListSide>,
) {
    for commit in commits {
        let id = commit.id.to_string();
        if !common_ids.contains(&id) {
            sides.entry(id.clone()).or_insert(side);
        }
        if included_ids.insert(id) {
            included.push(commit);
        }
    }
}

fn commit_id_set(commits: &[Commit]) -> HashSet<String> {
    commits.iter().map(|commit| commit.id.to_string()).collect()
}

fn rev_list_target_error(spec: &str, error: CommitBaseError) -> CliError {
    match error {
        CommitBaseError::HeadUnborn => CliError::failure(format!(
            "not a valid object name: '{spec}' (HEAD does not point to a commit)"
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget)
        .with_hint("create a commit before resolving HEAD."),
        CommitBaseError::InvalidReference(detail) => {
            CliError::failure(format!("not a valid object name: '{spec}' ({detail})"))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
        }
        CommitBaseError::ReadFailure(detail) => {
            CliError::fatal(format!("failed to resolve '{spec}': {detail}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        }
        CommitBaseError::CorruptReference(detail) => {
            CliError::fatal(format!("failed to resolve '{spec}': {detail}"))
                .with_stable_code(StableErrorCode::RepoCorrupt)
        }
    }
}
