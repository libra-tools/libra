//! Merge content: blob bytes -> conflict-marker rendering, diff3/zdiff3/refined
//! text merge and the shared blob-loading/renormalization helpers.
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

use self::workdir::*;
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

pub(crate) fn text_renormalization_for_path(path: &Path) -> TextRenormalization {
    let text = attributes::attribute_state_for_path("text", path);
    let eol_implies_text = matches!(
        attributes::attribute_state_for_path("eol", path),
        Some(AttributeState::Value(value)) if value.eq_ignore_ascii_case("lf")
            || value.eq_ignore_ascii_case("crlf")
    );
    match text {
        Some(AttributeState::Unset) => TextRenormalization::Never,
        Some(AttributeState::Value(value)) if value.eq_ignore_ascii_case("auto") => {
            TextRenormalization::Auto
        }
        Some(AttributeState::Set | AttributeState::Value(_)) => TextRenormalization::Always,
        Some(AttributeState::Unspecified) | None if eol_implies_text => TextRenormalization::Always,
        Some(AttributeState::Unspecified) | None => TextRenormalization::Never,
    }
}

pub(crate) fn renormalize_text_input(mode: TextRenormalization, content: &[u8]) -> bool {
    match mode {
        TextRenormalization::Never => false,
        TextRenormalization::Always => true,
        TextRenormalization::Auto => !text_auto_is_binary(content),
    }
}

pub(crate) fn text_auto_is_binary(content: &[u8]) -> bool {
    let mut printable = 0usize;
    let mut nonprintable = 0usize;
    let mut has_nul = false;
    let mut has_lone_cr = false;
    let mut index = 0usize;
    while index < content.len() {
        let byte = content[index];
        if byte == b'\r' {
            if content.get(index + 1) == Some(&b'\n') {
                index += 2;
                continue;
            }
            has_lone_cr = true;
        } else if byte == b'\n' {
            index += 1;
            continue;
        } else if byte == 127 {
            nonprintable += 1;
        } else if byte < 32 {
            if matches!(byte, b'\x08' | b'\t' | b'\x1b' | b'\x0c') {
                printable += 1;
            } else {
                has_nul |= byte == 0;
                nonprintable += 1;
            }
        } else {
            printable += 1;
        }
        index += 1;
    }
    if content.last() == Some(&b'\x1a') {
        nonprintable = nonprintable.saturating_sub(1);
    }
    has_lone_cr || has_nul || (printable >> 7) < nonprintable
}

pub(crate) fn split_original_line(record: &[u8]) -> (&[u8], &[u8]) {
    if let Some(body) = record.strip_suffix(b"\r\n") {
        (body, b"\r\n")
    } else if let Some(body) = record.strip_suffix(b"\n") {
        (body, b"\n")
    } else {
        (record, b"")
    }
}

pub(crate) fn normalize_merge_input(
    content: &[u8],
    whitespace: Option<MergeWhitespace>,
    renormalize: bool,
) -> NormalizedMergeInput {
    let mut canonical = Vec::with_capacity(content.len());
    let mut lines = Vec::new();
    for record in split_lines_preserving_eol(content) {
        let (original_body, original_eol) = split_original_line(record);
        // Every diff whitespace normalizer consumes logical lines (without the
        // CRLF terminator). Without a whitespace option, only renormalization
        // strips the CR from CRLF for comparison.
        let comparison_body = if (whitespace.is_some() || renormalize) && original_eol == b"\r\n" {
            original_body
        } else if original_eol == b"\r\n" {
            // Preserve the CR when neither comparison mode is active. This
            // branch is kept for the unit-level identity contract.
            &record[..record.len() - 1]
        } else {
            original_body
        };
        let key = match (whitespace, std::str::from_utf8(comparison_body)) {
            (Some(mode), Ok(text)) => mode.normalizer()(text).into_bytes(),
            // Whitespace comparison must never collapse distinct invalid
            // UTF-8 sequences through the replacement character. Keep those
            // bytes exact; the ordinary byte merge can still handle them.
            (Some(_), Err(_)) => comparison_body.to_vec(),
            (None, _) => comparison_body.to_vec(),
        };
        canonical.extend_from_slice(&key);
        if !original_eol.is_empty() {
            canonical.push(b'\n');
        }
        lines.push(NormalizedMergeLine {
            key,
            original_body: original_body.to_vec(),
            original_eol: original_eol.to_vec(),
        });
    }
    NormalizedMergeInput { canonical, lines }
}

