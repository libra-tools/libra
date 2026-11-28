//! Rename arbitration: side/decision detection, directory-rename plans and
//! the incremental and flattening apply paths.
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
        index::Index,
        object::{
            blob::Blob,
            commit::Commit,
            tree::{Tree, TreeItemMode},
        },
    },
};
use serde::{Deserialize, Serialize};
pub(crate) use state::{
    MergeState, merge_in_progress, merge_state_for_pseudo_refs, merge_state_gc_oids,
};
pub(crate) use tree_merge::*;
pub(crate) use virtual_base::*;
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

pub(crate) fn detect_resolve_rename_notices(
    base: &HashMap<PathBuf, MergeTreeEntry>,
    ours: &HashMap<PathBuf, MergeTreeEntry>,
    theirs: &HashMap<PathBuf, MergeTreeEntry>,
    virtual_blobs: &VirtualBlobs,
) -> Vec<ResolveRenameNotice> {
    let config = MergeRenameConfig::default();
    let mut reader = MergeRenameReader::new();
    let mut notices = Vec::new();
    for (side, items) in [(MergeSide::Ours, ours), (MergeSide::Theirs, theirs)] {
        notices.extend(
            detect_side_renames(base, items, &config, virtual_blobs, &mut reader)
                .matches
                .into_iter()
                .map(|pair| ResolveRenameNotice {
                    old: pair.old,
                    new: pair.new,
                    side,
                }),
        );
    }
    notices.sort_by(|left, right| {
        left.old
            .cmp(&right.old)
            .then_with(|| left.new.cmp(&right.new))
            .then_with(|| match (left.side, right.side) {
                (MergeSide::Ours, MergeSide::Theirs) => std::cmp::Ordering::Less,
                (MergeSide::Theirs, MergeSide::Ours) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            })
    });
    notices
}

