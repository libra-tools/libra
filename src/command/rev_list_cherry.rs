use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc};

use git_internal::{
    Diff,
    hash::ObjectHash,
    internal::object::{blob::Blob, commit::Commit, tree::Tree},
};

use super::{RevListArgs, rev_list_spec::RevListSide};
use crate::{
    command::load_object,
    internal::shallow::ShallowSet,
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        object_ext::TreeExt,
    },
};

#[derive(Debug, Clone)]
pub(super) struct RevListSelectedCommit {
    pub(super) commit: Commit,
    pub(super) side: Option<RevListSide>,
    pub(super) cherry_equivalent: bool,
}

pub(super) fn attach_cherry_metadata(
    commits: Vec<Commit>,
    sides: &HashMap<String, RevListSide>,
) -> Vec<RevListSelectedCommit> {
    commits
        .into_iter()
        .map(|commit| {
            let side = sides.get(&commit.id.to_string()).copied();
            RevListSelectedCommit {
                commit,
                side,
                cherry_equivalent: false,
            }
        })
        .collect()
}

pub(super) fn apply_cherry_filters(
    mut commits: Vec<RevListSelectedCommit>,
    args: &RevListArgs,
) -> CliResult<Vec<RevListSelectedCommit>> {
    if args.cherry_pick || args.cherry_mark || args.cherry {
        mark_cherry_equivalents(&mut commits)?;
    }

    if args.cherry_pick && !args.cherry {
        commits.retain(|commit| !commit.cherry_equivalent);
    }

    if args.cherry {
        commits.retain(|commit| commit.side == Some(RevListSide::Right));
    } else if args.left_only {
        commits.retain(|commit| commit.side == Some(RevListSide::Left));
    } else if args.right_only {
        commits.retain(|commit| commit.side == Some(RevListSide::Right));
    }

    Ok(commits)
}

fn mark_cherry_equivalents(commits: &mut [RevListSelectedCommit]) -> CliResult<()> {
    let signatures = commits
        .iter()
        .map(|commit| {
            commit
                .side
                .map(|side| {
                    commit_patch_signature(&commit.commit).map(|signature| (side, signature))
                })
                .transpose()
        })
        .collect::<CliResult<Vec<_>>>()?;
    let mut sides_by_signature = HashMap::<String, Vec<RevListSide>>::new();

    for (side, signature) in signatures.iter().flatten() {
        sides_by_signature
            .entry(signature.clone())
            .or_default()
            .push(*side);
    }

    for (commit, signature) in commits.iter_mut().zip(signatures) {
        let Some((_, signature)) = signature else {
            continue;
        };
        commit.cherry_equivalent = sides_by_signature.get(&signature).is_some_and(|sides| {
            sides.contains(&RevListSide::Left) && sides.contains(&RevListSide::Right)
        });
    }

    Ok(())
}

fn commit_patch_signature(commit: &Commit) -> CliResult<String> {
    let new_blobs = commit_tree_blobs(commit)?;
    let shallow = ShallowSet::load()
        .map_err(|error| rev_list_corrupt_error(error.to_string()).with_hint(error.hint()))?;
    let old_blobs = if let Some(parent) = shallow
        .parents_for_walk(&commit.id, &commit.parent_commit_ids)
        .first()
    {
        let parent = load_object::<Commit>(parent).map_err(|error| {
            rev_list_corrupt_error(format!("failed to load parent commit: {error}"))
        })?;
        commit_tree_blobs(&parent)?
    } else {
        Vec::new()
    };

    let diffs = build_diff_items(old_blobs, new_blobs)?;
    let mut signatures = diffs
        .into_iter()
        .map(|diff| normalize_diff_for_patch_id(&diff.path, &diff.data))
        .collect::<Vec<_>>();
    signatures.sort();
    Ok(signatures.join("\n"))
}

fn commit_tree_blobs(commit: &Commit) -> CliResult<Vec<(PathBuf, ObjectHash)>> {
    let tree = load_object::<Tree>(&commit.tree_id)
        .map_err(|error| rev_list_corrupt_error(format!("failed to load tree object: {error}")))?;
    Ok(tree.get_plain_items())
}

fn build_diff_items(
    old_blobs: Vec<(PathBuf, ObjectHash)>,
    new_blobs: Vec<(PathBuf, ObjectHash)>,
) -> CliResult<Vec<git_internal::diff::DiffItem>> {
    let load_error = Rc::new(RefCell::new(None::<CliError>));
    let load_error_for_read = Rc::clone(&load_error);
    let diffs =
        Diff::diff(
            old_blobs,
            new_blobs,
            Vec::new(),
            move |_path, hash| match load_blob_content(hash) {
                Ok(content) => content,
                Err(error) => {
                    record_diff_error(&load_error_for_read, error);
                    Vec::new()
                }
            },
        );

    if let Some(error) = load_error.borrow_mut().take() {
        return Err(error);
    }

    Ok(diffs)
}

fn load_blob_content(hash: &ObjectHash) -> CliResult<Vec<u8>> {
    load_object::<Blob>(hash)
        .map(|blob| blob.data)
        .map_err(|error| {
            rev_list_corrupt_error(format!("failed to load blob object {hash}: {error}"))
        })
}

fn record_diff_error(slot: &Rc<RefCell<Option<CliError>>>, error: CliError) {
    let mut slot = slot.borrow_mut();
    if slot.is_none() {
        *slot = Some(error);
    }
}

fn normalize_diff_for_patch_id(path: &str, diff: &str) -> String {
    let mut normalized = String::new();
    normalized.push_str(path);
    normalized.push('\n');

    for line in diff.lines() {
        if line.starts_with("diff --git ")
            || line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
        {
            continue;
        }
        if line.starts_with("@@ ") {
            normalized.push_str("@@\n");
            continue;
        }
        normalized.push_str(line);
        normalized.push('\n');
    }

    normalized
}

fn rev_list_corrupt_error(message: String) -> CliError {
    CliError::fatal(message).with_stable_code(StableErrorCode::RepoCorrupt)
}