pub(crate) fn normalized_marker(
    body: &[u8],
    marker: u8,
    marker_len: usize,
    label: Option<&[u8]>,
) -> bool {
    if body.len() < marker_len || body[..marker_len].iter().any(|byte| *byte != marker) {
        return false;
    }
    match label {
        Some(label) => {
            body.len() == marker_len + 1 + label.len()
                && body[marker_len] == b' '
                && body[marker_len + 1..] == *label
        }
        None => body.len() == marker_len,
    }
}

pub(crate) fn find_normalized_line(
    input: &NormalizedMergeInput,
    cursor: usize,
    key: &[u8],
) -> Option<usize> {
    input.lines[cursor..]
        .iter()
        .position(|line| line.key == key)
        .map(|offset| cursor + offset)
}

pub(crate) fn backfill_normalized_merge(
    rendered: &[u8],
    marker_len: usize,
    base: &NormalizedMergeInput,
    ours: &NormalizedMergeInput,
    theirs: &NormalizedMergeInput,
    original_ours: &[u8],
) -> Vec<u8> {
    let inputs = [base, ours, theirs];
    let mut cursors = [0usize; 3];
    let mut section = NormalizedOutputSection::Merged;
    let preferred_eol = conflict_marker_eol_for_inputs(&[original_ours]);
    let mut output = Vec::with_capacity(rendered.len());
    for record in split_lines_preserving_eol(rendered) {
        let (body, output_eol) = split_original_line(record);
        let marker = if normalized_marker(body, b'<', marker_len, Some(b"ours")) {
            section = NormalizedOutputSection::Ours;
            true
        } else if normalized_marker(body, b'|', marker_len, Some(b"original")) {
            section = NormalizedOutputSection::Base;
            true
        } else if normalized_marker(body, b'=', marker_len, None) {
            section = NormalizedOutputSection::Theirs;
            true
        } else if normalized_marker(body, b'>', marker_len, Some(b"theirs")) {
            section = NormalizedOutputSection::Merged;
            true
        } else {
            false
        };
        if marker {
            output.extend_from_slice(body);
            if !output_eol.is_empty() {
                output.extend_from_slice(preferred_eol);
            }
            continue;
        }

        let candidates: &[usize] = match section {
            NormalizedOutputSection::Merged => &[1, 2, 0],
            NormalizedOutputSection::Ours => &[1],
            NormalizedOutputSection::Base => &[0],
            NormalizedOutputSection::Theirs => &[2],
        };
        let selected = candidates
            .iter()
            .filter_map(|side| {
                find_normalized_line(inputs[*side], cursors[*side], body)
                    .map(|index| (*side, index, index - cursors[*side]))
            })
            .min_by_key(|(side, _, distance)| {
                (*distance, candidates.iter().position(|s| s == side))
            });
        if let Some((side, index, _)) = selected {
            let line = &inputs[side].lines[index];
            output.extend_from_slice(&line.original_body);
            cursors[side] = index + 1;
            if section == NormalizedOutputSection::Merged {
                // A merged/context line is normally present in every input.
                // Advancing matching cursors keeps repeated normalized lines
                // aligned without consuming side-specific conflict bodies.
                for other in 0..inputs.len() {
                    if other == side {
                        continue;
                    }
                    if let Some(other_index) =
                        find_normalized_line(inputs[other], cursors[other], body)
                    {
                        cursors[other] = other_index + 1;
                    }
                }
            }
            if !output_eol.is_empty() {
                if side == 1 && !line.original_eol.is_empty() {
                    output.extend_from_slice(&line.original_eol);
                } else {
                    output.extend_from_slice(preferred_eol);
                }
            }
        } else {
            output.extend_from_slice(body);
            if !output_eol.is_empty() {
                output.extend_from_slice(preferred_eol);
            }
        }
    }
    output
}