pub(crate) fn announce_resolve_rename_notices(
    notices: &[ResolveRenameNotice],
    upstream: &str,
    output: &OutputConfig,
) {
    if output.is_json() {
        return;
    }
    for notice in notices {
        info_println!(
            output,
            "notice: resolve strategy disables rename detection; treating {} -> {} in {} as delete/add",
            notice.old.display(),
            notice.new.display(),
            df_branch_label(notice.side, upstream)
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn detect_side_renames(
    base: &HashMap<PathBuf, MergeTreeEntry>,
    side: &HashMap<PathBuf, MergeTreeEntry>,
    config: &MergeRenameConfig,
    virtual_blobs: &VirtualBlobs,
    reader: &mut MergeRenameReader,
) -> SideRenames {
    detect_side_renames_inner(base, side, config, virtual_blobs, reader, false)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn decide_renames(
    base: &HashMap<PathBuf, MergeTreeEntry>,
    ours: &HashMap<PathBuf, MergeTreeEntry>,
    theirs: &HashMap<PathBuf, MergeTreeEntry>,
    our_matches: &[rename_detect::RenameMatch],
    their_matches: &[rename_detect::RenameMatch],
) -> Vec<RenameDecision> {
    let sides = [ours, theirs];
    let by_old: [HashMap<&PathBuf, &PathBuf>; 2] = [
        our_matches
            .iter()
            .map(|pair| (&pair.old, &pair.new))
            .collect(),
        their_matches
            .iter()
            .map(|pair| (&pair.old, &pair.new))
            .collect(),
    ];
    // Every pair, tagged with the side that made it, so the two passes below
    // can look at all of them at once.
    let pairs: Vec<(usize, &rename_detect::RenameMatch)> = our_matches
        .iter()
        .map(|pair| (0usize, pair))
        .chain(their_matches.iter().map(|pair| (1usize, pair)))
        .collect();

    // PASS 1 — everything that does not depend on occupancy.
    let mut declined: Vec<Option<RenameDeclined>> = Vec::with_capacity(pairs.len());
    for (index, pair) in &pairs {
        let other = sides[1 - index];
        let this = sides[*index];
        declined.push(match by_old[1 - index].get(&pair.old) {
            Some(destination) if **destination == pair.new => Some(RenameDeclined::SameDestination),
            Some(destination) => Some(RenameDeclined::DivergentRenames {
                theirs: (*destination).clone(),
            }),
            // The other side must still hold the source AS A FILE, and as the
            // same KIND the rename moved, for a three-way match at the new
            // path. Nothing there at all is Git's rename/delete; something of
            // a DIFFERENT kind (a directory, an empty marker, a symlink) is its
            // `type_changed` branch instead — the two are split below because
            // Git reports and resolves them differently.
            None if !other.get(&pair.old).is_some_and(|entry| {
                entry.mode != TreeItemMode::Tree
                    && this
                        .get(&pair.new)
                        .is_some_and(|moved| same_entry_kind(entry.mode, moved.mode))
            }) =>
            {
                Some(if other.contains_key(&pair.old) {
                    RenameDeclined::SourceTypeChanged
                } else {
                    RenameDeclined::SourceDeleted
                })
            }
            None => None,
        });
    }

    // PASS 2 — occupancy, as a fixed point.
    //
    // A rename takes its source away, so that source stops occupying its own
    // path and the directories above it, and two renames can free each other's
    // destination. But a rename structurally BLOCKED by an ancestor,
    // descendant, or marker never happens, so its source stays — and that can
    // block a further rename in turn. An exact file collision is different:
    // MG-06 resolves it at the destination and consumes the source, as Git
    // does. Releasing once and deciding once gets the structural case wrong
    // (Codex R15 gave the first, R16 the second, which left conflicting index
    // entries for `new` and `new/child`). So: release every eligible source,
    // then re-take only the structurally blocked ones, re-checking the renames
    // that re-taking could affect. Each rename is blocked at most once, so the
    // walk terminates and costs the paths' depth, not the square of the rename
    // count.
    let base_names: HashSet<PathBuf> = occupied_names(base.keys());
    let mut occupancy = [
        side_occupancy(base, sides[0]),
        side_occupancy(base, sides[1]),
    ];
    // Only an entry `side_occupancy` actually COUNTED may be released, and it
    // must be released with the same facts it was counted with. An entry the
    // other side carries unchanged from the base was never counted — releasing
    // it anyway decremented some other occupant's count to zero and let a
    // blocked rename through, which committed a file and a directory under one
    // name (Codex R17).
    let counted_at = |side: usize, path: &PathBuf| -> Option<bool> {
        let entry = sides[side].get(path)?;
        if base.get(path) == Some(entry) {
            return None;
        }
        Some(entry.mode == TreeItemMode::Tree)
    };
    for (slot, (index, pair)) in pairs.iter().enumerate() {
        if declined[slot].is_some() {
            continue;
        }
        let other = 1 - index;
        if let Some(marker) = counted_at(other, &pair.old) {
            occupancy[other].apply(&pair.old, marker, true, -1);
        }
    }
    let destination_dependents = DestinationDependents::from_pairs(&pairs, &declined);
    let mut queue: Vec<usize> = (0..pairs.len())
        .filter(|slot| declined[*slot].is_none())
        .collect();
    while let Some(slot) = queue.pop() {
        if declined[slot].is_some() {
            continue;
        }
        let (index, pair) = pairs[slot];
        let other = 1 - index;
        if !occupancy[other].occupied(&pair.new, base, &base_names) {
            continue;
        }
        let exact_collision = occupancy[other].file_at_path(&pair.new);
        declined[slot] = Some(if exact_collision {
            RenameDeclined::DestinationCollision
        } else {
            RenameDeclined::DestinationBlocked
        });
        if exact_collision {
            continue;
        }
        // The source stays where it is, so it occupies its path again.
        if let Some(marker) = counted_at(other, &pair.old) {
            occupancy[other].apply(&pair.old, marker, true, 1);
            destination_dependents.requeue_blocked_by(&pair.old, &mut queue);
        }
    }

    pairs
        .into_iter()
        .zip(declined)
        .map(|((index, pair), declined)| RenameDecision {
            old: pair.old.clone(),
            new: pair.new.clone(),
            side: if index == 0 {
                MergeSide::Ours
            } else {
                MergeSide::Theirs
            },
            declined,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_directory_renames(
    base: &HashMap<PathBuf, MergeTreeEntry>,
    ours: &mut HashMap<PathBuf, MergeTreeEntry>,
    theirs: &mut HashMap<PathBuf, MergeTreeEntry>,
    our_matches: &mut [rename_detect::RenameMatch],
    their_matches: &mut [rename_detect::RenameMatch],
    config: &MergeRenameConfig,
    conflict_style: ConflictStyle,
    branches: (&str, &str),
    context: &mut TreeMergeContext<'_>,
    forced: &mut Vec<(PathBuf, ConflictKind)>,
) -> Result<Vec<RenameConflictNote>, PullMergeError> {
    if config.directory_renames == DirectoryRenameMode::False || context.depth != 0 {
        return Ok(Vec::new());
    }

    let ours_plan = infer_provisional_directory_renames(our_matches, MergeSide::Ours);
    let theirs_plan = infer_provisional_directory_renames(their_matches, MergeSide::Theirs);
    let mut notes = Vec::new();

    // A split is actionable only when the renaming side actually removed the
    // old directory. Otherwise it is just a set of unrelated file renames.
    for split in ours_plan.splits.iter().chain(theirs_plan.splits.iter()) {
        let (renaming, other): (
            &HashMap<PathBuf, MergeTreeEntry>,
            &HashMap<PathBuf, MergeTreeEntry>,
        ) = match split.side {
            MergeSide::Ours => (&*ours, &*theirs),
            MergeSide::Theirs => (&*theirs, &*ours),
        };
        if !renaming.keys().any(|path| path.starts_with(&split.old)) {
            let mut affected: Vec<(PathBuf, MergeTreeEntry)> = other
                .iter()
                .filter(|(path, entry)| {
                    entry.mode != TreeItemMode::Tree
                        && path.starts_with(&split.old)
                        && base
                            .get(*path)
                            .is_none_or(|base_entry| base_entry.mode == TreeItemMode::Tree)
                })
                .map(|(path, entry)| (path.clone(), *entry))
                .collect();
            affected.sort_by(|(left, _), (right, _)| left.cmp(right));
            let Some((conflict_path, content)) = affected.into_iter().next() else {
                continue;
            };
            if !forced.iter().any(|(path, _)| path == &conflict_path) {
                forced.push((conflict_path, ConflictKind::DirectorySplit { content }));
            }
            notes.push(RenameConflictNote::DirectorySplit {
                old: split.old.clone(),
            });
        }
    }

    for rename in ours_plan.renames.into_iter().chain(theirs_plan.renames) {
        let (renaming, other, other_matches, added_side): (
            &HashMap<PathBuf, MergeTreeEntry>,
            &mut HashMap<PathBuf, MergeTreeEntry>,
            &mut [rename_detect::RenameMatch],
            MergeSide,
        ) = match rename.side {
            MergeSide::Ours => (&*ours, &mut *theirs, their_matches, MergeSide::Theirs),
            MergeSide::Theirs => (&*theirs, &mut *ours, our_matches, MergeSide::Ours),
        };
        // Git's directory rename rule applies only when the old directory is
        // gone on the side that supplied the file renames.
        if renaming.keys().any(|path| path.starts_with(&rename.old)) {
            continue;
        }

        let mut additions: Vec<PathBuf> = other
            .iter()
            .filter(|(path, entry)| {
                entry.mode != TreeItemMode::Tree
                    && path.starts_with(&rename.old)
                    && base
                        .get(*path)
                        .is_none_or(|base_entry| base_entry.mode == TreeItemMode::Tree)
            })
            .map(|(path, _)| path.clone())
            .collect();
        additions.sort();
        for old_path in additions {
            let Ok(relative) = old_path.strip_prefix(&rename.old) else {
                continue;
            };
            let new_path = rename.new.join(relative);
            if new_path == old_path {
                continue;
            }
            let Some(moved) = other.get(&old_path).copied() else {
                continue;
            };

            // Never overwrite a second path from the SAME side. Leave the
            // source addressable and make the ambiguity explicit instead.
            if other.contains_key(&new_path) {
                forced.push((
                    old_path.clone(),
                    ConflictKind::RenameMerged {
                        content: moved,
                        kind: RenameConflictKind::DirectoryRename,
                    },
                ));
                notes.push(RenameConflictNote::DirectoryMove {
                    old: old_path,
                    new: new_path,
                    added_side,
                    rename_side: rename.side,
                    conflict: true,
                });
                continue;
            }

            other.remove(&old_path);
            other.insert(new_path.clone(), moved);
            // If this addition was itself a regular rename destination, the
            // later per-file pass must use its relocated name too.
            for pair in other_matches.iter_mut() {
                if pair.new == old_path {
                    pair.new = new_path.clone();
                }
            }

            let conflict = config.directory_renames == DirectoryRenameMode::Conflict;
            if conflict {
                let counterpart = renaming
                    .get(&new_path)
                    .copied()
                    .filter(|entry| entry.mode != TreeItemMode::Tree);
                let kind = match (rename.side, counterpart) {
                    (MergeSide::Ours, Some(ours_entry)) => rename_destination_conflict(
                        &new_path,
                        &ours_entry,
                        &moved,
                        branches.1,
                        conflict_style,
                        context,
                    )?,
                    (MergeSide::Theirs, Some(theirs_entry)) => rename_destination_conflict(
                        &new_path,
                        &moved,
                        &theirs_entry,
                        branches.1,
                        conflict_style,
                        context,
                    )?,
                    (_, None) => ConflictKind::RenameMerged {
                        content: moved,
                        kind: RenameConflictKind::DirectoryRename,
                    },
                };
                forced.push((new_path.clone(), kind));
            }
            notes.push(RenameConflictNote::DirectoryMove {
                old: old_path,
                new: new_path,
                added_side,
                rename_side: rename.side,
                conflict,
            });
        }
    }
    Ok(notes)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn merge_rename_content(
    path: &Path,
    base: Option<&MergeTreeEntry>,
    ours: &MergeTreeEntry,
    theirs: &MergeTreeEntry,
    ours_label: &str,
    theirs_label: &str,
    base_label: &str,
    conflict_style: ConflictStyle,
    context: &mut TreeMergeContext<'_>,
) -> Result<(MergeTreeEntry, bool), PullMergeError> {
    // Git merges the mode independently of the content: the side that differs
    // from the base wins, and when BOTH differ from it there is no answer —
    // `handle_content_merge` keeps ours' and reports the merge UNCLEAN
    // (`merge-ort.c:2211-2218`), which is what makes a collision print Git's
    // `rename involved in collision` line even for a mode-only divergence
    // (Codex R1 P2-3).
    let (merged_mode, mode_clean) = match base {
        Some(base) if ours.mode != theirs.mode => {
            if ours.mode == base.mode {
                (theirs.mode, true)
            } else if theirs.mode == base.mode {
                (ours.mode, true)
            } else {
                (ours.mode, false)
            }
        }
        None if ours.mode != theirs.mode => (ours.mode, false),
        _ => (ours.mode, true),
    };
    // Git handles trivial OID merges before the file-type-specific rules
    // (`merge-ort.c:2243-2246`). A binary pure rename must carry the other
    // side's source edit, just as a text rename does, without invoking the
    // unmergeable-binary fallback or losing the independently merged mode.
    let trivial_hash =
        if ours.hash == theirs.hash || base.is_some_and(|base| ours.hash == base.hash) {
            Some(theirs.hash)
        } else if base.is_some_and(|base| theirs.hash == base.hash) {
            Some(ours.hash)
        } else {
            None
        };
    if let Some(hash) = trivial_hash {
        return Ok((
            MergeTreeEntry {
                hash,
                mode: merged_mode,
            },
            mode_clean,
        ));
    }
    // `-X ours` / `-X theirs` reach the rename's own content merge too: Git
    // maps `MERGE_VARIANT_OURS`/`THEIRS` onto `ll_opts.variant`
    // (`merge-ort.c:2129-2143`), so the favoured side settles the hunks and the
    // merge comes out clean. `context.favor` is always `None` inside the
    // virtual-ancestor fold, exactly as Git disables the variant at
    // `call_depth > 0` (Codex R1 P1-1).
    let favor = context.favor;
    // A symlink, a gitlink, or binary content has no line-level merge:
    // `ll_binary_merge` takes one whole side. Without `-X` this path keeps
    // ours and reports UNCLEAN; with `-X` it takes the favoured side and is clean.
    if !is_regular_file_mode(ours.mode) || !is_regular_file_mode(theirs.mode) {
        if context.depth > 0 {
            // Git restores the full original entry for recursive symlink
            // conflicts, but keeps clean=false (merge-ort.c:2301-2305).
            // Every rename caller supplies an original; an absent entry needs
            // the optional-path result of virtual_conflict_resolution instead.
            let original = base.copied().ok_or_else(|| {
                PullMergeError::TreeCreate(format!(
                    "cannot construct the virtual ancestor for {base_label}: \
                     the renamed non-file entry has no original version"
                ))
            })?;
            return Ok((original, false));
        }
        return Ok(match favor {
            Some(MergeFavor::Ours) => (*ours, true),
            Some(MergeFavor::Theirs) => (*theirs, true),
            None => (*ours, false),
        });
    }
    // A different original kind is a two-way content merge in Git; its mode
    // still participates independently above (merge-ort.c:2254-2263).
    let base_blob = match base.filter(|entry| is_regular_file_mode(entry.mode)) {
        Some(base) => Some(load_merge_blob(base.hash, context.virtual_blobs)?),
        None => None,
    };
    let ours_blob = load_merge_blob(ours.hash, context.virtual_blobs)?;
    let theirs_blob = load_merge_blob(theirs.hash, context.virtual_blobs)?;
    let base_data: &[u8] = match &base_blob {
        Some(blob) => &blob.data,
        None => &[],
    };
    let driver = context.driver_for_path(path);
    if matches!(driver, SelectedMergeDriver::Builtin(_))
        && (driver.is_builtin(BuiltinMergeDriver::Binary)
            || merge_input_is_binary(base_data)
            || merge_input_is_binary(&ours_blob.data)
            || merge_input_is_binary(&theirs_blob.data))
    {
        if context.depth > 0 {
            // ll_binary_merge(virtual_ancestor) returns orig with LL_MERGE_OK.
            // Preserve the merged mode; no regular original means an empty
            // blob, not either side's bytes or a missing ancestor path.
            let hash = match &base_blob {
                Some(original) => original.id,
                None => {
                    let empty = Blob::from_content_bytes(Vec::new());
                    context.record_merged_blob(&empty)?;
                    empty.id
                }
            };
            return Ok((
                MergeTreeEntry {
                    hash,
                    mode: merged_mode,
                },
                mode_clean,
            ));
        }
        // ll_binary_merge selects bytes only; handle_content_merge retains
        // the mode result even when the selected side has a different mode.
        let (hash, content_clean) = match (driver.fallback_builtin(), favor) {
            (BuiltinMergeDriver::Union, _) | (_, None) => (ours.hash, false),
            (_, Some(MergeFavor::Ours)) => (ours.hash, true),
            (_, Some(MergeFavor::Theirs)) => (theirs.hash, true),
        };
        return Ok((
            MergeTreeEntry {
                hash,
                mode: merged_mode,
            },
            content_clean && mode_clean,
        ));
    }
    let marker_len = conflict_marker_length_at_depth(
        &[base_data, &ours_blob.data, &theirs_blob.data],
        context.depth,
    )
    .saturating_add(1);
    let outcome = match &driver {
        SelectedMergeDriver::Builtin(driver) => merge_bytes_with_input_normalization(
            *driver,
            path,
            base_data,
            &ours_blob.data,
            &theirs_blob.data,
            MergeContentOptions {
                favor,
                conflict_style,
                extra_marker_size: 1 + 2 * context.depth,
                normalization: context.input_normalization,
            },
        )
        .map_err(PullMergeError::TreeCreate)?,
        SelectedMergeDriver::External(driver) => {
            run_external_merge_driver_with_input_normalization(
                &context.external_merge_runtime,
                driver,
                ExternalMergeInput {
                    path,
                    base_id: base.map_or_else(
                        || Blob::from_content_bytes(Vec::new()).id,
                        |entry| entry.hash,
                    ),
                    ours_id: ours.hash,
                    theirs_id: theirs.hash,
                    base: base_data,
                    ours: &ours_blob.data,
                    theirs: &theirs_blob.data,
                    marker_length: marker_len,
                    labels: ExternalMergeLabels {
                        ancestor: base_label,
                        ours: ours_label,
                        theirs: theirs_label,
                    },
                },
                context.input_normalization,
            )
            .map_err(PullMergeError::TreeCreate)?
        }
    };
    let external = matches!(driver, SelectedMergeDriver::External(_));
    let (bytes, clean) = match outcome {
        BuiltinMergeOutcome::Clean(bytes) => (bytes, true),
        BuiltinMergeOutcome::Conflict(bytes) if external => (bytes, false),
        BuiltinMergeOutcome::Conflict(bytes) => (
            relabel_conflict_markers(bytes, marker_len, ours_label, theirs_label, base_label),
            false,
        ),
    };
    let blob = Blob::from_content_bytes(bytes);
    context.record_merged_blob(&blob)?;
    Ok((
        MergeTreeEntry {
            hash: blob.id,
            mode: merged_mode,
        },
        // A mode divergence with no answer keeps the merge unclean even when
        // the CONTENT merged cleanly.
        clean && mode_clean,
    ))
}

pub(crate) fn rename_destination_conflict(
    path: &Path,
    ours: &MergeTreeEntry,
    theirs: &MergeTreeEntry,
    _upstream: &str,
    conflict_style: ConflictStyle,
    context: &mut TreeMergeContext<'_>,
) -> Result<ConflictKind, PullMergeError> {
    let ours_blob = load_merge_blob(ours.hash, context.virtual_blobs)?;
    let theirs_blob = load_merge_blob(theirs.hash, context.virtual_blobs)?;
    let driver = context.driver_for_path(path);
    let content = if ours.hash == theirs.hash {
        *ours
    } else if !is_regular_file_mode(ours.mode)
        || !is_regular_file_mode(theirs.mode)
        || (matches!(driver, SelectedMergeDriver::Builtin(_))
            && (driver.is_builtin(BuiltinMergeDriver::Binary)
                || merge_input_is_binary(&ours_blob.data)
                || merge_input_is_binary(&theirs_blob.data)))
    {
        // Git's binary fallback selects one complete input. Even a favored
        // result still carries the path conflict and both original stages.
        match (driver.fallback_builtin(), context.favor) {
            (BuiltinMergeDriver::Union, _) | (_, Some(MergeFavor::Ours) | None) => *ours,
            (_, Some(MergeFavor::Theirs)) => *theirs,
        }
    } else {
        // A base-less add/add still merges against the empty blob. In
        // particular, an empty added file has no conflicting hunk for -X to
        // choose, so it must not erase the other side's nonempty content.
        match try_merge_blob_contents(path, None, *ours, *theirs, driver.clone(), context)? {
            BlobMergeAttempt::Clean(merged)
            | BlobMergeAttempt::Conflict {
                rendered: Some(merged),
                ..
            } => MergeTreeEntry {
                hash: merged.hash,
                mode: ours.mode,
            },
            BlobMergeAttempt::Conflict { .. } | BlobMergeAttempt::NotApplicable => {
                let bytes = if driver.is_builtin(BuiltinMergeDriver::Binary) {
                    ours_blob.data
                } else {
                    both_changed_conflict_content(
                        None,
                        &ours_blob.data,
                        &theirs_blob.data,
                        &GitConflictLabels {
                            ours: context.ours_label.clone(),
                            base: context.ancestor_label.clone(),
                            theirs: context.theirs_label.clone(),
                        },
                        conflict_style,
                    )
                    .map_err(PullMergeError::TreeCreate)?
                };
                let blob = Blob::from_content_bytes(bytes);
                context.record_merged_blob(&blob)?;
                MergeTreeEntry {
                    hash: blob.id,
                    mode: ours.mode,
                }
            }
        }
    };
    Ok(ConflictKind::RenameMerged {
        content,
        kind: RenameConflictKind::Content,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_renames(
    base: &mut HashMap<PathBuf, MergeTreeEntry>,
    ours: &mut HashMap<PathBuf, MergeTreeEntry>,
    theirs: &mut HashMap<PathBuf, MergeTreeEntry>,
    decisions: &[RenameDecision],
    forced: &mut Vec<(PathBuf, ConflictKind)>,
    conflict_style: ConflictStyle,
    // How each side is named in a conflict marker. `HEAD` / the upstream ref
    // for the merge the user asked for, and Git's virtual-ancestor labels
    // inside the fold — Git labels a rename-involved merge `<branch>:<path>`
    // at every call depth.
    branches: (&str, &str),
    context: &mut TreeMergeContext<'_>,
) -> Result<Vec<RenameConflictNote>, PullMergeError> {
    let label = |side: MergeSide, path: &Path| {
        let branch = match side {
            MergeSide::Ours => branches.0,
            MergeSide::Theirs => branches.1,
        };
        format!("{branch}:{}", path.display())
    };
    let mut notes = Vec::new();
    // A rename/rename(1to2) arrives as one decision per side; Git reports and
    // resolves the PAIR once (`merge-ort.c:3078` consumes `i+1` as well).
    let mut folded: HashSet<PathBuf> = HashSet::new();
    for decision in decisions {
        match &decision.declined {
            // A plain rename, and rename/rename(1to1): Git carries the base to
            // the new path for both. For 1to1 (`merge-ort.c:2991-3018`) BOTH
            // sides already hold the destination, so moving the base is the
            // whole of it and the ordinary content merge decides the rest —
            // which is why 1to1 is a CLEAN merge in Git whenever the contents
            // agree, not the base-less add/add Libra degraded it to before.
            None | Some(RenameDeclined::SameDestination) => {
                let Some(base_entry) = base.remove(&decision.old) else {
                    continue;
                };
                base.insert(decision.new.clone(), base_entry);
                let other: &mut HashMap<PathBuf, MergeTreeEntry> = match decision.side {
                    MergeSide::Ours => &mut *theirs,
                    MergeSide::Theirs => &mut *ours,
                };
                if let Some(entry) = other.remove(&decision.old) {
                    other.insert(decision.new.clone(), entry);
                }
            }
            Some(RenameDeclined::DivergentRenames { theirs: other_path }) => {
                if !folded.insert(decision.old.clone()) {
                    continue;
                }
                let (ours_path, theirs_path) = match decision.side {
                    MergeSide::Ours => (decision.new.clone(), other_path.clone()),
                    MergeSide::Theirs => (other_path.clone(), decision.new.clone()),
                };
                let (Some(base_entry), Some(ours_entry), Some(theirs_entry)) = (
                    base.get(&decision.old).copied(),
                    ours.get(&ours_path).copied(),
                    theirs.get(&theirs_path).copied(),
                ) else {
                    continue;
                };
                let (merged, clean) = merge_rename_content(
                    &ours_path,
                    Some(&base_entry),
                    &ours_entry,
                    &theirs_entry,
                    &label(MergeSide::Ours, &ours_path),
                    &label(MergeSide::Theirs, &theirs_path),
                    &format!("base:{}", decision.old.display()),
                    conflict_style,
                    context,
                )?;
                // Git's `was_binary_blob` fallback (`merge-ort.c:3032-3053`):
                // when the content merge could not actually be performed it
                // just TOOK one whole side, so copying that side's blob to both
                // destinations would overwrite the other side's data. Git
                // detects exactly that shape — an unclean merge whose result IS
                // ours' input — and hands the second destination theirs'
                // original object instead. Git's own regression
                // `t/t6422-merge-rename-corner-cases.sh:1423-1438` requires the
                // two destinations to equal the two sides' originals
                // (Codex R1 P1-2).
                let binary_fallback = !clean && merged == ours_entry;
                let theirs_content = if binary_fallback {
                    theirs_entry
                } else {
                    merged
                };
                if binary_fallback {
                    crate::utils::error::emit_warning(format!(
                        "Cannot merge binary files: {} ({} vs. {})",
                        decision.old.display(),
                        label(MergeSide::Ours, &ours_path),
                        label(MergeSide::Theirs, &theirs_path)
                    ));
                }
                // Git copies ONE merge result into both sides' stages and
                // leaves the base under the OLD name — `merge-ort.c:3036-3068`
                // documents keeping the source at stage 1 as deliberate.
                ours.insert(ours_path.clone(), merged);
                theirs.insert(theirs_path.clone(), theirs_content);
                forced.push((
                    ours_path.clone(),
                    match theirs.get(&ours_path) {
                        Some(added) if context.depth == 0 => rename_destination_conflict(
                            &ours_path,
                            &merged,
                            added,
                            branches.1,
                            conflict_style,
                            context,
                        )?,
                        // The fold discards forced conflicts and resolves the
                        // remapped stages with virtual_conflict_resolution.
                        Some(_) | None => ConflictKind::RenameMerged {
                            content: merged,
                            kind: RenameConflictKind::RenameRename,
                        },
                    },
                ));
                forced.push((
                    theirs_path.clone(),
                    match ours.get(&theirs_path) {
                        Some(added) if context.depth == 0 => rename_destination_conflict(
                            &theirs_path,
                            added,
                            &theirs_content,
                            branches.1,
                            conflict_style,
                            context,
                        )?,
                        Some(_) | None => ConflictKind::RenameMerged {
                            content: theirs_content,
                            kind: RenameConflictKind::RenameRename,
                        },
                    },
                ));
                // INTENTIONAL DEVIATION from Git: the source is resolved by
                // REMOVAL rather than left unmerged at stage 1.
                //
                // Git's own comment at `merge-ort.c:3057-3068` calls keeping it
                // legacy — "For renames we normally remove the path at the old
                // name. It would thus seem consistent to do the same for
                // rename/rename(1to2) cases, but we haven't done so
                // traditionally and a number of the regression tests now encode
                // an expectation that the file is left there at stage 1" — and
                // spells out the two lines that would change it.
                //
                // Libra takes the consistent branch. The primary reason is
                // Git's own verdict above; the secondary one is that keeping
                // the stage would put the conflict out of reach of the ordinary
                // staging flow. The source has no working-tree file, and
                // `libra add` / `libra rm` / `libra restore --staged` all match
                // paths through the staged entry, so none of them can address
                // it (`LBR-CLI-003: pathspec did not match any files`, or
                // `path is unmerged`); `add -A` silently no-ops on it.
                //
                // It is NOT unresolvable, and an earlier version of this note
                // wrongly said so (Codex R1 P1-4): `libra read-tree HEAD`
                // followed by `add -A .` does clear it, as does
                // `update-index --cacheinfo`. Both are blunt — `read-tree`
                // replaces the whole index and so discards every resolution
                // staged so far, and `update-index` is plumbing — so the shape
                // would still be a trap for anyone following the documented
                // `add <path>` + `merge --continue` workflow.
                //
                // Both destinations carry the merged result, which already
                // incorporates the base, so nothing is lost by dropping it.
                base.remove(&decision.old);
                notes.push(RenameConflictNote::RenameRename {
                    old: decision.old.clone(),
                    ours_path,
                    theirs_path,
                });
            }
            Some(reason @ (RenameDeclined::SourceDeleted | RenameDeclined::SourceTypeChanged)) => {
                let type_changed = *reason == RenameDeclined::SourceTypeChanged;
                let other: &HashMap<PathBuf, MergeTreeEntry> = match decision.side {
                    MergeSide::Ours => &*theirs,
                    MergeSide::Theirs => &*ours,
                };
                // rename/add/delete: the destination is ALSO taken by the other
                // side. Git reports rename/delete but leaves the destination
                // "as-is so they look like an add/add conflict"
                // (`merge-ort.c:3180-3188`) — so the base is NOT carried over.
                let destination_taken = other.contains_key(&decision.new);
                let Some(base_entry) = base.remove(&decision.old) else {
                    continue;
                };
                let side: &HashMap<PathBuf, MergeTreeEntry> = match decision.side {
                    MergeSide::Ours => &*ours,
                    MergeSide::Theirs => &*theirs,
                };
                let Some(entry) = side.get(&decision.new).copied() else {
                    continue;
                };
                // A type-changed source SURVIVES under the old name (Git keeps
                // the two side stages and only clears the base's,
                // `merge-ort.c:3209-3211`); a deleted one is already gone from
                // both sides.
                if !type_changed {
                    ours.remove(&decision.old);
                    theirs.remove(&decision.old);
                }
                // Git copies the base to the NEW path's stage 1 whenever it
                // reaches the rename branch (`merge-ort.c:3202-3204`, before
                // the `type_changed` / `source_deleted` split). A COLLISION is
                // what suppresses that — and for a type change Git clears the
                // collision first (`:3090-3120`), so the base still travels.
                // Only rename/add/delete (`collision && source_deleted`) leaves
                // the destination base-less, "so they look like an add/add
                // conflict" (`:3180-3188`). Codex R1 P1-3.
                if type_changed || !destination_taken {
                    base.insert(decision.new.clone(), base_entry);
                }
                if !destination_taken {
                    // A PURE rename plus a delete is still a conflict for Git
                    // even though the content never changed (measured: stages
                    // 1 and 2 hold the same blob), so it is forced here — the
                    // ordinary match would call it a clean delete.
                    forced.push((
                        decision.new.clone(),
                        match decision.side {
                            MergeSide::Ours => {
                                ConflictKind::OursModifiedTheirsDeleted { ours: entry.hash }
                            }
                            MergeSide::Theirs => {
                                ConflictKind::TheirsModifiedOursDeleted { theirs: entry.hash }
                            }
                        },
                    ));
                } else if !type_changed && context.depth == 0 {
                    let (Some(ours_entry), Some(theirs_entry)) =
                        (ours.get(&decision.new), theirs.get(&decision.new))
                    else {
                        continue;
                    };
                    // Git keeps path_conflict after the add/add content merge,
                    // including when -X settles every hunk (merge-ort.c:3190).
                    forced.push((
                        decision.new.clone(),
                        rename_destination_conflict(
                            &decision.new,
                            ours_entry,
                            theirs_entry,
                            branches.1,
                            conflict_style,
                            context,
                        )?,
                    ));
                }
                // Git prints NOTHING extra for a type change: the destination's
                // own modify/delete line is the whole report.
                if !type_changed {
                    notes.push(RenameConflictNote::RenameDelete {
                        old: decision.old.clone(),
                        new: decision.new.clone(),
                        rename_side: decision.side,
                    });
                }
            }
            Some(RenameDeclined::DestinationCollision) => {
                let Some(base_entry) = base.get(&decision.old).copied() else {
                    continue;
                };
                let (side_entry, other_entry) = match decision.side {
                    MergeSide::Ours => (
                        ours.get(&decision.new).copied(),
                        theirs.get(&decision.old).copied(),
                    ),
                    MergeSide::Theirs => (
                        theirs.get(&decision.new).copied(),
                        ours.get(&decision.old).copied(),
                    ),
                };
                let (Some(side_entry), Some(other_entry)) = (side_entry, other_entry) else {
                    continue;
                };
                let (ours_entry, theirs_entry) = match decision.side {
                    MergeSide::Ours => (side_entry, other_entry),
                    MergeSide::Theirs => (other_entry, side_entry),
                };
                let (merged, clean) = merge_rename_content(
                    &decision.new,
                    Some(&base_entry),
                    &ours_entry,
                    &theirs_entry,
                    // Codex R1 P2-1: Git's collision branch sets
                    // `pathnames[other_source_index] = oldpath` and
                    // `pathnames[target_index] = newpath`
                    // (`merge-ort.c:3144-3146`) — only the side that DID the
                    // rename is labelled with the destination.
                    &label(
                        MergeSide::Ours,
                        match decision.side {
                            MergeSide::Ours => &decision.new,
                            MergeSide::Theirs => &decision.old,
                        },
                    ),
                    &label(
                        MergeSide::Theirs,
                        match decision.side {
                            MergeSide::Theirs => &decision.new,
                            MergeSide::Ours => &decision.old,
                        },
                    ),
                    &format!("base:{}", decision.old.display()),
                    conflict_style,
                    context,
                )?;
                // Git stores the rename's own merge at the renaming side's
                // stage of the destination, leaves the other side's entry at
                // its stage, records NO base there, and resolves the source by
                // removal (`merge-ort.c:3137-3179`) — so the destination comes
                // out as the add/add that was measured, for rename/add and
                // rename/rename(2to1) alike.
                match decision.side {
                    MergeSide::Ours => ours.insert(decision.new.clone(), merged),
                    MergeSide::Theirs => theirs.insert(decision.new.clone(), merged),
                };
                base.remove(&decision.old);
                ours.remove(&decision.old);
                theirs.remove(&decision.old);
                if !clean {
                    notes.push(RenameConflictNote::Collision {
                        old: decision.old.clone(),
                        new: decision.new.clone(),
                    });
                }
            }
            Some(RenameDeclined::DestinationBlocked) => {}
        }
    }
    Ok(notes)
}

pub(crate) fn announce_rename_notices(
    notes: &[RenameConflictNote],
    limited_sides: &[MergeSide],
    upstream: &str,
    output: &OutputConfig,
) {
    if output.is_json() {
        return;
    }
    for side in limited_sides {
        info_println!(
            output,
            "notice: skipped inexact rename detection for {} because more than the merge.renameLimit paths changed; exact renames were still detected",
            df_branch_label(*side, upstream)
        );
    }
    // MG-06: Git's own wording, verbatim. Each line is a CONFLICT, not a
    // notice: before this card these shapes degraded to "merging without
    // rename detection", which lost the merge base and with it the conflict.
    for note in notes {
        match note {
            RenameConflictNote::RenameRename {
                old,
                ours_path,
                theirs_path,
            } => info_println!(
                output,
                "CONFLICT (rename/rename): {} renamed to {} in {} and to {} in {}.",
                old.display(),
                ours_path.display(),
                df_branch_label(MergeSide::Ours, upstream),
                theirs_path.display(),
                df_branch_label(MergeSide::Theirs, upstream)
            ),
            RenameConflictNote::RenameDelete {
                old,
                new,
                rename_side,
            } => info_println!(
                output,
                "CONFLICT (rename/delete): {} renamed to {} in {}, but deleted in {}.",
                old.display(),
                new.display(),
                df_branch_label(*rename_side, upstream),
                df_branch_label(
                    match rename_side {
                        MergeSide::Ours => MergeSide::Theirs,
                        MergeSide::Theirs => MergeSide::Ours,
                    },
                    upstream
                )
            ),
            RenameConflictNote::Collision { old, new } => info_println!(
                output,
                "CONFLICT (rename involved in collision): rename of {} -> {} has content conflicts AND collides with another path; this may result in nested conflict markers.",
                old.display(),
                new.display()
            ),
            RenameConflictNote::DirectoryMove {
                old,
                new,
                added_side,
                rename_side,
                conflict: false,
            } => info_println!(
                output,
                "Path updated: {} added in {} inside a directory that was renamed in {}; moving it to {}.",
                old.display(),
                df_branch_label(*added_side, upstream),
                df_branch_label(*rename_side, upstream),
                new.display()
            ),
            RenameConflictNote::DirectoryMove {
                old,
                new,
                added_side,
                rename_side,
                conflict: true,
            } => info_println!(
                output,
                "CONFLICT (file location): {} added in {} inside a directory that was renamed in {}, suggesting it should perhaps be moved to {}.",
                old.display(),
                df_branch_label(*added_side, upstream),
                df_branch_label(*rename_side, upstream),
                new.display()
            ),
            RenameConflictNote::DirectorySplit { old } => info_println!(
                output,
                "CONFLICT (directory rename split): Unclear where to rename {} to; it was renamed to multiple other directories, with no destination getting a majority of the files.",
                old.display()
            ),
        }
    }
}

pub(crate) fn detect_and_apply_renames(
    base: &mut HashMap<PathBuf, MergeTreeEntry>,
    ours: &mut HashMap<PathBuf, MergeTreeEntry>,
    theirs: &mut HashMap<PathBuf, MergeTreeEntry>,
    config: &MergeRenameConfig,
    conflict_style: ConflictStyle,
    branches: (&str, &str),
    context: &mut TreeMergeContext<'_>,
) -> Result<RenameOutcome, PullMergeError> {
    if !config.enabled {
        return Ok(RenameOutcome {
            decisions: Vec::new(),
            limited: Vec::new(),
            notes: Vec::new(),
            forced: Vec::new(),
        });
    }
    // ONE reader for both sides: a blob shared by the two detections (the
    // base's, above all) is then read once.
    let mut reader = MergeRenameReader::new();
    let (mut our_side, mut their_side) = {
        let virtual_blobs = &*context.virtual_blobs;
        (
            detect_side_renames(base, ours, config, virtual_blobs, &mut reader),
            detect_side_renames(base, theirs, config, virtual_blobs, &mut reader),
        )
    };
    if our_side.matches.is_empty()
        && their_side.matches.is_empty()
        && !our_side.skipped_by_limit
        && !their_side.skipped_by_limit
    {
        return Ok(RenameOutcome {
            decisions: Vec::new(),
            limited: Vec::new(),
            notes: Vec::new(),
            forced: Vec::new(),
        });
    }
    let mut forced = Vec::new();
    let mut notes = apply_directory_renames(
        base,
        ours,
        theirs,
        &mut our_side.matches,
        &mut their_side.matches,
        config,
        conflict_style,
        branches,
        context,
        &mut forced,
    )?;
    let decisions = decide_renames(base, ours, theirs, &our_side.matches, &their_side.matches);
    notes.extend(apply_renames(
        base,
        ours,
        theirs,
        &decisions,
        &mut forced,
        conflict_style,
        branches,
        context,
    )?);
    let mut limited = Vec::new();
    if our_side.skipped_by_limit {
        limited.push(MergeSide::Ours);
    }
    if their_side.skipped_by_limit {
        limited.push(MergeSide::Theirs);
    }
    Ok(RenameOutcome {
        decisions,
        limited,
        notes,
        forced,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_incremental_renames(
    source: &mut dyn TreeSource,
    sources: &[Vec<(PathBuf, MergeTreeEntry, Option<MergeTreeEntry>)>; 2],
    dests: &[Vec<(PathBuf, MergeTreeEntry)>; 2],
    config: &MergeRenameConfig,
    merged: &mut HashMap<PathBuf, MergeTreeEntry>,
    conflicts: &mut Vec<(PathBuf, ConflictKind)>,
    files_changed: &mut usize,
    context: &mut TreeMergeContext<'_>,
    // MG-06: the style the rename-driven content merges are rendered with, and
    // the label the other side's paths carry in their conflict markers.
    conflict_style: ConflictStyle,
    upstream: &str,
    // The merge base's root, for MG-04's base-presence rule: a destination
    // directory holding nothing but empty trees is in the way only when the
    // base had NOTHING there (Codex R15). Probed per destination, and only for
    // destinations no FILE already occupies, so a merge without empty markers
    // reads nothing extra.
    base_tree: Option<ObjectHash>,
) -> Result<RenameOutcome, PullMergeError> {
    if !config.enabled {
        return Ok(RenameOutcome {
            decisions: Vec::new(),
            limited: Vec::new(),
            notes: Vec::new(),
            forced: Vec::new(),
        });
    }
    let mut decisions = Vec::new();
    let mut limited = Vec::new();
    let mut per_side: [Vec<rename_detect::RenameMatch>; 2] = [Vec::new(), Vec::new()];
    // Indexed once: the loops below look each candidate up by path, and the
    // lists grow with the change set.
    let source_index: [HashMap<&PathBuf, (MergeTreeEntry, Option<MergeTreeEntry>)>; 2] = [0, 1]
        .map(|index| {
            sources[index]
                .iter()
                .map(|(path, base, other)| (path, (*base, *other)))
                .collect()
        });
    let dest_index: [HashMap<&PathBuf, MergeTreeEntry>; 2] = [0, 1].map(|index| {
        dests[index]
            .iter()
            .map(|(path, entry)| (path, *entry))
            .collect()
    });
    // The same occupancy index the flattening engine builds, from the input
    // facts plus the result's own FILE paths (a carried subtree is checked
    // separately, below): one hash probe per rename instead of a scan.
    let mut reader = MergeRenameReader::new();
    for (index, side) in [MergeSide::Ours, MergeSide::Theirs].into_iter().enumerate() {
        if sources[index].is_empty() || dests[index].is_empty() {
            continue;
        }
        let base_map: HashMap<PathBuf, MergeTreeEntry> = sources[index]
            .iter()
            .map(|(path, base, _)| (path.clone(), *base))
            .collect();
        let side_map: HashMap<PathBuf, MergeTreeEntry> = dests[index].iter().cloned().collect();
        let detected = detect_side_renames(
            &base_map,
            &side_map,
            config,
            context.virtual_blobs,
            &mut reader,
        );
        if detected.skipped_by_limit {
            limited.push(side);
        }
        per_side[index] = detected.matches;
    }
    let other_at = |index: usize, path: &PathBuf| -> Option<MergeTreeEntry> {
        source_index[index].get(path).and_then(|(_, other)| *other)
    };
    let base_at = |index: usize, path: &PathBuf| -> Option<MergeTreeEntry> {
        source_index[index].get(path).map(|(base, _)| *base)
    };
    let dest_at = |index: usize, path: &PathBuf| -> Option<MergeTreeEntry> {
        dest_index[index].get(path).copied()
    };
    // The same rules the flattening engine applies, expressed over the
    // candidate lists: the other side must still HAVE the source, must not
    // have renamed it elsewhere, and must not occupy the destination.
    let mut ours_by_old: HashMap<&PathBuf, &PathBuf> = HashMap::new();
    for pair in &per_side[0] {
        ours_by_old.insert(&pair.old, &pair.new);
    }
    let mut theirs_by_old: HashMap<&PathBuf, &PathBuf> = HashMap::new();
    for pair in &per_side[1] {
        theirs_by_old.insert(&pair.old, &pair.new);
    }
    // What the OTHER side ADDED, as an input fact — independent of how the walk
    // resolved it. The result alone is not enough (Codex R17): when the other
    // side independently adds a file at the rename's destination and the merge
    // resolves that path in its favour, the result holds ONE entry there and
    // the walk cannot tell it apart from the rename's own product, so the
    // rename overwrote the other side's file and exited 0.
    let other_adds: [HashSet<PathBuf>; 2] =
        [0, 1].map(|index| occupied_names(dest_index[index].keys().copied()));
    // The same counted occupancy the flattening engine uses, built from what
    // the walk actually produced: every merged entry and every conflicted path.
    // A conflicted path occupies its name too (Codex R5 P1) — the flattening
    // engine sees it in the side maps, the walk only in this list.
    let mut occupancy = DestinationOccupancy::default();
    for (path, entry) in merged.iter() {
        // Ancestors only: the walk resolved the destination as a plain add, and
        // that entry IS the rename's product.
        occupancy.apply(path, entry.mode == TreeItemMode::Tree, false, 1);
    }
    for (path, _) in conflicts.iter() {
        occupancy.apply(path, false, true, 1);
    }
    let occupies_marker_only = |path: &Path, occupancy: &DestinationOccupancy| {
        occupancy.files.get(path).is_none_or(|count| *count == 0)
            && occupancy.markers.get(path).is_some_and(|count| *count > 0)
    };
    let conflicted_paths: HashSet<&PathBuf> = conflicts.iter().map(|(path, _)| path).collect();
    // `(marker, at_own_path, already_counted)` — the facts needed to release
    // and, when a rename is blocked, restore the other side's source. A D/F
    // candidate is deliberately not yet in `merged` or `conflicts`; the input
    // source tuple is still authoritative there and must be indexed before it
    // can participate in the optimistic release.
    let held_at = |index: usize, path: &PathBuf| -> Option<(bool, bool, bool)> {
        merged
            .get(path)
            .map(|entry| (entry.mode == TreeItemMode::Tree, false, true))
            .or_else(|| {
                conflicted_paths
                    .contains(path)
                    .then_some((false, true, true))
            })
            .or_else(|| {
                other_at(index, path).map(|entry| (entry.mode == TreeItemMode::Tree, true, false))
            })
    };
    // PASS 1 — the declines that do not depend on occupancy.
    let pairs: Vec<(usize, &rename_detect::RenameMatch)> = per_side[0]
        .iter()
        .map(|pair| (0usize, pair))
        .chain(per_side[1].iter().map(|pair| (1usize, pair)))
        .collect();
    let mut declined: Vec<Option<RenameDeclined>> = Vec::with_capacity(pairs.len());
    for (index, pair) in &pairs {
        let other_renamed = if *index == 0 {
            theirs_by_old.get(&pair.old).copied()
        } else {
            ours_by_old.get(&pair.old).copied()
        };
        declined.push(match other_renamed {
            Some(destination) if *destination == pair.new => Some(RenameDeclined::SameDestination),
            Some(destination) => Some(RenameDeclined::DivergentRenames {
                theirs: destination.clone(),
            }),
            // The other side must still hold the source, and hold it as the
            // SAME kind of entry the rename moved: a type change is a delete
            // for rename purposes, as Git treats it.
            None if !other_at(*index, &pair.old).is_some_and(|entry| {
                dest_at(*index, &pair.new)
                    .is_some_and(|moved| same_entry_kind(entry.mode, moved.mode))
            }) =>
            {
                Some(if other_at(*index, &pair.old).is_some() {
                    RenameDeclined::SourceTypeChanged
                } else {
                    RenameDeclined::SourceDeleted
                })
            }
            None => None,
        });
    }

    // PASS 2 — occupancy, as the same fixed point the flattening engine runs:
    // release every eligible source, then re-take only the ones structurally
    // blocked at their destination and re-check what that could affect. An
    // exact file collision consumes its source in the MG-06 collision pass.
    for (slot, (index, pair)) in pairs.iter().enumerate() {
        if declined[slot].is_some() {
            continue;
        }
        if let Some((marker, at_own_path, already_counted)) = held_at(*index, &pair.old) {
            if !already_counted {
                occupancy.apply(&pair.old, marker, at_own_path, 1);
            }
            occupancy.apply(&pair.old, marker, at_own_path, -1);
        }
    }
    let destination_dependents = DestinationDependents::from_pairs(&pairs, &declined);
    let mut base_presence = BasePresence::default();
    let mut queue: Vec<usize> = (0..pairs.len())
        .filter(|slot| declined[*slot].is_none())
        .collect();
    while let Some(slot) = queue.pop() {
        if declined[slot].is_some() {
            continue;
        }
        let (index, pair) = pairs[slot];
        let taken = other_adds[1 - index].contains(&pair.new)
            || occupancy.file_occupied(&pair.new)
            || (occupies_marker_only(&pair.new, &occupancy)
                && !base_holds_anything_at(source, base_tree, &pair.new, &mut base_presence)?)
            // A subtree the walk carries whole IS a directory there — but only
            // if it holds a file (an empty one is not in the way; `git merge`
            // uses the rename then, verified).
            || carried_subtree_holds_a_file(source, merged, &pair.new)?;
        if !taken {
            continue;
        }
        let exact_collision =
            dest_at(1 - index, &pair.new).is_some_and(|entry| entry.mode != TreeItemMode::Tree);
        declined[slot] = Some(if exact_collision {
            RenameDeclined::DestinationCollision
        } else {
            RenameDeclined::DestinationBlocked
        });
        if exact_collision {
            continue;
        }
        if let Some((marker, at_own_path, _)) = held_at(index, &pair.old) {
            occupancy.apply(&pair.old, marker, at_own_path, 1);
            destination_dependents.requeue_blocked_by(&pair.old, &mut queue);
        }
    }
    for ((index, pair), declined) in pairs.into_iter().zip(declined) {
        decisions.push(RenameDecision {
            old: pair.old.clone(),
            new: pair.new.clone(),
            side: if index == 0 {
                MergeSide::Ours
            } else {
                MergeSide::Theirs
            },
            declined,
        });
    }
    // A rename's destination can sit INSIDE a subtree the walk carries whole
    // (`shared` as one `TreeItemMode::Tree` entry). Writing both that entry
    // and a `shared/moved.txt` leaf would put two entries under one name in
    // the result tree, so the covering subtrees are expanded into leaves
    // first — only those, and only when a rename actually lands there.
    let touched: Vec<PathBuf> = decisions
        .iter()
        .filter(|decision| decision.declined.is_none())
        .flat_map(|decision| [decision.old.clone(), decision.new.clone()])
        .collect();
    expand_subtrees_covering(source, merged, &touched)?;
    // One pass to collect the paths the accepted renames will take over, then
    // ONE prune of the conflict list. Conflicts the resolution below pushes are
    // added afterwards, so they survive; two accepted renames never share a
    // path (a shared destination is declined as `SameDestination` or
    // `DestinationCollision`), so the order within the pass does not matter.
    let mut renamed_paths: HashSet<PathBuf> = HashSet::new();
    for decision in &decisions {
        if decision.declined.is_some() {
            continue;
        }
        let index = match decision.side {
            MergeSide::Ours => 0,
            MergeSide::Theirs => 1,
        };
        if base_at(index, &decision.old).is_some()
            && other_at(index, &decision.old).is_some()
            && dest_at(index, &decision.new).is_some()
        {
            renamed_paths.insert(decision.old.clone());
            renamed_paths.insert(decision.new.clone());
        }
    }
    conflicts.retain(|(path, _)| !renamed_paths.contains(path));
    for decision in &decisions {
        if decision.declined.is_some() {
            continue;
        }
        let index = match decision.side {
            MergeSide::Ours => 0,
            MergeSide::Theirs => 1,
        };
        let (Some(base_entry), Some(other_entry), Some(side_entry)) = (
            base_at(index, &decision.old),
            other_at(index, &decision.old),
            dest_at(index, &decision.new),
        ) else {
            continue;
        };
        // Drop what the walk decided for the two paths on their own. The
        // conflict list is pruned ONCE, above, rather than per decision:
        // exact renames are deliberately not capped by `merge.renameLimit`, so
        // scanning the whole list per accepted rename would be quadratic in
        // the tree size on a merge that renames a lot.
        merged.remove(&decision.old);
        merged.remove(&decision.new);
        let (ours_entry, theirs_entry) = match decision.side {
            MergeSide::Ours => (side_entry, other_entry),
            MergeSide::Theirs => (other_entry, side_entry),
        };
        let resolved = match resolve_three_way(
            &decision.new,
            Some(&base_entry),
            Some(&ours_entry),
            Some(&theirs_entry),
            context,
        )? {
            MergeResolution::Use(entry) => {
                merged.insert(decision.new.clone(), entry);
                Some(entry)
            }
            MergeResolution::Delete => None,
            MergeResolution::Conflict(kind) => {
                conflicts.push((decision.new.clone(), kind));
                None
            }
        };
        // `files_changed` must equal what the flattening engine counts over its
        // REMAPPED maps (`count_item_map_changes(ours, merged)`), so correct
        // what the walk counted for the two paths on their own:
        //   * after the remap ours holds the file at the NEW path (its own copy
        //     when ours renamed, the moved one when theirs did), and the source
        //     path is gone from both sides — no change there;
        //   * so the only change is "the result at the new path differs from
        //     ours' entry there".
        // The walk, seeing the paths separately, counted nothing for an ours
        // rename (it kept ours' file at the new path and ours never had the
        // source) and two for a theirs rename (source deleted, destination
        // added).
        // A theirs rename adds one ONLY when Git's diffstat would show a line
        // the remapped comparison does not: either the content is unchanged
        // (the comparison sees nothing, Git sees one `old => new`), or the pair
        // no longer reads as a rename at all (the comparison sees one, Git sees
        // a delete AND an add). A rename that changed content and still reads
        // as one is already counted by the first term (Codex R19, then R20).
        let ours_after_remap = Some(ours_entry);
        let changed_at_new_path = resolved != ours_after_remap;
        let still_a_rename = !changed_at_new_path
            || resolved.is_some_and(|entry| {
                pair_reads_as_a_rename(
                    &ours_entry,
                    &entry,
                    config,
                    context.virtual_blobs,
                    &mut reader,
                )
            });
        let truth = usize::from(changed_at_new_path)
            + usize::from(
                decision.side == MergeSide::Theirs && !(changed_at_new_path && still_a_rename),
            );
        let counted_by_walk = match decision.side {
            MergeSide::Ours => 0,
            MergeSide::Theirs => 2,
        };
        *files_changed = (*files_changed + truth).saturating_sub(counted_by_walk);
    }
    // MG-06: the declines the walk left as "no rename" become Git's PATH-LEVEL
    // conflicts here, over the walk's own output. The flattening engine does
    // the same surgery on its maps before it resolves anything
    // ([`apply_renames`]); both engines must land on the same result, which is
    // what the double-walk cases assert.
    let mut notes = Vec::new();
    let mut folded: HashSet<PathBuf> = HashSet::new();
    // A 2to1 collision updates each destination stage separately. Keep the
    // first source's merged content when processing the second source.
    let mut collision_inputs: HashMap<PathBuf, [Option<MergeTreeEntry>; 2]> = HashMap::new();
    // Whatever the walk decided for a path the rename pass takes over is
    // replaced wholesale: its merged entry and any conflict it recorded go,
    // and the rename's own verdict (if any) takes their place.
    let settle = |path: &PathBuf,
                  kind: Option<ConflictKind>,
                  merged: &mut HashMap<PathBuf, MergeTreeEntry>,
                  conflicts: &mut Vec<(PathBuf, ConflictKind)>| {
        merged.remove(path);
        conflicts.retain(|(other, _)| other != path);
        if let Some(kind) = kind {
            conflicts.push((path.clone(), kind));
        }
    };
    for decision in &decisions {
        let index = match decision.side {
            MergeSide::Ours => 0,
            MergeSide::Theirs => 1,
        };
        match &decision.declined {
            None => {}
            Some(RenameDeclined::SameDestination) => {
                // rename/rename(1to1): Git carries the base to the shared
                // destination and merges normally there
                // (`merge-ort.c:2991-3018`), so the add/add the walk saw
                // becomes a three-way merge that can come out CLEAN.
                let (Some(base_entry), Some(ours_entry), Some(theirs_entry)) = (
                    base_at(index, &decision.old),
                    dest_at(0, &decision.new),
                    dest_at(1, &decision.new),
                ) else {
                    continue;
                };
                if !folded.insert(decision.new.clone()) {
                    continue;
                }
                let resolved = resolve_three_way(
                    &decision.new,
                    Some(&base_entry),
                    Some(&ours_entry),
                    Some(&theirs_entry),
                    context,
                )?;
                let before_counted = merged
                    .get(&decision.new)
                    .is_none_or(|entry| *entry != ours_entry)
                    || conflicts.iter().any(|(path, _)| *path == decision.new);
                settle(
                    &decision.new,
                    match resolved {
                        MergeResolution::Conflict(kind) => Some(kind),
                        _ => None,
                    },
                    merged,
                    conflicts,
                );
                let after = match resolved {
                    MergeResolution::Use(entry) => {
                        merged.insert(decision.new.clone(), entry);
                        Some(entry)
                    }
                    MergeResolution::Delete => None,
                    MergeResolution::Conflict(_) => None,
                };
                let after_counted = after != Some(ours_entry);
                *files_changed = (*files_changed + usize::from(after_counted))
                    .saturating_sub(usize::from(before_counted));
                // The old path is gone from both sides already; the walk
                // resolved it as a clean delete, which is what Git does.
            }
            Some(RenameDeclined::DivergentRenames { theirs: other_path }) => {
                if !folded.insert(decision.old.clone()) {
                    continue;
                }
                let (ours_path, theirs_path) = match decision.side {
                    MergeSide::Ours => (decision.new.clone(), other_path.clone()),
                    MergeSide::Theirs => (other_path.clone(), decision.new.clone()),
                };
                let (Some(base_entry), Some(ours_entry), Some(theirs_entry)) = (
                    base_at(index, &decision.old),
                    dest_at(0, &ours_path),
                    dest_at(1, &theirs_path),
                ) else {
                    continue;
                };
                let (merged_entry, merged_clean) = merge_rename_content(
                    &ours_path,
                    Some(&base_entry),
                    &ours_entry,
                    &theirs_entry,
                    &format!(
                        "{}:{}",
                        df_branch_label(MergeSide::Ours, upstream),
                        ours_path.display()
                    ),
                    &format!(
                        "{}:{}",
                        df_branch_label(MergeSide::Theirs, upstream),
                        theirs_path.display()
                    ),
                    &format!("base:{}", decision.old.display()),
                    conflict_style,
                    context,
                )?;
                // Git's `was_binary_blob` fallback (`merge-ort.c:3032-3053`):
                // when the content merge could not actually be performed it
                // just TOOK one whole side, so copying that side's blob to both
                // destinations would overwrite the other side's data. Git
                // detects exactly that shape — an unclean merge whose result IS
                // ours' input — and hands the second destination theirs'
                // original object instead. Git's own regression
                // `t/t6422-merge-rename-corner-cases.sh:1423-1438` requires the
                // two destinations to equal the two sides' originals
                // (Codex R1 P1-2).
                let theirs_content = if !merged_clean && merged_entry == ours_entry {
                    theirs_entry
                } else {
                    merged_entry
                };
                for (path, content, rename_side) in [
                    (&ours_path, merged_entry, 0),
                    (&theirs_path, theirs_content, 1),
                ] {
                    let original_ours = dest_at(0, path);
                    let before_counted = merged.get(path).copied() != original_ours;
                    let kind = match dest_at(1 - rename_side, path) {
                        Some(added) => {
                            let (ours_side, theirs_side) = if rename_side == 0 {
                                (content, added)
                            } else {
                                (added, content)
                            };
                            rename_destination_conflict(
                                path,
                                &ours_side,
                                &theirs_side,
                                upstream,
                                conflict_style,
                                context,
                            )?
                        }
                        None => ConflictKind::RenameMerged {
                            content,
                            kind: RenameConflictKind::RenameRename,
                        },
                    };
                    settle(path, Some(kind), merged, conflicts);
                    let ours_after_remap = if rename_side == 0 {
                        Some(content)
                    } else {
                        original_ours
                    };
                    *files_changed = (*files_changed + usize::from(ours_after_remap.is_some()))
                        .saturating_sub(usize::from(before_counted));
                }
                // The source is resolved by removal, not left unmerged — see
                // the deviation note in `apply_renames`.
                settle(&decision.old, None, merged, conflicts);
                notes.push(RenameConflictNote::RenameRename {
                    old: decision.old.clone(),
                    ours_path,
                    theirs_path,
                });
            }
            Some(reason @ (RenameDeclined::SourceDeleted | RenameDeclined::SourceTypeChanged)) => {
                let type_changed = *reason == RenameDeclined::SourceTypeChanged;
                let Some(side_entry) = dest_at(index, &decision.new) else {
                    continue;
                };
                // rename/add/delete: the destination is ALSO taken, and Git
                // leaves it looking like an add/add (`merge-ort.c:3180-3188`).
                let destination_taken = dest_at(1 - index, &decision.new).is_some();
                if !destination_taken {
                    settle(
                        &decision.new,
                        Some(match decision.side {
                            MergeSide::Ours => ConflictKind::OursModifiedTheirsDeleted {
                                ours: side_entry.hash,
                            },
                            MergeSide::Theirs => ConflictKind::TheirsModifiedOursDeleted {
                                theirs: side_entry.hash,
                            },
                        }),
                        merged,
                        conflicts,
                    );
                    // Codex R1 P1-6. The walk decided the destination on its
                    // own: when OURS renamed, it saw ours' own entry there and
                    // counted nothing; when THEIRS renamed, it saw an add that
                    // differs from ours' absence and counted one. Forcing the
                    // conflict takes the path out of the result, so the truth —
                    // what `count_item_map_changes(ours_after_remap, merged)`
                    // sees — is one when ours holds a file there after the
                    // remap (i.e. ours did the rename) and zero otherwise.
                    let truth = usize::from(decision.side == MergeSide::Ours);
                    let counted_by_walk = usize::from(decision.side == MergeSide::Theirs);
                    *files_changed = (*files_changed + truth).saturating_sub(counted_by_walk);
                } else if !type_changed {
                    let (Some(ours_entry), Some(theirs_entry)) =
                        (dest_at(0, &decision.new), dest_at(1, &decision.new))
                    else {
                        continue;
                    };
                    let before_counted = merged.get(&decision.new).copied() != Some(ours_entry);
                    settle(
                        &decision.new,
                        Some(rename_destination_conflict(
                            &decision.new,
                            &ours_entry,
                            &theirs_entry,
                            upstream,
                            conflict_style,
                            context,
                        )?),
                        merged,
                        conflicts,
                    );
                    *files_changed =
                        (*files_changed + 1).saturating_sub(usize::from(before_counted));
                }
                if type_changed && destination_taken {
                    // The base travels to the destination even though something
                    // occupies it (Codex R1 P1-3), so the walk's base-less
                    // add/add verdict has to be replaced by the three-way the
                    // flattening engine forms there.
                    if let (Some(base_entry), Some(other_entry)) = (
                        base_at(index, &decision.old),
                        dest_at(1 - index, &decision.new),
                    ) {
                        let (ours_side, theirs_side) = match decision.side {
                            MergeSide::Ours => (side_entry, other_entry),
                            MergeSide::Theirs => (other_entry, side_entry),
                        };
                        let resolved = resolve_three_way(
                            &decision.new,
                            Some(&base_entry),
                            Some(&ours_side),
                            Some(&theirs_side),
                            context,
                        )?;
                        settle(
                            &decision.new,
                            match resolved {
                                MergeResolution::Conflict(kind) => Some(kind),
                                _ => None,
                            },
                            merged,
                            conflicts,
                        );
                        if let MergeResolution::Use(entry) = resolved {
                            merged.insert(decision.new.clone(), entry);
                        }
                    }
                }
                if type_changed {
                    // Git clears only the BASE bit at the source
                    // (`oldinfo->filemask &= 0x06`, `merge-ort.c:3211`), which
                    // leaves the type-changed entry as a plain one-sided add —
                    // the walk saw base + one side and called it modify/delete,
                    // so its verdict has to be replaced. Measured on git 2.50.1
                    // (`/Volumes/Data/tmp/mg06-git/ktype`): the result tree
                    // holds `120000 old` beside the conflicted `new`.
                    if let Some(surviving) = other_at(index, &decision.old) {
                        settle(&decision.old, None, merged, conflicts);
                        merged.insert(decision.old.clone(), surviving);
                    }
                } else {
                    settle(&decision.old, None, merged, conflicts);
                    notes.push(RenameConflictNote::RenameDelete {
                        old: decision.old.clone(),
                        new: decision.new.clone(),
                        rename_side: decision.side,
                    });
                }
            }
            Some(RenameDeclined::DestinationCollision) => {
                let (Some(base_entry), Some(side_entry), Some(other_entry)) = (
                    base_at(index, &decision.old),
                    dest_at(index, &decision.new),
                    other_at(index, &decision.old),
                ) else {
                    continue;
                };
                let (ours_entry, theirs_entry) = match decision.side {
                    MergeSide::Ours => (side_entry, other_entry),
                    MergeSide::Theirs => (other_entry, side_entry),
                };
                let (rename_merged, clean) = merge_rename_content(
                    &decision.new,
                    Some(&base_entry),
                    &ours_entry,
                    &theirs_entry,
                    // Codex R1 P2-1: only the renaming side carries the
                    // destination path; the other side keeps the source.
                    &format!(
                        "{}:{}",
                        df_branch_label(MergeSide::Ours, upstream),
                        match decision.side {
                            MergeSide::Ours => decision.new.display(),
                            MergeSide::Theirs => decision.old.display(),
                        }
                    ),
                    &format!(
                        "{}:{}",
                        df_branch_label(MergeSide::Theirs, upstream),
                        match decision.side {
                            MergeSide::Theirs => decision.new.display(),
                            MergeSide::Ours => decision.old.display(),
                        }
                    ),
                    &format!("base:{}", decision.old.display()),
                    conflict_style,
                    context,
                )?;
                // The destination becomes an add/add between the rename's own
                // merge and whatever the other side put there — no base stage,
                // exactly as measured for rename/add and rename/rename(2to1).
                let destination = collision_inputs
                    .entry(decision.new.clone())
                    .or_insert_with(|| [dest_at(0, &decision.new), dest_at(1, &decision.new)]);
                let counted_destination = merged.get(&decision.new).copied() != destination[0];
                let original_source = match decision.side {
                    MergeSide::Ours => None,
                    MergeSide::Theirs => Some(other_entry),
                };
                let counted_source = merged.get(&decision.old).copied() != original_source;
                destination[index] = Some(rename_merged);
                let [ours_side, theirs_side] = *destination;
                let resolved = resolve_three_way(
                    &decision.new,
                    None,
                    ours_side.as_ref(),
                    theirs_side.as_ref(),
                    context,
                )?;
                settle(
                    &decision.new,
                    match resolved {
                        MergeResolution::Conflict(kind) => Some(kind),
                        _ => None,
                    },
                    merged,
                    conflicts,
                );
                if let MergeResolution::Use(entry) = resolved {
                    merged.insert(decision.new.clone(), entry);
                }
                settle(&decision.old, None, merged, conflicts);
                // Compare against the same remapped ours that the flat path
                // uses. Consuming a source removes its earlier walk count;
                // a destination shared by two renames is counted only once.
                let changed_destination = merged.get(&decision.new).copied() != ours_side;
                *files_changed = (*files_changed + usize::from(changed_destination))
                    .saturating_sub(usize::from(counted_destination) + usize::from(counted_source));
                if !clean {
                    notes.push(RenameConflictNote::Collision {
                        old: decision.old.clone(),
                        new: decision.new.clone(),
                    });
                }
            }
            Some(RenameDeclined::DestinationBlocked) => {}
        }
    }
    Ok(RenameOutcome {
        decisions,
        limited,
        notes,
        forced: Vec::new(),
    })
}
