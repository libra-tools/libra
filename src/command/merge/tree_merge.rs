//! Merge tree arbitration: the three-way/octopus tree walk, item-map
//! comparison and blob-index staging, owned by the merge command.
#![allow(unused_imports)]
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

// Preserve the existing command::merge type path for downstream callers.
#[allow(unused_imports)]
pub(crate) use autostash::StoppedMerge;
pub(crate) use autostash::{
    MergeAutostash, conclude_merge_after_commit, conclude_stopped_merge, snapshot_stopped_merge,
};
use autostash::{
    preflight_held_autostash, prepare_merge_autostash, resolve_pending_autostash,
    resolve_pending_autostash_with, store_pending_autostash, verify_autostash_ownership,
};
use clap::{Parser, ValueEnum};
pub(crate) use conflict::*;
pub(crate) use content::*;
use git_internal::{
    hash::ObjectHash,
    internal::{
        index::{Index, IndexEntry},
        object::{
            blob::Blob,
            commit::Commit,
            signature::{Signature, SignatureType},
            tree::{Tree, TreeItemMode},
        },
    },
};
use serde::{Deserialize, Serialize};
pub(crate) use state::{
    MergeState, merge_in_progress, merge_state_for_pseudo_refs, merge_state_gc_oids,
};
pub(crate) use workdir::*;

use super::{
    get_target_commit, load_object, load_object_raw, rename_detect, reset,
    restore::{self, RestoreArgs},
    save_object, status, switch, *,
};
use crate::{
    command::{
        commit::{CleanupMode, cleanup_commit_message, parse_cleanup_mode},
        editor,
    },
    common_utils::{format_commit_msg, parse_commit_msg},
    info_println,
    internal::{
        branch::{Branch, BranchStoreError},
        config::ConfigKv,
        db::get_db_conn_instance,
        head::Head,
        merge_base,
        reflog::{ReflogAction, ReflogContext, with_reflog},
        repo_hooks::{
            RepoHook, replay_repo_hook_output, run_advisory_repo_hook, run_repo_hook_with_io,
        },
        tree_plumbing,
    },
    utils::{
        attributes::{self, AttributeState},
        error::{CliError, CliResult, StableErrorCode},
        object_ext::TreeExt,
        output::{OutputConfig, emit_json_data},
        path, util, worktree,
    },
};

pub(crate) fn automatic_merge_tree(
    merged_items: &HashMap<PathBuf, MergeTreeEntry>,
    placements: &[(PathBuf, ConflictKind, Option<PathBuf>)],
    labels: &GitConflictLabels,
    conflict_style: ConflictStyle,
) -> Result<ObjectHash, PullMergeError> {
    let mut items = merged_items.clone();
    for (path, kind, original) in placements {
        let entry = if original.is_some() {
            match moved_file_content(kind) {
                Some(entry) => entry,
                None => automatic_conflict_entry(kind, labels, conflict_style)?,
            }
        } else {
            automatic_conflict_entry(kind, labels, conflict_style)?
        };
        items.insert(path.clone(), entry);
    }
    create_tree_from_items_map(&items).map_err(PullMergeError::TreeCreate)
}