pub(crate) fn merge_bytes_with_input_normalization(
    driver: BuiltinMergeDriver,
    path: &Path,
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    options: MergeContentOptions,
) -> Result<BuiltinMergeOutcome, String> {
    let MergeContentOptions {
        favor,
        conflict_style,
        extra_marker_size,
        normalization,
    } = options;
    let text_mode = if normalization.renormalize {
        text_renormalization_for_path(path)
    } else {
        TextRenormalization::Never
    };
    let renormalize_base = renormalize_text_input(text_mode, base);
    let renormalize_ours = renormalize_text_input(text_mode, ours);
    let renormalize_theirs = renormalize_text_input(text_mode, theirs);
    let active = normalization.whitespace.is_some()
        || renormalize_base
        || renormalize_ours
        || renormalize_theirs;
    if !active
        || driver == BuiltinMergeDriver::Binary
        || [base, ours, theirs]
            .iter()
            .any(|input| merge_input_is_binary(input))
    {
        return merge_bytes_with_refined_driver(
            driver,
            base,
            ours,
            theirs,
            favor,
            conflict_style,
            extra_marker_size,
        );
    }

    let normalized_base = normalize_merge_input(base, normalization.whitespace, renormalize_base);
    let normalized_ours = normalize_merge_input(ours, normalization.whitespace, renormalize_ours);
    let normalized_theirs =
        normalize_merge_input(theirs, normalization.whitespace, renormalize_theirs);
    let marker_len = super::conflict::conflict_marker_length(&[
        &normalized_base.canonical,
        &normalized_ours.canonical,
        &normalized_theirs.canonical,
    ])
    .saturating_add(extra_marker_size);
    let outcome = merge_bytes_with_refined_driver(
        driver,
        &normalized_base.canonical,
        &normalized_ours.canonical,
        &normalized_theirs.canonical,
        favor,
        conflict_style,
        extra_marker_size,
    )?;
    let backfill = |bytes: Vec<u8>| {
        backfill_normalized_merge(
            &bytes,
            marker_len,
            &normalized_base,
            &normalized_ours,
            &normalized_theirs,
            ours,
        )
    };
    Ok(match outcome {
        BuiltinMergeOutcome::Clean(bytes) => BuiltinMergeOutcome::Clean(backfill(bytes)),
        BuiltinMergeOutcome::Conflict(bytes) => BuiltinMergeOutcome::Conflict(backfill(bytes)),
    })
}

#[allow(dead_code)] // retained as the label-defaulting wrapper for callers
pub(crate) fn merge_bytes_with_driver(
    driver: BuiltinMergeDriver,
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    favor: Option<MergeFavor>,
    conflict_style: diffy::ConflictStyle,
    extra_marker_size: usize,
) -> Result<BuiltinMergeOutcome, String> {
    merge_bytes_with_driver_impl(
        driver,
        base,
        ours,
        theirs,
        favor,
        conflict_style.into(),
        extra_marker_size,
        false,
        ConflictMarkerLabels {
            ours: "ours",
            base: "original",
            theirs: "theirs",
        },
    )
}

