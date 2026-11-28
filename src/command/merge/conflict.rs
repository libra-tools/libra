//! Merge conflict staging/materialization: marker rendering, fixed-format
//! conflict side classification and three-way resolution helpers.
#![allow(unused_imports)]
use std::{
    borrow::Cow,
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

pub(crate) fn classify_relative_to_base(
    base: Option<&MergeTreeEntry>,
    side: Option<&MergeTreeEntry>,
) -> RelativeState {
    match (base, side) {
        (Some(base), Some(side)) if base == side => RelativeState::Same(*side),
        (Some(_), Some(side)) => RelativeState::Modified(*side),
        (Some(_), None) => RelativeState::Deleted,
        (None, Some(side)) => RelativeState::Added(*side),
        (None, None) => RelativeState::Missing,
    }
}

pub(crate) fn resolve_three_way(
    path: &Path,
    base: Option<&MergeTreeEntry>,
    ours: Option<&MergeTreeEntry>,
    theirs: Option<&MergeTreeEntry>,
    context: &mut TreeMergeContext<'_>,
) -> Result<MergeResolution, PullMergeError> {
    let favor = context.favor;
    let base_present = base.is_some();
    let ours_state = classify_relative_to_base(base, ours);
    let theirs_state = classify_relative_to_base(base, theirs);

    Ok(match (base_present, ours_state, theirs_state) {
        (false, RelativeState::Missing, RelativeState::Missing) => MergeResolution::Delete,
        (false, RelativeState::Added(ours), RelativeState::Missing) => MergeResolution::Use(ours),
        (false, RelativeState::Missing, RelativeState::Added(theirs)) => {
            MergeResolution::Use(theirs)
        }
        (false, RelativeState::Added(ours), RelativeState::Added(theirs)) => {
            if ours == theirs {
                MergeResolution::Use(theirs)
            } else {
                let driver = context.driver_for_path(path);
                if driver.is_builtin(BuiltinMergeDriver::Text)
                    && let Some(favor) = favor
                {
                    // Preserve the long-standing add/add `-X` shortcut for
                    // the built-in text driver: it chooses one complete side
                    // without loading either blob. External drivers still run
                    // and own `%A`, regardless of the strategy option.
                    favored_resolution(favor, Some(ours), Some(theirs))
                } else {
                    match try_merge_blob_contents(path, None, ours, theirs, driver, context)? {
                        BlobMergeAttempt::Clean(merged) => MergeResolution::Use(merged),
                        BlobMergeAttempt::Conflict { driver, rendered } => {
                            MergeResolution::Conflict(ConflictKind::BothChanged {
                                base: None,
                                ours: ours.hash,
                                theirs: theirs.hash,
                                driver,
                                rendered,
                            })
                        }
                        BlobMergeAttempt::NotApplicable if favor.is_some() => favored_resolution(
                            favor.unwrap_or(MergeFavor::Ours),
                            Some(ours),
                            Some(theirs),
                        ),
                        BlobMergeAttempt::NotApplicable => {
                            MergeResolution::Conflict(ConflictKind::BothChanged {
                                base: None,
                                ours: ours.hash,
                                theirs: theirs.hash,
                                driver: BuiltinMergeDriver::Text,
                                rendered: None,
                            })
                        }
                    }
                }
            }
        }
        (true, RelativeState::Same(ours), RelativeState::Same(_)) => MergeResolution::Use(ours),
        (true, RelativeState::Same(_), RelativeState::Modified(theirs)) => {
            MergeResolution::Use(theirs)
        }
        (true, RelativeState::Modified(ours), RelativeState::Same(_)) => MergeResolution::Use(ours),
        (true, RelativeState::Modified(ours), RelativeState::Modified(theirs)) => {
            if ours == theirs {
                MergeResolution::Use(theirs)
            } else {
                let driver = context.driver_for_path(path);
                match try_merge_blob_contents(path, base, ours, theirs, driver, context)? {
                    BlobMergeAttempt::Clean(merged) => MergeResolution::Use(merged),
                    BlobMergeAttempt::Conflict { driver, rendered } => {
                        MergeResolution::Conflict(ConflictKind::BothChanged {
                            base: base.map(|b| b.hash),
                            ours: ours.hash,
                            theirs: theirs.hash,
                            driver,
                            rendered,
                        })
                    }
                    BlobMergeAttempt::NotApplicable if favor.is_some() => favored_resolution(
                        favor.unwrap_or(MergeFavor::Ours),
                        Some(ours),
                        Some(theirs),
                    ),
                    BlobMergeAttempt::NotApplicable => {
                        MergeResolution::Conflict(ConflictKind::BothChanged {
                            base: base.map(|b| b.hash),
                            ours: ours.hash,
                            theirs: theirs.hash,
                            driver: BuiltinMergeDriver::Text,
                            rendered: None,
                        })
                    }
                }
            }
        }
        (true, RelativeState::Deleted, RelativeState::Same(_)) => MergeResolution::Delete,
        (true, RelativeState::Same(_), RelativeState::Deleted) => MergeResolution::Delete,
        (true, RelativeState::Deleted, RelativeState::Deleted) => MergeResolution::Delete,
        // A modify/delete is NOT a content conflict, so `-X ours` / `-X theirs`
        // does not settle it — it stays a conflict, exactly as Git leaves it.
        // FIX-MG05-01 (pre-existing, reproduced on the released v0.22.15
        // binary): applying the strategy option here resolved the pair in
        // favour of the DELETION, so the other side's edit was destroyed by a
        // merge that exited 0 and recorded nothing. Measured on git 2.50.1,
        // both directions and both options: `git merge -X ours` and
        // `-X theirs` over `f.txt` deleted on one side and modified on the
        // other print `CONFLICT (modify/delete)` and keep the modified content
        // at stages 1 and 2/3. The user docs already promised this ("a strategy
        // option settles content hunks only").
        (true, RelativeState::Deleted, RelativeState::Modified(theirs)) => {
            MergeResolution::Conflict(ConflictKind::TheirsModifiedOursDeleted {
                theirs: theirs.hash,
            })
        }
        (true, RelativeState::Modified(ours), RelativeState::Deleted) => {
            MergeResolution::Conflict(ConflictKind::OursModifiedTheirsDeleted { ours: ours.hash })
        }
        _ => MergeResolution::Delete,
    })
}

pub(crate) fn resolve_favored_content(
    conflicted: Vec<u8>,
    marker_len: usize,
    favor: MergeFavor,
) -> Result<Vec<u8>, String> {
    resolve_conflicted_content(conflicted, marker_len, ConflictResolution::Favor(favor))
}

pub(crate) fn resolve_conflicted_content(
    conflicted: Vec<u8>,
    marker_len: usize,
    resolution: ConflictResolution,
) -> Result<Vec<u8>, String> {
    let marker = |byte: u8, label: Option<&[u8]>| {
        let mut line = vec![byte; marker_len];
        if let Some(label) = label {
            line.push(b' ');
            line.extend_from_slice(label);
        }
        line.push(b'\n');
        line
    };
    let open = marker(b'<', Some(b"ours"));
    let original = marker(b'|', Some(b"original"));
    let separator = marker(b'=', None);
    let close = marker(b'>', Some(b"theirs"));

    let find_after = |haystack: &[u8], start: usize, needle: &[u8]| {
        haystack
            .get(start..)
            .and_then(|tail| {
                tail.windows(needle.len())
                    .position(|window| window == needle)
            })
            .map(|relative| start + relative)
    };
    let malformed = || "internal three-way merge produced malformed conflict markers".to_string();

    let mut output = Vec::with_capacity(conflicted.len());
    let mut cursor = 0usize;
    let mut resolved = 0usize;
    while let Some(open_start) = find_after(&conflicted, cursor, &open) {
        output.extend_from_slice(&conflicted[cursor..open_start]);
        let ours_start = open_start + open.len();
        let original_start =
            find_after(&conflicted, ours_start, &original).ok_or_else(malformed)?;
        let base_start = original_start + original.len();
        let separator_start =
            find_after(&conflicted, base_start, &separator).ok_or_else(malformed)?;
        let theirs_start = separator_start + separator.len();
        let close_start = find_after(&conflicted, theirs_start, &close).ok_or_else(malformed)?;
        match resolution {
            ConflictResolution::Favor(MergeFavor::Ours) => {
                output.extend_from_slice(&conflicted[ours_start..original_start]);
            }
            ConflictResolution::Favor(MergeFavor::Theirs) => {
                output.extend_from_slice(&conflicted[theirs_start..close_start]);
            }
            ConflictResolution::Union => {
                output.extend_from_slice(&conflicted[ours_start..original_start]);
                output.extend_from_slice(&conflicted[theirs_start..close_start]);
            }
        }
        cursor = close_start + close.len();
        resolved += 1;
    }
    if resolved == 0 {
        return Err(malformed());
    }
    output.extend_from_slice(&conflicted[cursor..]);
    Ok(output)
}

pub(crate) fn conflict_placements(
    conflicts: &[(PathBuf, ConflictKind)],
    occupied: &HashSet<PathBuf>,
    upstream: &str,
) -> Vec<(PathBuf, ConflictKind, Option<PathBuf>)> {
    let mut taken = occupied.clone();
    let mut placed = Vec::with_capacity(conflicts.len());
    for (path, kind) in conflicts {
        match df_file_side(kind) {
            Some(file_side) => {
                let target = unique_df_path(path, &df_branch_label(file_side, upstream), &taken);
                taken.insert(target.clone());
                placed.push((target, *kind, Some(path.clone())));
            }
            None => placed.push((path.clone(), *kind, None)),
        }
    }
    placed
}

pub(crate) fn conflict_kind_name(kind: &ConflictKind) -> &'static str {
    match kind {
        ConflictKind::BothChanged { .. } => "content",
        ConflictKind::OursModifiedTheirsDeleted { .. }
        | ConflictKind::TheirsModifiedOursDeleted { .. } => "modify-delete",
        ConflictKind::FileDirectory {
            modify_delete: true,
            ..
        } => "modify-delete",
        ConflictKind::FileDirectory { .. } => "file-directory",
        // A 1to2 destination with another add is a content collision as well
        // as a path conflict; pre-rendering must not hide that public kind.
        ConflictKind::RenameMerged { kind, .. } => match kind {
            RenameConflictKind::RenameRename => "rename-rename",
            RenameConflictKind::Content => "content",
            RenameConflictKind::DirectoryRename => "directory-rename",
        },
        ConflictKind::DirectorySplit { .. } => "directory-rename",
    }
}