pub(crate) fn automatic_conflict_entry(
    kind: &ConflictKind,
    labels: &GitConflictLabels,
    conflict_style: ConflictStyle,
) -> Result<MergeTreeEntry, PullMergeError> {
    let content = match *kind {
        ConflictKind::BothChanged {
            rendered: Some(entry),
            ..
        } => return Ok(entry),
        ConflictKind::BothChanged {
            base,
            ours,
            theirs,
            driver,
            rendered: None,
        } => {
            let ours_blob: Blob = load_object(&ours).map_err(|error| PullMergeError::TreeLoad {
                tree_id: ours.to_string(),
                detail: error.to_string(),
            })?;
            let theirs_blob: Blob =
                load_object(&theirs).map_err(|error| PullMergeError::TreeLoad {
                    tree_id: theirs.to_string(),
                    detail: error.to_string(),
                })?;
            match driver {
                BuiltinMergeDriver::Binary => ours_blob.data,
                BuiltinMergeDriver::Union => {
                    let base_data = match base {
                        Some(base) => {
                            load_object::<Blob>(&base)
                                .map_err(|error| PullMergeError::TreeLoad {
                                    tree_id: base.to_string(),
                                    detail: error.to_string(),
                                })?
                                .data
                        }
                        None => Vec::new(),
                    };
                    match merge_bytes_with_refined_driver_labeled(
                        driver,
                        &base_data,
                        &ours_blob.data,
                        &theirs_blob.data,
                        None,
                        conflict_style,
                        0,
                        labels.as_marker_labels(),
                    )
                    .map_err(PullMergeError::TreeCreate)?
                    {
                        BuiltinMergeOutcome::Clean(bytes)
                        | BuiltinMergeOutcome::Conflict(bytes) => bytes,
                    }
                }
                BuiltinMergeDriver::Text => both_changed_conflict_content(
                    base,
                    &ours_blob.data,
                    &theirs_blob.data,
                    labels,
                    conflict_style,
                )
                .map_err(PullMergeError::TreeCreate)?,
            }
        }
        ConflictKind::OursModifiedTheirsDeleted { ours } => {
            let ours_blob: Blob = load_object(&ours).map_err(|error| PullMergeError::TreeLoad {
                tree_id: ours.to_string(),
                detail: error.to_string(),
            })?;
            // Git leaves the modified side in the tree verbatim.
            ours_blob.data
        }
        ConflictKind::TheirsModifiedOursDeleted { theirs } => {
            let theirs_blob: Blob =
                load_object(&theirs).map_err(|error| PullMergeError::TreeLoad {
                    tree_id: theirs.to_string(),
                    detail: error.to_string(),
                })?;
            theirs_blob.data
        }
        ConflictKind::FileDirectory { file, .. } => return Ok(file),
        ConflictKind::RenameMerged { content, .. } | ConflictKind::DirectorySplit { content } => {
            return Ok(content);
        }
        ConflictKind::RenameSourceOnly { entry } => {
            let blob: Blob =
                load_object(&entry.hash).map_err(|error| PullMergeError::TreeLoad {
                    tree_id: entry.hash.to_string(),
                    detail: error.to_string(),
                })?;
            blob.data
        }
    };
    let blob = Blob::from_content_bytes(content);
    save_object(&blob, &blob.id).map_err(|error| PullMergeError::TreeCreate(error.to_string()))?;
    Ok(MergeTreeEntry {
        hash: blob.id,
        mode: TreeItemMode::Blob,
    })
}