pub(crate) fn merge_bytes_with_refined_driver(
    driver: BuiltinMergeDriver,
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    favor: Option<MergeFavor>,
    conflict_style: ConflictStyle,
    extra_marker_size: usize,
) -> Result<BuiltinMergeOutcome, String> {
    merge_bytes_with_refined_driver_labeled(
        driver,
        base,
        ours,
        theirs,
        favor,
        conflict_style,
        extra_marker_size,
        ConflictMarkerLabels {
            ours: "ours",
            base: "original",
            theirs: "theirs",
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn merge_bytes_with_refined_driver_labeled(
    driver: BuiltinMergeDriver,
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    favor: Option<MergeFavor>,
    conflict_style: ConflictStyle,
    extra_marker_size: usize,
    labels: ConflictMarkerLabels<'_>,
) -> Result<BuiltinMergeOutcome, String> {
    merge_bytes_with_driver_impl(
        driver,
        base,
        ours,
        theirs,
        favor,
        conflict_style,
        extra_marker_size,
        true,
        labels,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn merge_bytes_with_driver_impl(
    driver: BuiltinMergeDriver,
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    favor: Option<MergeFavor>,
    conflict_style: ConflictStyle,
    extra_marker_size: usize,
    refine: bool,
    labels: ConflictMarkerLabels<'_>,
) -> Result<BuiltinMergeOutcome, String> {
    // High-level merge consumers normally settle these OID-equality cases
    // before low-level dispatch. Keep the shared helper equally safe for
    // byte-oriented consumers such as merge-file.
    if ours == theirs {
        return Ok(BuiltinMergeOutcome::Clean(ours.to_vec()));
    }
    if ours == base {
        return Ok(BuiltinMergeOutcome::Clean(theirs.to_vec()));
    }
    if theirs == base {
        return Ok(BuiltinMergeOutcome::Clean(ours.to_vec()));
    }
    let binary_fallback = driver == BuiltinMergeDriver::Binary
        || (driver == BuiltinMergeDriver::Union
            && (merge_input_is_binary(base)
                || merge_input_is_binary(ours)
                || merge_input_is_binary(theirs)));
    if binary_fallback {
        // Git's union driver sets the union variant itself; if xdiff rejects
        // binary input, that variant is not ours/theirs and the binary fallback
        // therefore reports a conflict with ours.
        let effective_favor = if driver == BuiltinMergeDriver::Union {
            None
        } else {
            favor
        };
        return Ok(match effective_favor {
            None => BuiltinMergeOutcome::Conflict(ours.to_vec()),
            Some(MergeFavor::Ours) => BuiltinMergeOutcome::Clean(ours.to_vec()),
            Some(MergeFavor::Theirs) => BuiltinMergeOutcome::Clean(theirs.to_vec()),
        });
    }

    let marker_len = super::conflict::conflict_marker_length(&[base, ours, theirs])
        .saturating_add(extra_marker_size);
    let mut options = diffy::MergeOptions::new();
    options
        .set_conflict_style(if favor.is_some() || driver == BuiltinMergeDriver::Union {
            diffy::ConflictStyle::Diff3
        } else {
            conflict_style.diffy_style()
        })
        .set_conflict_marker_length(marker_len);
    match options.merge_bytes(base, ours, theirs) {
        Ok(bytes) => Ok(BuiltinMergeOutcome::Clean(bytes)),
        Err(conflicted) => match driver {
            BuiltinMergeDriver::Union => {
                resolve_conflicted_content(conflicted, marker_len, ConflictResolution::Union)
                    .map(BuiltinMergeOutcome::Clean)
            }
            BuiltinMergeDriver::Text => match favor {
                Some(favor) => resolve_favored_content(conflicted, marker_len, favor)
                    .map(BuiltinMergeOutcome::Clean),
                None if refine => {
                    let (rendered, has_conflicts) = refine_diffy_conflicts(
                        &conflicted,
                        marker_len,
                        conflict_style,
                        base,
                        ours,
                        theirs,
                        labels,
                    )?;
                    Ok(if has_conflicts {
                        BuiltinMergeOutcome::Conflict(rendered)
                    } else {
                        BuiltinMergeOutcome::Clean(rendered)
                    })
                }
                None => Ok(BuiltinMergeOutcome::Conflict(conflicted)),
            },
            BuiltinMergeDriver::Binary => {
                Err("internal binary merge driver unexpectedly reached the text merger".to_string())
            }
        },
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ConflictMarkerLabels<'a> {
    pub ours: &'a str,
    pub base: &'a str,
    pub theirs: &'a str,
}

pub(crate) fn marker_bytes(
    byte: u8,
    marker_len: usize,
    label: Option<&str>,
    eol: &[u8],
) -> Vec<u8> {
    let mut marker = vec![byte; marker_len];
    if let Some(label) = label {
        marker.push(b' ');
        marker.extend_from_slice(label.as_bytes());
    }
    marker.extend_from_slice(eol);
    marker
}

pub(crate) fn input_uses_crlf_only(content: &[u8]) -> Option<bool> {
    let mut saw_crlf = false;
    for (index, byte) in content.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        if index == 0 || content[index - 1] != b'\r' {
            return Some(false);
        }
        saw_crlf = true;
    }
    saw_crlf.then_some(true)
}

pub(crate) fn conflict_marker_eol_for_inputs(inputs: &[&[u8]]) -> &'static [u8] {
    let mut saw_crlf = false;
    for input in inputs {
        match input_uses_crlf_only(input) {
            Some(true) => saw_crlf = true,
            Some(false) => return b"\n",
            None => {}
        }
    }
    if saw_crlf { b"\r\n" } else { b"\n" }
}

pub(crate) fn split_lines_preserving_eol(content: &[u8]) -> Vec<&[u8]> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (index, byte) in content.iter().enumerate() {
        if *byte == b'\n' {
            lines.push(&content[start..=index]);
            start = index + 1;
        }
    }
    if start < content.len() {
        lines.push(&content[start..]);
    }
    lines
}

pub(crate) fn append_lines(output: &mut Vec<u8>, lines: &[&[u8]]) {
    for line in lines {
        output.extend_from_slice(line);
    }
}

pub(crate) fn append_marker(
    output: &mut Vec<u8>,
    byte: u8,
    marker_len: usize,
    label: Option<&str>,
    eol: &[u8],
) {
    output.extend_from_slice(&marker_bytes(byte, marker_len, label, eol));
}

pub(crate) fn ensure_conflict_side_ends_with_eol(output: &mut Vec<u8>, side: &[&[u8]], eol: &[u8]) {
    if side.last().is_some_and(|line| !line.ends_with(b"\n")) {
        output.extend_from_slice(eol);
    }
}

pub(crate) fn append_conflict_block(
    output: &mut Vec<u8>,
    ours: &[&[u8]],
    base: Option<&[&[u8]]>,
    theirs: &[&[u8]],
    marker_len: usize,
    eol: &[u8],
    labels: ConflictMarkerLabels<'_>,
) {
    append_marker(output, b'<', marker_len, Some(labels.ours), eol);
    append_lines(output, ours);
    ensure_conflict_side_ends_with_eol(output, ours, eol);
    if let Some(base) = base {
        append_marker(output, b'|', marker_len, Some(labels.base), eol);
        append_lines(output, base);
        ensure_conflict_side_ends_with_eol(output, base, eol);
    }
    append_marker(output, b'=', marker_len, None, eol);
    append_lines(output, theirs);
    ensure_conflict_side_ends_with_eol(output, theirs, eol);
    append_marker(output, b'>', marker_len, Some(labels.theirs), eol);
}

pub(crate) fn render_whole_file_conflict(
    ours: &[u8],
    theirs: &[u8],
    ours_label: &str,
    theirs_label: &str,
) -> Vec<u8> {
    let ours_lines = split_lines_preserving_eol(ours);
    let theirs_lines = split_lines_preserving_eol(theirs);
    let eol = conflict_marker_eol_for_inputs(&[ours, theirs]);
    let marker_len = conflict_marker_length(&[ours, theirs]);
    let mut output = Vec::with_capacity(ours.len() + theirs.len() + 64);
    append_conflict_block(
        &mut output,
        &ours_lines,
        None,
        &theirs_lines,
        marker_len,
        eol,
        ConflictMarkerLabels {
            ours: ours_label,
            base: "base",
            theirs: theirs_label,
        },
    );
    output
}

pub(crate) fn append_refined_merge_block(
    output: &mut Vec<u8>,
    ours: &[u8],
    theirs: &[u8],
    marker_len: usize,
    eol: &[u8],
    labels: ConflictMarkerLabels<'_>,
) -> bool {
    let ours_lines = split_lines_preserving_eol(ours);
    let theirs_lines = split_lines_preserving_eol(theirs);
    if ours_lines.is_empty() || theirs_lines.is_empty() {
        append_conflict_block(
            output,
            &ours_lines,
            None,
            &theirs_lines,
            marker_len,
            eol,
            labels,
        );
        return true;
    }

    let operations =
        similar::capture_diff_slices(similar::Algorithm::Myers, &ours_lines, &theirs_lines);
    let first_change = operations
        .iter()
        .position(|operation| operation.tag() != similar::DiffTag::Equal);
    let Some(first_change) = first_change else {
        append_lines(output, &ours_lines);
        return false;
    };
    let last_change = operations
        .iter()
        .rposition(|operation| operation.tag() != similar::DiffTag::Equal)
        .unwrap_or(first_change);

    let first_old = operations[first_change].old_range();
    let first_new = operations[first_change].new_range();
    append_lines(output, &ours_lines[..first_old.start]);
    let mut group_old_start = first_old.start;
    let mut group_new_start = first_new.start;

    // XDL_MERGE_ZEALOUS re-diffs the two postimages, then folds equal runs of
    // at most three lines back into the surrounding conflict. Longer runs stay
    // visible as non-conflicting context between smaller conflict blocks.
    if first_change < last_change {
        for operation in &operations[(first_change + 1)..last_change] {
            if operation.tag() != similar::DiffTag::Equal {
                continue;
            }
            let old = operation.old_range();
            let new = operation.new_range();
            if old.len() <= 3 {
                continue;
            }
            append_conflict_block(
                output,
                &ours_lines[group_old_start..old.start],
                None,
                &theirs_lines[group_new_start..new.start],
                marker_len,
                eol,
                labels,
            );
            append_lines(output, &ours_lines[old.clone()]);
            group_old_start = old.end;
            group_new_start = new.end;
        }
    }

    let last_old = operations[last_change].old_range();
    let last_new = operations[last_change].new_range();
    append_conflict_block(
        output,
        &ours_lines[group_old_start..last_old.end],
        None,
        &theirs_lines[group_new_start..last_new.end],
        marker_len,
        eol,
        labels,
    );
    append_lines(output, &ours_lines[last_old.end..]);
    true
}

pub(crate) fn append_zdiff3_block(
    output: &mut Vec<u8>,
    ours: &[u8],
    base: &[u8],
    theirs: &[u8],
    marker_len: usize,
    eol: &[u8],
    labels: ConflictMarkerLabels<'_>,
) -> bool {
    let ours_lines = split_lines_preserving_eol(ours);
    let base_lines = split_lines_preserving_eol(base);
    let theirs_lines = split_lines_preserving_eol(theirs);
    let mut prefix = 0usize;
    while prefix < ours_lines.len()
        && prefix < theirs_lines.len()
        && ours_lines[prefix] == theirs_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < ours_lines.len().saturating_sub(prefix)
        && suffix < theirs_lines.len().saturating_sub(prefix)
        && ours_lines[ours_lines.len() - 1 - suffix]
            == theirs_lines[theirs_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    append_lines(output, &ours_lines[..prefix]);
    let ours_end = ours_lines.len() - suffix;
    let theirs_end = theirs_lines.len() - suffix;
    let remains_conflicted = prefix < ours_end || prefix < theirs_end;
    if remains_conflicted {
        append_conflict_block(
            output,
            &ours_lines[prefix..ours_end],
            Some(&base_lines),
            &theirs_lines[prefix..theirs_end],
            marker_len,
            eol,
            labels,
        );
    }
    append_lines(output, &ours_lines[ours_end..]);
    remains_conflicted
}

pub(crate) fn find_generated_marker(haystack: &[u8], start: usize, marker: &[u8]) -> Option<usize> {
    let tail = haystack.get(start..)?;
    let mut fallback = None;
    for relative in tail
        .windows(marker.len())
        .enumerate()
        .filter_map(|(index, window)| (window == marker).then_some(index))
    {
        let position = start + relative;
        fallback.get_or_insert(position);
        if position == 0 || haystack[position - 1] == b'\n' {
            return Some(position);
        }
    }
    // diffy does not insert a missing newline before the next marker. Retain a
    // fallback for an unterminated conflict side after preferring line-start
    // markers, which cannot collide with input because marker size is bumped.
    fallback
}

pub(crate) fn refine_diffy_conflicts(
    conflicted: &[u8],
    marker_len: usize,
    style: ConflictStyle,
    base_input: &[u8],
    ours_input: &[u8],
    theirs_input: &[u8],
    labels: ConflictMarkerLabels<'_>,
) -> Result<(Vec<u8>, bool), String> {
    let raw_eol = b"\n";
    let open = marker_bytes(b'<', marker_len, Some("ours"), raw_eol);
    let original = marker_bytes(b'|', marker_len, Some("original"), raw_eol);
    let separator = marker_bytes(b'=', marker_len, None, raw_eol);
    let close = marker_bytes(b'>', marker_len, Some("theirs"), raw_eol);
    let eol = conflict_marker_eol_for_inputs(&[ours_input, theirs_input, base_input]);
    let malformed = || "internal three-way merge produced malformed conflict markers".to_string();

    let mut output = Vec::with_capacity(conflicted.len());
    let mut cursor = 0usize;
    let mut parsed = 0usize;
    let mut has_conflicts = false;
    while let Some(open_start) = find_generated_marker(conflicted, cursor, &open) {
        output.extend_from_slice(&conflicted[cursor..open_start]);
        let ours_start = open_start + open.len();
        let (ours_end, base_range, separator_start) = match style {
            ConflictStyle::Merge => {
                let separator_start = find_generated_marker(conflicted, ours_start, &separator)
                    .ok_or_else(malformed)?;
                (separator_start, None, separator_start)
            }
            ConflictStyle::Diff3 | ConflictStyle::ZDiff3 => {
                let original_start = find_generated_marker(conflicted, ours_start, &original)
                    .ok_or_else(malformed)?;
                let base_start = original_start + original.len();
                let separator_start = find_generated_marker(conflicted, base_start, &separator)
                    .ok_or_else(malformed)?;
                (
                    original_start,
                    Some(base_start..separator_start),
                    separator_start,
                )
            }
        };
        let theirs_start = separator_start + separator.len();
        let close_start =
            find_generated_marker(conflicted, theirs_start, &close).ok_or_else(malformed)?;
        let ours = &conflicted[ours_start..ours_end];
        let theirs = &conflicted[theirs_start..close_start];
        has_conflicts |= match style {
            ConflictStyle::Merge => {
                append_refined_merge_block(&mut output, ours, theirs, marker_len, eol, labels)
            }
            ConflictStyle::Diff3 => {
                let base_range = base_range.ok_or_else(malformed)?;
                let ours_lines = split_lines_preserving_eol(ours);
                let base_lines = split_lines_preserving_eol(&conflicted[base_range]);
                let theirs_lines = split_lines_preserving_eol(theirs);
                append_conflict_block(
                    &mut output,
                    &ours_lines,
                    Some(&base_lines),
                    &theirs_lines,
                    marker_len,
                    eol,
                    labels,
                );
                true
            }
            ConflictStyle::ZDiff3 => {
                let base_range = base_range.ok_or_else(malformed)?;
                append_zdiff3_block(
                    &mut output,
                    ours,
                    &conflicted[base_range],
                    theirs,
                    marker_len,
                    eol,
                    labels,
                )
            }
        };
        cursor = close_start + close.len();
        parsed += 1;
    }
    if parsed == 0 {
        return Err(malformed());
    }
    output.extend_from_slice(&conflicted[cursor..]);
    Ok((output, has_conflicts))
}

pub(crate) fn merge_bytes_with_favor(
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    favor: MergeFavor,
) -> Result<Vec<u8>, String> {
    let marker_len = unambiguous_conflict_marker_length(&[base, ours, theirs]);
    let mut merge_options = diffy::MergeOptions::new();
    merge_options
        .set_conflict_style(diffy::ConflictStyle::Diff3)
        .set_conflict_marker_length(marker_len);
    match merge_options.merge_bytes(base, ours, theirs) {
        Ok(merged) => Ok(merged),
        Err(conflicted) => resolve_favored_content(conflicted, marker_len, favor),
    }
}

pub(crate) fn unambiguous_conflict_marker_length(sides: &[&[u8]]) -> usize {
    const DEFAULT_MARKER_LENGTH: usize = 7;
    let mut longest = 0usize;
    for side in sides {
        for marker in *b"<>=|" {
            let mut run = 0usize;
            for byte in *side {
                if *byte == marker {
                    run += 1;
                    longest = longest.max(run);
                } else {
                    run = 0;
                }
            }
        }
    }
    DEFAULT_MARKER_LENGTH.max(longest.saturating_add(1))
}

pub(crate) fn load_merge_blob(
    hash: ObjectHash,
    virtual_blobs: &VirtualBlobs,
) -> Result<Blob, PullMergeError> {
    if let Some(data) = virtual_blobs.get(&hash) {
        return Ok(Blob::from_content_bytes(data.clone()));
    }
    load_object(&hash).map_err(|error| PullMergeError::ObjectLoad {
        object_id: hash.to_string(),
        detail: error.to_string(),
    })
}