pub(crate) fn conflict_payload(content: &[u8]) -> Cow<'_, str> {
    match std::str::from_utf8(content) {
        Ok(text) => Cow::Borrowed(text),
        Err(_) => Cow::Owned(format!("[binary content, {} bytes]", content.len())),
    }
}

pub(crate) fn write_conflict_markers(
    workdir: &Path,
    path: &Path,
    labels: &GitConflictLabels,
    kind: ConflictKind,
    conflict_style: ConflictStyle,
) -> Result<(), String> {
    let content: Vec<u8> = match kind {
        ConflictKind::BothChanged {
            base,
            ours,
            theirs,
            driver,
            rendered,
        } => {
            if let Some(rendered) = rendered {
                return load_object::<Blob>(&rendered.hash)
                    .map(|blob| blob.data)
                    .map_err(|error| error.to_string())
                    .and_then(|content| {
                        write_workdir_file_with_mode(
                            workdir,
                            path,
                            &content,
                            worktree_conflict_executable(path),
                        )
                    });
            }
            let ours_blob: Blob = load_object(&ours).map_err(|error| error.to_string())?;
            let theirs_blob: Blob = load_object(&theirs).map_err(|error| error.to_string())?;
            match driver {
                BuiltinMergeDriver::Binary => ours_blob.data,
                BuiltinMergeDriver::Union => {
                    let base_data = match base {
                        Some(base) => {
                            load_object::<Blob>(&base)
                                .map_err(|error| error.to_string())?
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
                    )? {
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
                )?,
            }
        }
        ConflictKind::OursModifiedTheirsDeleted { ours } => {
            let ours_blob: Blob = load_object(&ours).map_err(|error| error.to_string())?;
            let ours = conflict_payload(&ours_blob.data);
            render_whole_file_conflict(
                ours.as_bytes(),
                &[],
                GitConflictLabels::OURS,
                &format!("{} (deleted)", labels.theirs),
            )
        }
        ConflictKind::TheirsModifiedOursDeleted { theirs } => {
            let theirs_blob: Blob = load_object(&theirs).map_err(|error| error.to_string())?;
            let theirs = conflict_payload(&theirs_blob.data);
            render_whole_file_conflict(
                &[],
                theirs.as_bytes(),
                &format!("{} (deleted)", GitConflictLabels::OURS),
                &labels.theirs,
            )
        }
        // The directory kept the original path; `path` here is already the
        // file's `unique_path`, and its content is written verbatim — Git
        // moves the file, it does not mark it up.
        ConflictKind::FileDirectory { file, .. } => {
            let blob: Blob = load_object(&file.hash).map_err(|error| error.to_string())?;
            blob.data
        }
        // MG-06: already the merged result Git recorded for the rename
        // (`merge-ort.c:3021-3053`), including its conflict markers when the
        // content merge was not clean — written verbatim, never marked up
        // twice.
        ConflictKind::RenameMerged { content, .. } => {
            let blob: Blob = load_object(&content.hash).map_err(|error| error.to_string())?;
            return write_workdir_entry(workdir, path, content.mode, &blob.data);
        }
        ConflictKind::DirectorySplit { content } => {
            let blob: Blob = load_object(&content.hash).map_err(|error| error.to_string())?;
            return write_workdir_entry(workdir, path, content.mode, &blob.data);
        }
    };
    write_workdir_file_with_mode(workdir, path, &content, worktree_conflict_executable(path))
}

pub(crate) fn both_changed_conflict_content(
    base: Option<ObjectHash>,
    ours: &[u8],
    theirs: &[u8],
    labels: &GitConflictLabels,
    conflict_style: ConflictStyle,
) -> Result<Vec<u8>, String> {
    let whole_file = || {
        let ours = conflict_payload(ours);
        let theirs = conflict_payload(theirs);
        render_whole_file_conflict(
            ours.as_bytes(),
            theirs.as_bytes(),
            &labels.ours,
            &labels.theirs,
        )
    };

    // Load the common-ancestor content (if any) and defer to the shared
    // line-level renderer; fall back to whole-file markers for binary sides.
    let base_data: Option<Vec<u8>> = match base {
        Some(base) => {
            let base_blob: Blob = load_object(&base).map_err(|error| error.to_string())?;
            Some(base_blob.data)
        }
        None => None,
    };
    Ok(render_line_level_conflict_labeled(
        base_data.as_deref(),
        ours,
        theirs,
        labels,
        conflict_style,
    )?
    .unwrap_or_else(whole_file))
}

#[allow(dead_code)]
pub(crate) fn render_line_level_conflict(
    base: Option<&[u8]>,
    ours: &[u8],
    theirs: &[u8],
    commit_label: &str,
    conflict_style: ConflictStyle,
) -> Result<Option<Vec<u8>>, String> {
    render_line_level_conflict_labeled(
        base,
        ours,
        theirs,
        &GitConflictLabels {
            ours: GitConflictLabels::OURS.to_string(),
            base: "base".to_string(),
            theirs: commit_label.to_string(),
        },
        conflict_style,
    )
}

pub(crate) fn render_line_level_conflict_labeled(
    base: Option<&[u8]>,
    ours: &[u8],
    theirs: &[u8],
    labels: &GitConflictLabels,
    conflict_style: ConflictStyle,
) -> Result<Option<Vec<u8>>, String> {
    if std::str::from_utf8(ours).is_err()
        || std::str::from_utf8(theirs).is_err()
        || base.is_some_and(|b| std::str::from_utf8(b).is_err())
    {
        return Ok(None);
    }

    // Choose a marker length long enough that no line in the inputs can be
    // mistaken for (and then wrongly relabelled as) a generated marker — Git's
    // conflict-marker-size bumping. With this length the relabel below matches
    // only `diffy`'s emitted markers.
    let marker_len = conflict_marker_length(&[base.unwrap_or(&[]), ours, theirs]);
    let mut options = diffy::MergeOptions::new();
    options.set_conflict_style(conflict_style.diffy_style());
    options.set_conflict_marker_length(marker_len);
    match options.merge_bytes(base.unwrap_or(&[]), ours, theirs) {
        // A genuine conflict: refine diffy's block and relabel it in one
        // byte-preserving pass so the shared merge/cherry-pick/revert renderer
        // also controls zdiff3 and marker line endings.
        Err(conflicted) => refine_diffy_conflicts(
            &conflicted,
            marker_len,
            conflict_style,
            base.unwrap_or(&[]),
            ours,
            theirs,
            labels.as_marker_labels(),
        )
        .map(|(rendered, has_conflicts)| has_conflicts.then_some(rendered)),
        // Content merged cleanly with no markers (no real text conflict — e.g. a
        // mode-only divergence): let the caller surface it as a whole-file
        // conflict rather than writing the silently-merged text.
        Ok(_) => Ok(None),
    }
}

pub(crate) fn conflict_marker_length(sides: &[&[u8]]) -> usize {
    const DEFAULT_MARKER_LENGTH: usize = 7;
    let mut longest = 0usize;
    for side in sides {
        for line in side.split(|&b| b == b'\n') {
            let Some(&first) = line.first() else { continue };
            if matches!(first, b'<' | b'>' | b'=' | b'|') {
                let run = line.iter().take_while(|&&b| b == first).count();
                if run < DEFAULT_MARKER_LENGTH {
                    continue;
                }
                let label = line
                    .get(run + 1..)
                    .map(|rest| rest.strip_suffix(b"\r").unwrap_or(rest))
                    .unwrap_or_default();
                // Nested conflict labels (`HEAD:path`, `base:path`, …) are
                // content being wrapped, not a reason to widen the outer
                // markers. Virtual-ancestor labels stay distinguishable and
                // therefore still widen one level.
                if label.starts_with(b"Temporary merge branch") {
                    longest = longest.max(run);
                }
            }
        }
    }
    if longest >= DEFAULT_MARKER_LENGTH {
        longest + 1
    } else {
        DEFAULT_MARKER_LENGTH
    }
}

pub(crate) fn relabel_conflict_markers(
    conflicted: Vec<u8>,
    marker_len: usize,
    ours_label: &str,
    theirs_label: &str,
    // The `|||||||` label under `diff3`. Git names the merge base
    // `<ancestor>` when all three paths are the same and `<ancestor>:<path>`
    // when they are not (`merge-ort.c:2147-2155`), so a rename-driven merge
    // labels it with the SOURCE path (Codex R1 P2-2).
    base_label: &str,
) -> Vec<u8> {
    let open = "<".repeat(marker_len);
    let close = ">".repeat(marker_len);
    let bars = "|".repeat(marker_len);
    let ours_marker = format!("{open} ours");
    let theirs_marker = format!("{close} theirs");
    // `diffy`'s base marker; emitted for Diff3 and ZDiff3 raw blocks.
    let original_marker = format!("{bars} original");
    let head_marker = format!("{open} {ours_label}");
    let label_marker = format!("{close} {theirs_label}");
    // Match the `||||||| base` label convention `restore --conflict=diff3` uses.
    let base_marker = format!("{bars} {base_label}");

    // Byte-wise, never through `String::from_utf8_lossy`: the recursive
    // virtual-ancestor fold relabels content that Git's binary rule considers
    // TEXT (no NUL byte) but that need not be valid UTF-8, and a lossy
    // conversion would rewrite those bytes as U+FFFD.
    //
    // `split(b'\n')` + rejoining round-trips exactly, including a trailing
    // newline (which yields a final empty segment that re-joins cleanly).
    let mut relabelled = Vec::with_capacity(conflicted.len());
    for (index, line) in conflicted.split(|byte| *byte == b'\n').enumerate() {
        if index > 0 {
            relabelled.push(b'\n');
        }
        let (body, had_cr) = match line.strip_suffix(b"\r") {
            Some(body) => (body, true),
            None => (line, false),
        };
        let replacement = if body == ours_marker.as_bytes() {
            head_marker.as_bytes()
        } else if body == theirs_marker.as_bytes() {
            label_marker.as_bytes()
        } else if body == original_marker.as_bytes() {
            base_marker.as_bytes()
        } else {
            body
        };
        relabelled.extend_from_slice(replacement);
        if had_cr {
            relabelled.push(b'\r');
        }
    }
    relabelled
}