pub(crate) fn merge_tree_items(
    base_items: &HashMap<PathBuf, MergeTreeEntry>,
    our_items: &HashMap<PathBuf, MergeTreeEntry>,
    their_items: &HashMap<PathBuf, MergeTreeEntry>,
    context: &mut TreeMergeContext<'_>,
) -> Result<ThreeWayMergeResult, PullMergeError> {
    let mut all_paths: HashSet<PathBuf> = base_items.keys().cloned().collect();
    all_paths.extend(our_items.keys().cloned());
    all_paths.extend(their_items.keys().cloned());

    let mut merged_items = HashMap::new();
    let mut conflicts = Vec::new();
    for path in all_paths {
        let [base, ours, theirs] = sides_without_empty_dir_beside_file(
            base_items.get(&path),
            our_items.get(&path),
            their_items.get(&path),
        );
        match resolve_three_way(&path, base, ours, theirs, context)? {
            MergeResolution::Use(hash) => {
                merged_items.insert(path, hash);
            }
            MergeResolution::Delete => {}
            MergeResolution::Conflict(kind) => conflicts.push((path, kind)),
        }
    }

    // MG-04: every path that is a file on exactly one side while entries exist
    // beneath it (in the result or in any input) is a D/F candidate; the
    // post-pass decides whether the directory really survives. Component-wise
    // path order puts every `foo/...` entry directly after `foo`, so one sorted
    // pass finds them without a quadratic scan. A candidate is recorded even
    // when the file's own resolution deleted it (a strategy option may have),
    // and an empty-directory marker never counts as a file.
    let side_file = |items: &HashMap<PathBuf, MergeTreeEntry>, path: &PathBuf| {
        items
            .get(path)
            .copied()
            .filter(|entry| entry.mode != TreeItemMode::Tree)
    };
    let mut ordered: Vec<&PathBuf> = merged_items
        .keys()
        .chain(conflicts.iter().map(|(path, _)| path))
        .chain(our_items.keys())
        .chain(their_items.keys())
        .collect();
    ordered.sort();
    ordered.dedup();
    let mut candidates: Vec<DfCandidate> = ordered
        .windows(2)
        .filter(|pair| pair[1].starts_with(pair[0]))
        .filter_map(|pair| {
            let path = pair[0];
            let (file_side, file) = match (side_file(our_items, path), side_file(their_items, path))
            {
                (Some(file), None) => (MergeSide::Ours, file),
                (None, Some(file)) => (MergeSide::Theirs, file),
                _ => return None,
            };
            Some(DfCandidate {
                path: path.clone(),
                file_side,
                file,
                base_file: side_file(base_items, path),
                // Filled in below, once (and only if) there is a candidate.
                base_present: false,
            })
        })
        .collect();
    if !candidates.is_empty() {
        let mut base_paths: Vec<&PathBuf> = base_items.keys().collect();
        base_paths.sort();
        for candidate in &mut candidates {
            let at = base_paths.partition_point(|path| path.as_path() < candidate.path.as_path());
            candidate.base_present = base_paths
                .get(at)
                .is_some_and(|path| path.starts_with(&candidate.path));
        }
    }
    // The flattening engine's only tree entries are empty-directory markers.
    let mut no_subtrees = |_: &ObjectHash| Ok(false);
    resolve_df_conflicts(
        &mut merged_items,
        &mut conflicts,
        candidates,
        &mut no_subtrees,
    )?;
    // Empty-directory entries served the D/F decision; the flat result is
    // leaves only (it never rebuilds empty trees — registered in MG-03).
    merged_items.retain(|_, entry| entry.mode != TreeItemMode::Tree);

    Ok(ThreeWayMergeResult {
        merged_items,
        conflicts,
    })
}

pub(crate) fn count_item_map_changes(
    before: &HashMap<PathBuf, MergeTreeEntry>,
    after: &HashMap<PathBuf, MergeTreeEntry>,
) -> usize {
    let mut paths: HashSet<PathBuf> = before.keys().cloned().collect();
    paths.extend(after.keys().cloned());
    paths
        .into_iter()
        .filter(|path| {
            // An empty-directory marker (MG-04's flat view) is not a file: it
            // reads as ABSENT, so an empty directory turning into a file is
            // one added file, and a marker on both sides is no change.
            let file = |entry: Option<&MergeTreeEntry>| {
                entry
                    .copied()
                    .filter(|entry| entry.mode != TreeItemMode::Tree)
            };
            file(before.get(path)) != file(after.get(path))
        })
        .count()
}

pub(crate) fn add_blob_index_entry(
    index: &mut Index,
    path: &Path,
    item: MergeTreeEntry,
    stage: u8,
) -> Result<(), PullMergeError> {
    // A gitlink records a SUBMODULE's commit id, which is not an object of this
    // repository — asking for it as a blob would fail. Only a pass-through
    // gitlink (identical on all three sides, ADR-MG-01) ever reaches here, so
    // the pointer is registered verbatim with a zero size.
    let size = if item.mode == TreeItemMode::Commit {
        0
    } else {
        let blob: Blob = load_object(&item.hash).map_err(|error| {
            PullMergeError::IndexSave(format!(
                "failed to load blob {} for index entry '{}': {error}",
                item.hash,
                path.display()
            ))
        })?;
        blob.data.len() as u32
    };
    let mut entry =
        IndexEntry::new_from_blob(path_to_index_key(path)?.to_string(), item.hash, size);
    entry.mode = tree_item_mode_to_index_mode(item.mode)?;
    entry.flags.stage = stage;
    index.add(entry);
    Ok(())
}
