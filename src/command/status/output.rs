//! Status output rendering: human, JSON, porcelain v1/v2 and short formats.
//! These renderers consume the shared `StatusData` shape collected by the
//! facade/scan/cache and never re-collect or mutate repository state.
#![allow(unused_imports)]

use std::{
    collections::{HashMap, HashSet},
    io,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};

use clap::{Parser, ValueEnum};
use git_internal::{
    errors::GitError,
    hash::ObjectHash,
    internal::{
        index::Index,
        object::{
            commit::Commit,
            tree::{Tree, TreeItemMode},
        },
    },
};
use serde::Serialize;

use super::{fail_closed_on_io_blocked, *};
use crate::{
    command::calc_file_blob_hash,
    internal::{
        branch::{Branch, BranchStoreError},
        config::ConfigKv,
        head::Head,
        shallow::ShallowSet,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        ignore::IgnorePolicy,
        object_ext::{CommitExt, TreeExt},
        output::{ColorChoice, OutputConfig, emit_json_data},
        path,
        pathspec::{PathspecError, PathspecSet},
        util,
    },
};

// ---------------------------------------------------------------------------
// Status model items and helpers that stay in the parent facade.
// ---------------------------------------------------------------------------
pub(super) async fn render_status_to_writer(
    data: &StatusData,
    args: &StatusArgs,
    output: &OutputConfig,
    writer: &mut impl Write,
) -> CliResult<()> {
    fail_closed_on_io_blocked(data, output)?;
    let write_error =
        |err: io::Error| crate::utils::output::stdout_write_error("write status output", err);
    let mut buffer = Vec::new();

    // §B.6.4 machine-format path base: porcelain v1/v2 ALWAYS emit
    // repository-root-relative paths (Git parity), while the human
    // formats honor `status.relativePaths`. The collected data carries
    // the display base, so project it back for the porcelain renderers.
    let porcelain_data;
    let data = if args.porcelain.is_some() {
        porcelain_data = data.to_repo_relative();
        &porcelain_data
    } else {
        data
    };

    // Porcelain modes
    match args.porcelain {
        Some(PorcelainVersion::V2) => {
            if args.branch {
                write_branch_info_v2(
                    &data.head,
                    data.head_oid.as_ref(),
                    data.upstream.as_ref(),
                    args.show_ahead_behind(),
                    args.null_terminated,
                    &mut buffer,
                )?;
            }
            output_porcelain_v2(
                &data.staged,
                &data.unstaged,
                &data.unmerged,
                &data.intent_to_add,
                &data.ignored_files,
                data.porcelain_v2.as_deref(),
                &data.staged_rename_details,
                &data.unstaged_rename_details,
                args.null_terminated,
                data.quote_path,
                &mut buffer,
            )?;
            writer.write_all(&buffer).map_err(write_error)?;
            return Ok(());
        }
        Some(PorcelainVersion::V1) => {
            if args.branch {
                print_branch_info(
                    &data.head,
                    data.upstream.as_ref(),
                    args.show_ahead_behind(),
                    args.null_terminated,
                    &mut buffer,
                )?;
            }
            output_porcelain_with_unmerged(
                &data.staged,
                &data.unstaged,
                &data.unmerged,
                &data.intent_to_add,
                args.null_terminated,
                data.quote_path,
                &mut buffer,
            )?;
            if args.ignored && !data.ignored_files.is_empty() {
                for file in &data.ignored_files {
                    if args.null_terminated {
                        write!(&mut buffer, "!! ").map_err(write_error)?;
                        write_raw_path(&mut buffer, file).map_err(write_error)?;
                        buffer.push(b'\0');
                    } else {
                        buffer.extend_from_slice(b"!! ");
                        buffer.extend_from_slice(&quote_pathname_bytes(file, data.quote_path));
                        buffer.push(b'\n');
                    }
                }
            }
            writer.write_all(&buffer).map_err(write_error)?;
            return Ok(());
        }
        None => {}
    };

    // `status.relativePaths=false`: Git renders the HUMAN formats (short and
    // long) with repository-root-relative paths. Collection stays cwd-relative
    // throughout (pathspec filtering and porcelain metadata lookups depend on
    // it); only the rendered copy is converted here. Porcelain/JSON output is
    // reached before this point and keeps its existing path shape.
    let rooted_data;
    let data = if args.relative_paths {
        data
    } else {
        rooted_data = data_with_repo_root_paths(data);
        &rooted_data
    };

    // Short format
    if args.short {
        if args.branch {
            print_branch_info(
                &data.head,
                data.upstream.as_ref(),
                args.show_ahead_behind(),
                args.null_terminated,
                &mut buffer,
            )?;
        }
        // `data` is cwd-relative for the default relativePaths mode; the
        // porcelain/`--no-relative` path passed above was root-relative, so
        // its renderers use the already-root sort key.
        let sort_base = args.relative_paths.then_some(data.sort_base.as_path());
        output_short_format_with_config(
            &data.staged,
            &data.unstaged,
            &data.unmerged,
            &data.intent_to_add,
            sort_base,
            output,
            args.null_terminated,
            data.quote_path,
            &mut buffer,
        )
        .await?;
        if args.ignored {
            for file in &data.ignored_files {
                if args.null_terminated {
                    write!(&mut buffer, "!! ").map_err(write_error)?;
                    write_raw_path(&mut buffer, file).map_err(write_error)?;
                    buffer.push(b'\0');
                } else {
                    buffer.extend_from_slice(b"!! ");
                    buffer.extend_from_slice(&quote_pathname_bytes(file, data.quote_path));
                    buffer.push(b'\n');
                }
            }
        }
        writer.write_all(&buffer).map_err(write_error)?;
        return Ok(());
    }

    // Standard human format
    render_human_status(data, args, &mut buffer)?;
    writer.write_all(&buffer).map_err(write_error)?;
    Ok(())
}

/// Convert every display path in `data` from cwd-relative to
/// repository-root-relative (`status.relativePaths=false`). Rename pairs,
/// unmerged entries, and ignored paths are converted alongside the staged and
/// unstaged change sets.
fn data_with_repo_root_paths(data: &StatusData) -> StatusData {
    // Collapsed untracked/ignored directories carry a deliberate trailing
    // `/` marker (see `status_untracked`); path conversion must not eat it,
    // or directories become indistinguishable from files in the output.
    fn convert(path: &Path) -> PathBuf {
        with_dir_marker(path, util::to_workdir_path(path))
    }
    fn changes(changes: &Changes) -> Changes {
        Changes {
            new: changes.new.iter().map(|p| convert(p)).collect(),
            modified: changes.modified.iter().map(|p| convert(p)).collect(),
            deleted: changes.deleted.iter().map(|p| convert(p)).collect(),
            renamed: changes
                .renamed
                .iter()
                .map(|(from, to)| (convert(from), convert(to)))
                .collect(),
        }
    }
    fn details(details: &RenameDetails) -> RenameDetails {
        details
            .iter()
            .map(|((from, to), value)| ((convert(from), convert(to)), *value))
            .collect()
    }
    let mut rooted = data.clone();
    rooted.staged = changes(&data.staged);
    rooted.unstaged = changes(&data.unstaged);
    rooted.intent_to_add = data.intent_to_add.iter().map(|p| convert(p)).collect();
    // Keep the score/exactness lookup keys aligned with the converted rename
    // pairs, or JSON emission from a subdirectory would miss every detail.
    rooted.staged_rename_details = details(&data.staged_rename_details);
    rooted.unstaged_rename_details = details(&data.unstaged_rename_details);
    rooted.unmerged = data
        .unmerged
        .iter()
        .map(|entry| entry.clone().with_path(convert(&entry.path)))
        .collect();
    rooted.ignored_files = data.ignored_files.iter().map(|p| convert(p)).collect();
    rooted
}

// ---------------------------------------------------------------------------
// Human standard format
// ---------------------------------------------------------------------------

fn render_human_status(
    data: &StatusData,
    args: &StatusArgs,
    buffer: &mut Vec<u8>,
) -> CliResult<()> {
    let write_error =
        |err: io::Error| crate::utils::output::stdout_write_error("write status output", err);

    // Branch header
    match &data.head {
        Head::Detached(commit_hash) => {
            writeln!(buffer, "HEAD detached at {}", &commit_hash.to_string()[..8])
                .map_err(write_error)?;
        }
        Head::Branch(branch) => {
            writeln!(buffer, "On branch {branch}").map_err(write_error)?;
        }
    }

    // Upstream tracking info
    if let Some(upstream) = &data.upstream {
        render_upstream_human(upstream, args.show_ahead_behind(), buffer)?;
    }

    if let Some(notice) = &data.sequence_notice {
        writeln!(buffer, "{notice}").map_err(write_error)?;
    }
    if data.sparse_view_active {
        writeln!(
            buffer,
            "note: a sparse view is active (scopes 'ls-files'/'diff' output; status is not filtered)"
        )
        .map_err(write_error)?;
    }
    if let Some(merge_state) = &data.merge_state {
        render_merge_state_human(merge_state, buffer)?;
    }

    if !data.has_commits {
        writeln!(buffer, "\nNo commits yet\n").map_err(write_error)?;
    }

    // Stash info
    if let Some(stash_count) = data.stash_count
        && stash_count > 0
    {
        let entry_text = if stash_count == 1 { "entry" } else { "entries" };
        writeln!(
            buffer,
            "Your stash currently has {stash_count} {entry_text}"
        )
        .map_err(write_error)?;
    }

    // Clean tree
    if data.merge_state.is_none()
        && data.staged.is_empty()
        && data.unstaged.is_empty()
        && data.intent_to_add.is_empty()
        && data.unmerged.is_empty()
    {
        writeln!(buffer, "nothing to commit, working tree clean").map_err(write_error)?;
        return Ok(());
    }

    // Staged changes
    if !data.staged.is_empty() {
        writeln!(buffer, "Changes to be committed:").map_err(write_error)?;
        writeln!(
            buffer,
            "  use \"libra restore --staged <file>...\" to unstage"
        )
        .map_err(write_error)?;
        let entries = build_human_entries(
            &data.staged.deleted,
            "deleted:",
            &data.staged.modified,
            "modified:",
            &data.staged.new,
            "new file:",
            &data.staged.renamed,
            "renamed:",
            data.quote_path,
        );
        if args.column {
            render_columnated_labeled_entries(buffer, &entries, colored::Color::BrightGreen)?;
        } else {
            for (label, path) in entries {
                let mut line = format!("\t{label} ").into_bytes();
                line.extend_from_slice(&path);
                push_colored_line(buffer, &colored::Color::BrightGreen.to_fg_str(), &line);
            }
        }
    }

    // Unstaged changes (modified + deleted + renamed + intent-to-add new
    // files — a probe-paired unstaged rename can be the section's ONLY
    // content, §B.3.1)
    if !data.unstaged.deleted.is_empty()
        || !data.unstaged.modified.is_empty()
        || !data.unstaged.renamed.is_empty()
        || !data.intent_to_add.is_empty()
    {
        writeln!(buffer, "Changes not staged for commit:").map_err(write_error)?;
        writeln!(
            buffer,
            "  use \"libra add <file>...\" to update what will be committed"
        )
        .map_err(write_error)?;
        writeln!(
            buffer,
            "  use \"libra restore <file>...\" to discard changes in working directory"
        )
        .map_err(write_error)?;
        let entries = build_human_entries(
            &data.unstaged.deleted,
            "deleted:",
            &data.unstaged.modified,
            "modified:",
            &data.intent_to_add,
            "new file:",
            &data.unstaged.renamed,
            "renamed:",
            data.quote_path,
        );
        if args.column {
            render_columnated_labeled_entries(buffer, &entries, colored::Color::BrightRed)?;
        } else {
            for (label, path) in entries {
                let mut line = format!("\t{label} ").into_bytes();
                line.extend_from_slice(&path);
                push_colored_line(buffer, &colored::Color::BrightRed.to_fg_str(), &line);
            }
        }
    }

    // ADR-FM-06 (#470 FM-05): a legacy-materialization tree loses executable
    // bits; FM-04 makes it visible as ` M`, this hint offers the one-shot
    // repair. Human long format only — short/porcelain/JSON stay unchanged.
    if !data.mode_fix_candidates.is_empty() {
        let count = data.mode_fix_candidates.len();
        let noun = if count == 1 { "file" } else { "files" };
        writeln!(
            buffer,
            "Note: {count} tracked {noun} lost the executable bit (index records 100755)"
        )
        .map_err(write_error)?;
        if count > 5 {
            writeln!(
                buffer,
                "  use \"libra restore -- .\" to restore all executable bits"
            )
            .map_err(write_error)?;
        } else {
            let paths = data
                .mode_fix_candidates
                .iter()
                .map(|path| {
                    String::from_utf8_lossy(&quote_pathname_bytes(path, data.quote_path))
                        .into_owned()
                })
                .collect::<Vec<_>>()
                .join(" ");
            writeln!(buffer, "  use \"libra restore -- {paths}\" to restore it")
                .map_err(write_error)?;
        }
    }

    if !data.unmerged.is_empty() {
        writeln!(buffer, "Unmerged paths:").map_err(write_error)?;
        writeln!(buffer, "  use \"libra add <file>...\" to mark resolution")
            .map_err(write_error)?;
        writeln!(
            buffer,
            "  use \"libra merge --abort\" or the active sequencer abort command to abort"
        )
        .map_err(write_error)?;
        let entries = data
            .unmerged
            .iter()
            .map(|entry| {
                (
                    unmerged_human_label(entry),
                    quote_pathname_bytes(&entry.path, data.quote_path),
                )
            })
            .collect::<Vec<_>>();
        if args.column {
            render_columnated_labeled_entries(buffer, &entries, colored::Color::BrightRed)?;
        } else {
            for (label, path) in entries {
                let mut line = format!("\t{label} ").into_bytes();
                line.extend_from_slice(&path);
                push_colored_line(buffer, &colored::Color::BrightRed.to_fg_str(), &line);
            }
        }
    }

    // Untracked
    if !data.unstaged.new.is_empty() {
        writeln!(buffer, "Untracked files:").map_err(write_error)?;
        writeln!(
            buffer,
            "  use \"libra add <file>...\" to include in what will be committed"
        )
        .map_err(write_error)?;
        if args.column {
            render_columnated_paths(buffer, &data.unstaged.new, data.quote_path)?;
        } else {
            for f in &data.unstaged.new {
                let mut line = b"\t".to_vec();
                line.extend_from_slice(&quote_pathname_bytes(f, data.quote_path));
                push_colored_line(buffer, &colored::Color::BrightRed.to_fg_str(), &line);
            }
        }
    }

    // Ignored
    if args.ignored && !data.ignored_files.is_empty() {
        writeln!(buffer, "Ignored files:").map_err(write_error)?;
        writeln!(
            buffer,
            "  (modify .libraignore to change which files are ignored)"
        )
        .map_err(write_error)?;
        if args.column {
            render_columnated_paths(buffer, &data.ignored_files, data.quote_path)?;
        } else {
            for f in &data.ignored_files {
                let mut line = b"\t".to_vec();
                line.extend_from_slice(&quote_pathname_bytes(f, data.quote_path));
                push_colored_line(buffer, &colored::Color::BrightRed.to_fg_str(), &line);
            }
        }
    }

    Ok(())
}

fn unmerged_human_label(entry: &UnmergedEntry) -> &'static str {
    match entry.xy() {
        ('D', 'D') => "both deleted:",
        ('A', 'U') => "added by us:",
        ('U', 'D') => "deleted by them:",
        ('U', 'A') => "added by them:",
        ('D', 'U') => "deleted by us:",
        ('A', 'A') => "both added:",
        _ => "both modified:",
    }
}

/// Build a flat list of (label, path) for human output.
#[allow(clippy::too_many_arguments)]
fn build_human_entries<'a>(
    deleted: &[PathBuf],
    deleted_label: &'a str,
    modified: &[PathBuf],
    modified_label: &'a str,
    new_files: &[PathBuf],
    new_label: &'a str,
    renamed: &[(PathBuf, PathBuf)],
    renamed_label: &'a str,
    quote_path: bool,
) -> Vec<(&'a str, Vec<u8>)> {
    let mut entries = Vec::new();
    for f in deleted {
        entries.push((deleted_label, quote_pathname_bytes(f, quote_path)));
    }
    for f in modified {
        entries.push((modified_label, quote_pathname_bytes(f, quote_path)));
    }
    for (old, new) in renamed {
        let mut line = quote_pathname_bytes(old, quote_path);
        line.extend_from_slice(b" -> ");
        line.extend_from_slice(&quote_pathname_bytes(new, quote_path));
        entries.push((renamed_label, line));
    }
    for f in new_files {
        entries.push((new_label, quote_pathname_bytes(f, quote_path)));
    }
    entries
}

/// Render labeled entries in aligned columns.
fn render_columnated_labeled_entries(
    buffer: &mut Vec<u8>,
    entries: &[(&str, Vec<u8>)],
    color: colored::Color,
) -> CliResult<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let max_label_width = entries.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
    for (label, path) in entries {
        let mut line = format!("\t{label:max_label_width$} ").into_bytes();
        line.extend_from_slice(path);
        push_colored_line(buffer, &color.to_fg_str(), &line);
    }
    Ok(())
}

/// Wrap one content line in an ANSI fg color + reset and terminate it,
/// honoring the colored crate's own colorize gate (so piped/test output
/// stays plain, exactly like the `.bright_green()` call sites it
/// replaces). The colored crate's API is `String`-only, so byte-faithful
/// paths (raw non-UTF-8 bytes under `core.quotePath=false`) are colored
/// manually with the same codes the crate emits.
fn push_colored_line(buffer: &mut Vec<u8>, fg: &str, line: &[u8]) {
    let colorize = colored::control::SHOULD_COLORIZE.should_colorize();
    if colorize {
        buffer.extend_from_slice(b"\x1b[");
        buffer.extend_from_slice(fg.as_bytes());
        buffer.extend_from_slice(b"m");
    }
    buffer.extend_from_slice(line);
    if colorize {
        buffer.extend_from_slice(b"\x1b[0m");
    }
    buffer.push(b'\n');
}

/// Render plain paths in multiple columns like `ls`.
fn render_columnated_paths(
    buffer: &mut Vec<u8>,
    paths: &[PathBuf],
    quote_path: bool,
) -> CliResult<()> {
    let write_error =
        |err: io::Error| crate::utils::output::stdout_write_error("write status output", err);
    if paths.is_empty() {
        return Ok(());
    }

    let names: Vec<Vec<u8>> = paths
        .iter()
        .map(|p| quote_pathname_bytes(p, quote_path))
        .collect();
    let widths: Vec<usize> = names.iter().map(|n| n.len()).collect();
    let max_width = *widths.iter().max().unwrap_or(&0);
    let term_width = terminal_width().unwrap_or(80);
    // Leave a leading tab and some padding room.
    let usable_width = term_width.saturating_sub(8);
    let col_width = max_width + 2;
    let num_cols = usable_width
        .checked_div(col_width)
        .unwrap_or(usable_width)
        .max(1);
    let num_rows = names.len().div_ceil(num_cols);

    for row in 0..num_rows {
        write!(buffer, "\t").map_err(write_error)?;
        for col in 0..num_cols {
            let idx = col * num_rows + row;
            if idx >= names.len() {
                break;
            }
            let name = &names[idx];
            buffer.extend_from_slice(name);
            if col + 1 < num_cols {
                for _ in name.len()..col_width {
                    buffer.push(b' ');
                }
            }
        }
        writeln!(buffer).map_err(write_error)?;
    }
    Ok(())
}

/// Best-effort terminal width.
fn terminal_width() -> Option<usize> {
    if std::io::stdout().is_terminal() {
        std::env::var("COLUMNS")
            .ok()
            .and_then(|s| s.parse().ok())
            .or(Some(80))
    } else {
        None
    }
}

fn render_merge_state_human(merge_state: &MergeStatusInfo, buffer: &mut Vec<u8>) -> CliResult<()> {
    let write_error =
        |err: io::Error| crate::utils::output::stdout_write_error("write status output", err);

    writeln!(
        buffer,
        "You are in the middle of a merge with '{}'.",
        merge_state.target_ref
    )
    .map_err(write_error)?;
    if merge_state.unresolved_count == 0 {
        writeln!(
            buffer,
            "  (all conflicts fixed: run \"libra merge --continue\")"
        )
        .map_err(write_error)?;
    } else if merge_state.conflicted_paths.is_empty() {
        writeln!(
            buffer,
            "  (conflicts remain outside the selected pathspec; run \"libra status\" to see them)"
        )
        .map_err(write_error)?;
    } else {
        writeln!(
            buffer,
            "  (fix conflicts and run \"libra merge --continue\")"
        )
        .map_err(write_error)?;
    }
    writeln!(buffer, "  (use \"libra merge --abort\" to abort the merge)").map_err(write_error)?;
    Ok(())
}

fn render_upstream_human(
    upstream: &UpstreamInfo,
    show_counts: bool,
    buffer: &mut Vec<u8>,
) -> CliResult<()> {
    let write_error =
        |err: io::Error| crate::utils::output::stdout_write_error("write status output", err);

    if upstream.gone {
        writeln!(
            buffer,
            "Your branch is based on '{}', but the upstream is gone.",
            upstream.remote_ref
        )
        .map_err(write_error)?;
        return Ok(());
    }

    // ahead/behind are None on an unborn branch (no local commit to compare).
    let (ahead, behind) = match (upstream.ahead, upstream.behind) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            // Unborn branch: upstream exists but no local commits yet.
            return Ok(());
        }
    };

    if ahead == 0 && behind == 0 {
        writeln!(
            buffer,
            "Your branch is up to date with '{}'.",
            upstream.remote_ref
        )
        .map_err(write_error)?;
    } else if ahead > 0 && behind == 0 {
        // Git drops the numeric count under `--no-ahead-behind`.
        if show_counts {
            writeln!(
                buffer,
                "Your branch is ahead of '{}' by {} commit{}.",
                upstream.remote_ref,
                ahead,
                if ahead == 1 { "" } else { "s" }
            )
            .map_err(write_error)?;
        } else {
            writeln!(buffer, "Your branch is ahead of '{}'.", upstream.remote_ref)
                .map_err(write_error)?;
        }
        writeln!(
            buffer,
            "  (use \"libra push\" to publish your local commits)"
        )
        .map_err(write_error)?;
    } else if ahead == 0 && behind > 0 {
        if show_counts {
            writeln!(
                buffer,
                "Your branch is behind '{}' by {} commit{}.",
                upstream.remote_ref,
                behind,
                if behind == 1 { "" } else { "s" }
            )
            .map_err(write_error)?;
        } else {
            writeln!(buffer, "Your branch is behind '{}'.", upstream.remote_ref)
                .map_err(write_error)?;
        }
        writeln!(buffer, "  (use \"libra pull\" to update your local branch)")
            .map_err(write_error)?;
    } else if show_counts {
        writeln!(
            buffer,
            "Your branch and '{}' have diverged,",
            upstream.remote_ref
        )
        .map_err(write_error)?;
        writeln!(
            buffer,
            "and have {ahead} and {behind} different commits each, respectively."
        )
        .map_err(write_error)?;
        writeln!(
            buffer,
            "  (use \"libra pull\" to merge the remote branch into yours)"
        )
        .map_err(write_error)?;
    } else {
        writeln!(
            buffer,
            "Your branch and '{}' refer to different commits.",
            upstream.remote_ref
        )
        .map_err(write_error)?;
        writeln!(
            buffer,
            "  (use \"libra pull\" to merge the remote branch into yours)"
        )
        .map_err(write_error)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// JSON rendering
// ---------------------------------------------------------------------------

/// Render collected warnings to stderr (`warning: …`) and mark the global
/// warning tracker — human/short/porcelain delivery only (§B.5 matrix).
/// JSON callers must NOT use this: their warnings ride in `data.warnings[]`.
pub fn deliver_warnings_stderr(warnings: &[StatusWarning]) {
    // In TEXT mode RepositoryPreflight entries were already printed live by
    // `emit_warning` when the preflight raised them (with structured output
    // inactive it prints immediately); they join the structured list only so
    // JSON payloads and §B.5 exit arbitration see them, and re-printing them
    // here doubled the stderr line. Under a structured envelope nothing was
    // printed live (`emit_warning` only buffers), so the JSON output-failure
    // fallback that calls this MUST deliver them or they vanish with the
    // envelope — the skip is therefore conditional on live delivery having
    // happened.
    let preflight_already_live = !crate::utils::output::structured_output_active();
    let mut delivered_any = false;
    for warning in warnings {
        if preflight_already_live && matches!(warning.code, StatusWarningCode::RepositoryPreflight)
        {
            continue;
        }
        eprintln!("warning: {}", warning.message);
        delivered_any = true;
    }
    if delivered_any {
        crate::utils::output::record_warning();
    }
}

/// Build a `source = cache` structured warning (§B.5 R0-8b).
pub fn cache_warning(code: StatusWarningCode, message: impl Into<String>) -> StatusWarning {
    StatusWarning {
        code,
        message: message.into(),
        source: code.source(),
    }
}

/// The single §B.5 exit arbitration point.
///
/// Every status path — full scan, `--scan`, both cache modes, and both
/// fallbacks — resolves its exit code here instead of repeating the
/// comparison. That matters because the ordering is subtle and one branch
/// getting it wrong is invisible in review: an early `silent_exit(1)` for a
/// dirty tree would preempt the warning exit 9 that is supposed to outrank
/// it. Priority: **fatal ≻ 9 (`--exit-code-on-warning`) ≻ 1 (dirty,
/// including a non-empty `io_blocked`) ≻ 0**. Fatal is raised earlier by
/// `fail_closed_on_io_blocked`, so this resolver covers 9 ≻ 1 ≻ 0.
pub struct StatusOutcome<'a> {
    data: &'a StatusData,
    args: &'a StatusArgs,
}

impl<'a> StatusOutcome<'a> {
    pub fn new(data: &'a StatusData, args: &'a StatusArgs) -> Self {
        Self { data, args }
    }

    pub fn resolve(&self, output: &OutputConfig) -> CliResult<()> {
        if let Some(exit) = warning_exit(output, &self.data.warnings) {
            return Err(exit);
        }
        // `--exit-code`: dirty → exit 1, silently (no error line).
        if self.args.exit_code && self.data.is_dirty() {
            return Err(CliError::silent_exit(1));
        }
        Ok(())
    }
}

/// §B.5 exit arbitration, rule 2: warnings + `--exit-code-on-warning` exit 9
/// and take precedence over the dirty exit 1. Local to each return point
/// because an early `silent_exit(1)` would otherwise preempt the top-level
/// exit-9 pass in `cli.rs` (and JSON never records globally).
fn warning_exit(output: &OutputConfig, warnings: &[StatusWarning]) -> Option<CliError> {
    // Decided from THIS invocation's structured list only. Consulting the
    // process-global tracker would let a warning emitted by an earlier
    // embedded call flip a later, clean one to exit 9 with an empty
    // `warnings[]` — and the reverse leak, where a status call marks the
    // tracker for whoever runs next. Preflight advisories are folded into
    // the list at collection time, so nothing is lost by dropping the
    // global read.
    (output.exit_code_on_warning && !warnings.is_empty()).then(|| CliError::silent_exit(9))
}

/// Sort key for `io_blocked[]`: the RAW path encoding, matching what
/// `raw_base64` serializes. `PathBuf`'s own ordering compares WTF-8 bytes on
/// Windows, which disagrees with UTF-16 code-unit order once supplementary
/// characters are involved — so a machine consumer relying on the documented
/// "sorted by raw path bytes" would see a different order than it computed.
pub fn raw_path_sort_key(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        // BIG-endian in the SORT KEY only. The documented order is by UTF-16
        // code unit, and a bytewise comparison reproduces that only when the
        // high byte comes first: little-endian would put U+0101 (`01 01`)
        // after U+0200 (`00 02`), inverting the required order. The
        // published `raw_base64` stays little-endian — the key exists to be
        // compared, not to be transmitted.
        let mut bytes = Vec::new();
        for unit in path.as_os_str().encode_wide() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        bytes
    }
    #[cfg(not(any(unix, windows)))]
    path.to_string_lossy().into_owned().into_bytes()
}

/// Reversible encoding of a path whose name is not valid UTF-8, for the
/// `io_blocked[].path.raw_base64` contract (§B.6.0.1). Returns `None` for a
/// valid-UTF-8 name, whose `display` form is already lossless.
///
/// Unix encodes the raw `OsStr` bytes. Windows encodes the UTF-16 code units
/// LITTLE-ENDIAN, because an unpaired surrogate has no UTF-8 form at all —
/// returning `None` there would break reversibility for exactly the names
/// that need it.
pub fn raw_path_base64(path: &Path) -> Option<String> {
    use base64::Engine as _;

    if path.to_str().is_some() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Some(base64::engine::general_purpose::STANDARD.encode(path.as_os_str().as_bytes()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let mut bytes = Vec::new();
        for unit in path.as_os_str().encode_wide() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        Some(base64::engine::general_purpose::STANDARD.encode(bytes))
    }
    #[cfg(not(any(unix, windows)))]
    None
}

/// A path as a JSON string, using the same escaping the docs promise for
/// display forms. Undecodable bytes become `\ooo` octal escapes rather than
/// `U+FFFD`, so distinct filenames stay distinct in the payload. The quoting
/// wrapper is stripped: JSON supplies its own quoting.
fn json_path_string(path: &Path, quote_path: bool) -> String {
    match path.to_str() {
        Some(text) => text.to_string(),
        None => {
            let quoted = quote_pathname(path, quote_path);
            quoted
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .map(str::to_string)
                .unwrap_or(quoted)
        }
    }
}

pub fn build_status_json(data: &StatusData, _args: &StatusArgs) -> serde_json::Value {
    // §B.5 delivery matrix: porcelain and JSON paths are ALWAYS
    // repository-root-relative, regardless of the invocation subdirectory.
    // Collection stays cwd-relative (pathspec filtering depends on it), so
    // the JSON payload converts here — identity when cwd is the repo root.
    let rooted = data_with_repo_root_paths(data);
    let data = &rooted;
    // Paths render through the SAME escaping the docs promise for display
    // forms. `Path::display()` replaces undecodable bytes with U+FFFD, so
    // two different real filenames could collapse to one JSON string —
    // silently merging distinct entries for every consumer.
    let quote = data.quote_path;
    let paths_to_json = move |paths: &[PathBuf]| -> Vec<serde_json::Value> {
        paths
            .iter()
            .map(|p| serde_json::Value::String(json_path_string(p, quote)))
            .collect()
    };

    let renamed_to_json = move |renamed: &[(PathBuf, PathBuf)]| -> Vec<serde_json::Value> {
        renamed
            .iter()
            .map(|(old, new)| {
                serde_json::json!({
                    "from": json_path_string(old, quote),
                    "to": json_path_string(new, quote),
                })
            })
            .collect()
    };

    // Top-level `renames[]` with score/exactness/side (§B.6.5), sorted by the
    // destination path for determinism.
    let mut renames: Vec<serde_json::Value> = Vec::new();
    let mut push_renames = |pairs: &[(PathBuf, PathBuf)], details: &RenameDetails, staged: bool| {
        for (old, new) in pairs {
            let (score, exact) = details
                .get(&(old.clone(), new.clone()))
                .copied()
                .unwrap_or((100, true));
            renames.push(serde_json::json!({
                "from": json_path_string(old, quote),
                "to": json_path_string(new, quote),
                "score": score,
                "exact": exact,
                "staged": staged,
                "unstaged": !staged,
            }));
        }
    };
    push_renames(&data.staged.renamed, &data.staged_rename_details, true);
    push_renames(&data.unstaged.renamed, &data.unstaged_rename_details, false);
    renames.sort_by(|a, b| a["to"].as_str().cmp(&b["to"].as_str()));

    // §B.6.0.1 io_blocked[] public contract: escaped repo-relative display
    // (same quoting as non-`-z` porcelain), lossless raw bytes for
    // non-UTF-8 paths, the KNOWN staged component only, the reason
    // taxonomy, and the staged rename pair when one is known. Sorted by raw
    // path, deduplicated. Every entry also emits a worktree-family warning.
    // Warnings already carry the worktree family from collection time
    // (§B.5 single arbitration source); JSON only serializes them.
    let warnings_json = data.warnings.clone();
    let mut io_blocked_json: Vec<serde_json::Value> = Vec::new();
    for event in &data.io_blocked {
        let display = quote_pathname(&event.path, data.quote_path);
        let raw_base64: serde_json::Value = match raw_path_base64(&event.path) {
            Some(encoded) => serde_json::Value::String(encoded),
            None => serde_json::Value::Null,
        };
        // Compare on REPO-RELATIVE keys: `event.path` is repo-relative
        // while the change lists carry the display base, so from a
        // subdirectory a display-base conversion of the event would never
        // match (the historical `staged`/`rename` = null bug).
        // The change lists may carry repo-relative OR display-base paths
        // depending on the caller; accept either spelling of the same file
        // so the schema fields stay correct from a subdirectory.
        let matches_event = |candidate: &PathBuf| -> bool {
            candidate == &event.path || current_to_workdir(candidate) == event.path
        };
        let staged_component = if data.staged.modified.iter().any(matches_event) {
            serde_json::json!("M")
        } else if data.staged.new.iter().any(matches_event) {
            serde_json::json!("A")
        } else if data.staged.deleted.iter().any(matches_event) {
            serde_json::json!("D")
        } else if data
            .staged
            .renamed
            .iter()
            .any(|(_, new)| matches_event(new))
        {
            serde_json::json!("R")
        } else {
            serde_json::Value::Null
        };
        let rename = data
            .staged
            .renamed
            .iter()
            .find(|(_, new)| matches_event(new))
            .map(|pair| {
                let score = data
                    .staged_rename_details
                    .get(pair)
                    .map(|(pct, _)| *pct)
                    .unwrap_or(100);
                // Lossless like every other JSON path in the payload —
                // `display()` would U+FFFD-corrupt a non-UTF-8 pair
                // (2026-08-06 R0-6 review; latent, since staged pairs
                // currently require addable UTF-8 index paths).
                serde_json::json!({
                    "from": json_path_string(&pair.0, quote),
                    "to": json_path_string(&pair.1, quote),
                    "score": score,
                })
            })
            .unwrap_or(serde_json::Value::Null);
        let (reason, _warning_code) = io_blocked_reason_and_code(event.reason);
        io_blocked_json.push(serde_json::json!({
            "path": { "display": display, "raw_base64": raw_base64 },
            "staged": staged_component,
            "reason": reason,
            "rename": rename,
        }));
    }
    // §B.6.0.1: rename detection is complete only when nothing degraded it —
    // no probe truncation/blocks and no engine skip/limit/budget warnings.
    let rename_detection_complete = !data.rename_scan_blocked
        && !data.warnings.iter().any(|w| {
            matches!(
                w.code,
                StatusWarningCode::ProbeTruncated
                    | StatusWarningCode::RenameLimitProductSkipped
                    | StatusWarningCode::SimilarityBudgetExceeded
                    | StatusWarningCode::MetadataUnavailable
                    | StatusWarningCode::MetadataBudgetExceeded
                    | StatusWarningCode::WorktreeBudgetExceeded
                    | StatusWarningCode::RenamePathEncodingUnsupported
            )
        });

    let head = match &data.head {
        Head::Branch(name) => serde_json::json!({"type": "branch", "name": name}),
        Head::Detached(hash) => {
            serde_json::json!({"type": "detached", "oid": hash.to_string()})
        }
    };

    let upstream_json = match &data.upstream {
        Some(u) => serde_json::json!({
            "remote_ref": u.remote_ref,
            "ahead": u.ahead,
            "behind": u.behind,
            "gone": u.gone,
        }),
        None => serde_json::Value::Null,
    };

    let mut json_data = serde_json::json!({
        "head": head,
        "has_commits": data.has_commits,
        "upstream": upstream_json,
        "staged": {
            "new": paths_to_json(&data.staged.new),
            "modified": paths_to_json(&data.staged.modified),
            "deleted": paths_to_json(&data.staged.deleted),
            "renamed": renamed_to_json(&data.staged.renamed),
        },
        "unstaged": {
            "new": paths_to_json(&data.intent_to_add),
            "intent_to_add": paths_to_json(&data.intent_to_add),
            "modified": paths_to_json(&data.unstaged.modified),
            "deleted": paths_to_json(&data.unstaged.deleted),
            "renamed": renamed_to_json(&data.unstaged.renamed),
        },
        "unmerged": paths_to_json(
            &data
                .unmerged
                .iter()
                .map(|entry| entry.path.clone())
                .collect::<Vec<_>>()
        ),
        "untracked": paths_to_json(&data.unstaged.new),
        "ignored": paths_to_json(&data.ignored_files),
        "warnings": warnings_json,
        "renames": renames,
        "io_blocked": io_blocked_json,
        "base_scan_complete": !data.base_scan_blocked,
        "rename_detection_complete": rename_detection_complete,
        "complete": !data.base_scan_blocked && rename_detection_complete,
        "is_clean": !data.is_dirty(),
    });

    if let Some(merge_state) = &data.merge_state
        && let Some(map) = json_data.as_object_mut()
    {
        map.insert(
            "merge_state".to_string(),
            serde_json::json!({
                "target_ref": merge_state.target_ref,
                "conflicted_paths": merge_state.conflicted_paths,
            }),
        );
    }

    if let Some(stash_count) = data.stash_count
        && let Some(map) = json_data.as_object_mut()
    {
        map.insert("stash_entries".to_string(), serde_json::json!(stash_count));
    }

    json_data
}

// ---------------------------------------------------------------------------
// Porcelain v1
// ---------------------------------------------------------------------------

pub fn output_porcelain(
    staged: &Changes,
    unstaged: &Changes,
    null_terminated: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    output_porcelain_with_unmerged(staged, unstaged, &[], &[], null_terminated, true, writer)
}

fn output_porcelain_with_unmerged(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
    intent_to_add: &[PathBuf],
    null_terminated: bool,
    quote_path: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);

    // Renames render as a single `R  <old> -> <new>` record (Git porcelain v1
    // §B.6.3), never as two `R` endpoint rows. Under `-z` the record is
    // `XY SP <new> NUL <old> NUL` (raw path bytes, new before old, matching
    // Git); non-`-z` paths go through `quote_pathname` (§B.6.6).
    for entry in
        generate_short_status_entries_with_unmerged(staged, unstaged, unmerged, intent_to_add, None)
    {
        match entry {
            ShortStatusEntry::Path {
                path,
                staged: x,
                unstaged: y,
            } => {
                if null_terminated {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    write_raw_path(writer, &path).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                } else {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&path, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b"\n").map_err(write_err)?;
                }
            }
            ShortStatusEntry::Rename {
                old,
                new,
                staged: x,
                unstaged: y,
            } => {
                if null_terminated {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    write_raw_path(writer, &new).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                    write_raw_path(writer, &old).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                } else {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&old, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b" -> ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&new, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b"\n").map_err(write_err)?;
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Porcelain v2
// ---------------------------------------------------------------------------

/// File information from HEAD tree for porcelain v2 output.
struct FileInfo {
    mode: u32,
    hash: String,
}

pub struct PorcelainV2Data {
    index: Index,
    head_tree_items: HashMap<PathBuf, FileInfo>,
}

fn tree_item_mode_to_u32(mode: TreeItemMode) -> u32 {
    match mode {
        TreeItemMode::Blob => 0o100644,
        TreeItemMode::BlobExecutable => 0o100755,
        TreeItemMode::Link => 0o120000,
        TreeItemMode::Tree => 0o040000,
        TreeItemMode::Commit => 0o160000,
    }
}

/// Classify a raw index entry mode into the tree-item mode it would commit as,
/// mirroring `tree::create_tree_from_index`. Lets staged-change detection notice
/// a mode-only change (e.g. the executable bit set by `add --chmod=+x`).
pub fn index_mode_to_tree_item_mode(mode: u32) -> TreeItemMode {
    match mode & 0o170000 {
        0o120000 => TreeItemMode::Link,
        0o040000 => TreeItemMode::Tree,
        0o160000 => TreeItemMode::Commit,
        _ if mode & 0o111 != 0 => TreeItemMode::BlobExecutable,
        _ => TreeItemMode::Blob,
    }
}

fn format_mode(mode: u32) -> String {
    format!("{:06o}", mode)
}

pub fn current_to_workdir(path: &std::path::Path) -> PathBuf {
    let abs_path = util::cur_dir().join(path);
    util::to_workdir_path(&abs_path)
}

/// Tri-state worktree mode. "Gone" and "unreadable" are different answers:
/// the first is representable (`000000`), the second must never be guessed.
enum WorktreeMode {
    Mode(u32),
    Gone,
    Unreadable,
}

/// Mode of an already repository-root-relative path. The porcelain v2
/// payload is projected to repo-root paths before rendering, so it must NOT
/// go through `current_to_workdir` a second time.
fn get_worktree_mode_result_for_workdir(workdir_path: &std::path::Path) -> WorktreeMode {
    // Debug-only seam: the RENDER-time unreadable branch is otherwise only
    // reachable by winning a race between collection and rendering, so tests
    // name the path that must report as unreadable.
    #[cfg(debug_assertions)]
    if std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_some()
        && let Ok(target) = std::env::var("LIBRA_TEST_UNREADABLE_MODE_PATH")
        && !target.is_empty()
        && workdir_path == std::path::Path::new(&target)
    {
        return WorktreeMode::Unreadable;
    }
    let abs_path = util::workdir_to_absolute(workdir_path);
    match std::fs::symlink_metadata(&abs_path) {
        Ok(metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                WorktreeMode::Mode(if metadata.file_type().is_symlink() {
                    0o120000
                } else if metadata.permissions().mode() & 0o111 != 0 {
                    0o100755
                } else {
                    0o100644
                })
            }
            #[cfg(not(unix))]
            {
                let _ = metadata;
                WorktreeMode::Mode(0o100644)
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => WorktreeMode::Gone,
        Err(_) => WorktreeMode::Unreadable,
    }
}

/// Worktree mode of an already repository-root-relative path, with the
/// pre-R0 lenient fallback. The porcelain v2 payload is projected once at
/// the render entry (`to_repo_relative`), so this must NOT re-project via
/// `current_to_workdir` — from a subdirectory that second projection made
/// every lookup miss and fabricated `100644` (2026-08-06 R0-5 review).
/// The fallback (vs the rename arm's hard error) is justified by the
/// collection phase failing closed on unreadable tracked paths first.
fn get_worktree_mode(workdir_path: &std::path::Path) -> u32 {
    match get_worktree_mode_result_for_workdir(workdir_path) {
        WorktreeMode::Mode(mode) => mode,
        _ => 0o100644,
    }
}

/// Whether `path` is recorded at stage 0 of `index` as a `160000` gitlink.
pub fn is_gitlink_index_entry(index: &Index, path: &str) -> bool {
    index
        .get(path, 0)
        .is_some_and(|entry| is_submodule_mode(entry.mode))
}

fn is_submodule_mode(mode: u32) -> bool {
    mode == 0o160000
}

fn get_submodule_status(_file_path: &std::path::Path) -> String {
    "S...".to_string()
}

pub fn build_porcelain_v2_data(
    index: Index,
    head_oid: Option<&ObjectHash>,
) -> CliResult<PorcelainV2Data> {
    let head_tree_items = if let Some(commit_hash) = head_oid {
        let (_, tree) = load_head_commit_tree(commit_hash)?;
        try_get_plain_items_with_mode(&tree)
            .map_err(|source| StatusError::HeadTreeExpansion { source })?
            .into_iter()
            .map(|(path, hash, mode)| {
                (
                    path,
                    FileInfo {
                        mode: tree_item_mode_to_u32(mode),
                        hash: hash.to_string(),
                    },
                )
            })
            .collect()
    } else {
        HashMap::new()
    };

    Ok(PorcelainV2Data {
        index,
        head_tree_items,
    })
}

pub(crate) fn build_porcelain_v2_data_with_head(
    index: Index,
    head_tree: Option<&HeadTreeSnapshot>,
) -> PorcelainV2Data {
    let head_tree_items = head_tree
        .map(|snapshot| {
            snapshot
                .items
                .iter()
                .map(|(path, hash, mode)| {
                    (
                        path.clone(),
                        FileInfo {
                            mode: tree_item_mode_to_u32(*mode),
                            hash: hash.to_string(),
                        },
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    PorcelainV2Data {
        index,
        head_tree_items,
    }
}

/// Emit a porcelain v2 `2 <xy> …` rename record (§B.6.4):
/// `2 <xy> <sub> <mH> <mI> <mW> <hH> <hI> R<pct> <new>\t<old>`. Under `-z` the
/// path field becomes `<new> NUL <old> NUL`.
#[allow(clippy::too_many_arguments)]
fn write_rename_porcelain_v2(
    old: &Path,
    new: &Path,
    x: char,
    y: char,
    score: u32,
    metadata: &PorcelainV2Data,
    zero_hash: &str,
    null_terminated: bool,
    quote_path: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);
    // The porcelain payload was ALREADY projected to repository-root paths
    // (`to_repo_relative`), so these keys are used as-is. Converting again
    // would double the prefix when `status` runs from a subdirectory —
    // `sub/a.txt` became `sub/sub/a.txt`, the HEAD/index lookups missed, and
    // the record fail-closed instead of reporting its real mode and hash.
    let old_workdir = old.to_path_buf();
    let new_workdir = new.to_path_buf();

    // Staged rename: HEAD side is the OLD path, index side is the NEW path.
    // Unstaged-only rename (`.R`): there is no staged component, so Git copies
    // the index fields into the HEAD fields.
    let staged_rename = x == 'R';
    // §B.6.4 forbids fabricated all-zero hashes / default modes in a
    // rename record: a script that trusts `2 R…` must be able to trust
    // its mode+hash columns. Missing metadata is an internal
    // inconsistency, so fail closed with the offending path instead.
    let missing = |field: &str, path: &Path| -> CliError {
        CliError::fatal(format!(
            "cannot render the porcelain v2 rename record for '{}': {field} metadata is missing",
            path.display()
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
        .with_hint("re-run 'libra status' after 'libra add'/'libra reset' settles the index")
    };
    // The HEAD side comes from the HEAD TREE whenever the record has a real
    // staged component — `R.` (staged rename) and `MR`/`AR` (an unstaged
    // rename whose SOURCE also changed in the index) alike. Only a pure
    // `.R`, where HEAD and index agree by construction, copies the index
    // fields; doing that for `MR` would claim HEAD matches an index the user
    // just changed.
    let head_from_tree = staged_rename || x != '.';
    let (mode_head, hash_head) = if staged_rename {
        metadata
            .head_tree_items
            .get(&old_workdir)
            .map(|info| (info.mode, info.hash.clone()))
            .ok_or_else(|| missing("HEAD tree", &old_workdir))?
    } else if head_from_tree {
        // `MR`/`AR`: the source is the HEAD path. An `A` source has no HEAD
        // entry at all, which is exactly what `A` means, so the zero hash is
        // the honest answer there rather than a fabrication.
        match metadata.head_tree_items.get(&old_workdir) {
            Some(info) => (info.mode, info.hash.clone()),
            None if x == 'A' => (0, zero_hash.to_string()),
            None => return Err(missing("HEAD tree", &old_workdir)),
        }
    } else {
        // `.R`: filled from the index below (fixup).
        (0, zero_hash.to_string())
    };
    let index_key = if staged_rename {
        &new_workdir
    } else {
        &old_workdir
    };
    let index_str = index_key
        .to_str()
        .ok_or_else(|| missing("index (non-UTF-8 path)", index_key))?;
    let (mode_index, hash_index) = metadata
        .index
        .get(index_str, 0)
        .map(|entry| (entry.mode, entry.hash.to_string()))
        .ok_or_else(|| missing("index", index_key))?;
    let (mode_head, hash_head) = if staged_rename || head_from_tree {
        (mode_head, hash_head)
    } else {
        (mode_index, hash_index.clone())
    };
    // A worktree-deleted destination (`RD`) has no worktree entry: mW must
    // be 000000 like an ordinary v2 deleted row, not a fabricated 100644.
    let mode_worktree = if y == 'D' {
        0
    } else {
        // A mode read that FAILS is not `100644`. Between the scan and this
        // render the destination can be deleted or made unreadable; emitting
        // a fabricated regular-file mode would hand a script a value the
        // filesystem never reported. Fail closed instead — the same rule the
        // hash fields already follow.
        match get_worktree_mode_result_for_workdir(&new_workdir) {
            WorktreeMode::Mode(mode) => mode,
            // Genuinely absent (a chained rename already moved it on):
            // `000000`, the v2 spelling for "no worktree entry".
            WorktreeMode::Gone => 0,
            // Present but UNREADABLE: a fabricated `100644` would hand a
            // script a mode the filesystem never reported.
            WorktreeMode::Unreadable => {
                return Err(CliError::fatal(format!(
                    "cannot read the worktree mode of '{}' while rendering its rename record",
                    quote_pathname(new, true)
                ))
                .with_stable_code(StableErrorCode::IoReadFailed)
                .with_hint("re-run 'libra status' once the path is readable again"));
            }
        }
    };
    let sub = if is_submodule_mode(mode_index) || is_submodule_mode(mode_head) {
        get_submodule_status(new)
    } else {
        "N...".to_string()
    };

    write!(
        writer,
        "2 {x}{y} {} {} {} {} {} {} R{} ",
        sub,
        format_mode(mode_head),
        format_mode(mode_index),
        format_mode(mode_worktree),
        hash_head,
        hash_index,
        score,
    )
    .map_err(write_err)?;
    if null_terminated {
        write_raw_path(writer, new).map_err(write_err)?;
        writer.write_all(b"\0").map_err(write_err)?;
        write_raw_path(writer, old).map_err(write_err)?;
        writer.write_all(b"\0").map_err(write_err)?;
    } else {
        writer
            .write_all(&quote_pathname_bytes(new, quote_path))
            .map_err(write_err)?;
        writer.write_all(b"\t").map_err(write_err)?;
        writer
            .write_all(&quote_pathname_bytes(old, quote_path))
            .map_err(write_err)?;
        writer.write_all(b"\n").map_err(write_err)?;
    }
    Ok(())
}

/// Output porcelain v2 format using metadata collected during status computation.
#[allow(clippy::too_many_arguments)]
fn output_porcelain_v2(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
    intent_to_add: &[PathBuf],
    ignored: &[PathBuf],
    metadata: Option<&PorcelainV2Data>,
    staged_rename_details: &RenameDetails,
    unstaged_rename_details: &RenameDetails,
    null_terminated: bool,
    quote_path: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let metadata =
        metadata.ok_or_else(|| CliError::internal("missing porcelain v2 metadata for status"))?;
    let zero_hash = zero_hash_str();
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);

    for entry in unmerged {
        write_unmerged_porcelain_v2(entry, &zero_hash, null_terminated, quote_path, writer)?;
    }

    // Rename records (`2 …`) render separately from the flattened `1 …` list;
    // their endpoints are excluded below so they never double as change rows.
    let mut endpoints: HashSet<PathBuf> = HashSet::new();
    for (old, new) in &staged.renamed {
        endpoints.insert(old.clone());
        endpoints.insert(new.clone());
        // Worktree state of the NEW path rides in the second XY column
        // (`RM`/`RD`), mirroring Git — the endpoint row is suppressed.
        let unstaged_char = if unstaged.modified.contains(new) {
            'M'
        } else if unstaged.deleted.contains(new) {
            'D'
        } else {
            '.'
        };
        // A missing score is NOT 100: `R100` is the documented spelling of an
        // exact rename, so defaulting to it would publish an inexact pair as
        // byte-identical. The mode and hash fields already fail closed on
        // missing metadata; the score column gets the same treatment.
        let score = staged_rename_details
            .get(&(old.clone(), new.clone()))
            .map(|(pct, _)| *pct)
            .ok_or_else(|| {
                CliError::fatal(format!(
                    "cannot render the porcelain v2 rename record for '{}': score metadata is missing",
                    quote_pathname(new, quote_path)
                ))
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("re-run 'libra status' after 'libra add'/'libra reset' settles the index")
            })?;
        write_rename_porcelain_v2(
            old,
            new,
            'R',
            unstaged_char,
            score,
            metadata,
            &zero_hash,
            null_terminated,
            quote_path,
            writer,
        )?;
    }
    for (old, new) in &unstaged.renamed {
        endpoints.insert(old.clone());
        endpoints.insert(new.clone());
        let score = unstaged_rename_details
            .get(&(old.clone(), new.clone()))
            .map(|(pct, _)| *pct)
            .ok_or_else(|| {
                CliError::fatal(format!(
                    "cannot render the porcelain v2 rename record for '{}': score metadata is missing",
                    quote_pathname(new, quote_path)
                ))
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("re-run 'libra status' after 'libra add'/'libra reset' settles the index")
            })?;
        // The SOURCE of an unstaged rename may also carry a staged change
        // (edit `a`, `add a`, then move it to `b`). Git reports that as
        // `MR`, keeping the real HEAD and index sides distinct. Hardcoding
        // `.R` both lost the staged component — the endpoint row is
        // suppressed, so it vanished entirely — and made the record copy the
        // index hash into `hH`, claiming HEAD and index agree when they do
        // not.
        let staged_char = if staged.modified.contains(old) {
            'M'
        } else if staged.new.contains(old) {
            'A'
        } else {
            '.'
        };
        write_rename_porcelain_v2(
            old,
            new,
            staged_char,
            'R',
            score,
            metadata,
            &zero_hash,
            null_terminated,
            quote_path,
            writer,
        )?;
    }

    // An unresolved conflict lives ONLY in its `u` record: the
    // stage-0-less index also classifies the path as a staged deletion,
    // and without this exclusion the ordinary loop would emit a bogus
    // duplicate `1 D.` row for it (2026-08-06 R0-5 review).
    let unmerged_paths: std::collections::HashSet<&std::path::Path> =
        unmerged.iter().map(|entry| entry.path.as_path()).collect();
    let status_list =
        generate_short_format_status_with_unmerged(staged, unstaged, &[], intent_to_add);
    for (file, staged_status, unstaged_status) in status_list {
        if endpoints.contains(&file) {
            continue;
        }
        if unmerged_paths.contains(file.as_path()) {
            continue;
        }
        if staged_status == '?' && unstaged_status == '?' {
            if null_terminated {
                write!(writer, "? ").map_err(write_err)?;
                write_raw_path(writer, &file).map_err(write_err)?;
            } else {
                write!(writer, "? ").map_err(write_err)?;
                writer
                    .write_all(&quote_pathname_bytes(&file, quote_path))
                    .map_err(write_err)?;
            }
            if null_terminated {
                writer.write_all(b"\0").map_err(write_err)?;
            } else {
                writer.write_all(b"\n").map_err(write_err)?;
            }
            continue;
        }

        // The porcelain payload is ALREADY repository-root-relative
        // (`to_repo_relative` at the render entry). Re-projecting through
        // `current_to_workdir` here made every index/HEAD lookup miss from
        // a subdirectory and fall back to fabricated `100644`/zero-hash
        // metadata (2026-08-06 R0-5 review).
        let workdir_path = file.clone();
        let file_str = workdir_path.to_str().unwrap_or_default();

        // WT-06: Git treats an intent-to-add entry as ABSENT on the index
        // side of a porcelain v2 record (`1 .A N... 000000 000000 100644
        // <zero> <zero> path`), even though stage 0 holds the empty-blob
        // placeholder.
        let is_intent_to_add = intent_to_add.iter().any(|path| path == &file);
        let (mode_index, hash_index) = if is_intent_to_add {
            (0, zero_hash.clone())
        } else if let Some(entry) = metadata.index.get(file_str, 0) {
            (entry.mode, entry.hash.to_string())
        } else if staged_status == 'D' {
            // Semantically absent: the deletion is staged, so stage 0 has
            // no entry — Git v2 spells that `000000` plus the zero hash.
            (0, zero_hash.clone())
        } else {
            // Every other `1` row REQUIRES a stage-0 entry; fabricating
            // `100644` here would forge metadata for a lookup that must
            // not miss (2026-08-06 R0-5 review).
            return Err(CliError::fatal(format!(
                "missing index entry for '{file_str}' while rendering porcelain v2"
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid));
        };

        let (mode_head, hash_head) = if staged_status == 'A' {
            (0, zero_hash.clone())
        } else if let Some(info) = metadata.head_tree_items.get(&workdir_path) {
            (info.mode, info.hash.clone())
        } else {
            (0, zero_hash.clone())
        };

        let mode_worktree = if unstaged_status == 'D' {
            0
        } else {
            match get_worktree_mode_result_for_workdir(&workdir_path) {
                WorktreeMode::Mode(mode) => mode,
                // A path absent from the worktree (e.g. a staged deletion's
                // `D.` row) is semantically gone: `000000`, like Git.
                WorktreeMode::Gone => 0,
                // Unreadable is a different answer from gone and must never
                // be spelled `100644` (2026-08-06 R0-5 review, mirroring
                // the rename-record arm).
                WorktreeMode::Unreadable => {
                    return Err(CliError::fatal(format!(
                        "cannot read the worktree mode of '{file_str}' while rendering \
                         porcelain v2"
                    ))
                    .with_stable_code(StableErrorCode::IoReadFailed));
                }
            }
        };

        let sub = if is_submodule_mode(mode_index) || is_submodule_mode(mode_head) {
            get_submodule_status(&file)
        } else {
            "N...".to_string()
        };

        // Git porcelain v2 spells an unmodified side as `.`, never the
        // v1-style space — `1  M` instead of `1 .M` breaks fixed-column
        // consumers (2026-08-06 R0-5 review).
        let dot = |status: char| if status == ' ' { '.' } else { status };
        write!(
            writer,
            "1 {}{} {} {} {} {} {} {} ",
            dot(staged_status),
            dot(unstaged_status),
            sub,
            format_mode(mode_head),
            format_mode(mode_index),
            format_mode(mode_worktree),
            hash_head,
            hash_index,
        )
        .map_err(write_err)?;
        if null_terminated {
            // `1` rows always carry UTF-8 index paths today, but the `-z`
            // wire format is raw bytes by contract.
            write_raw_path(writer, &file).map_err(write_err)?;
            writer.write_all(b"\0").map_err(write_err)?;
        } else {
            writer
                .write_all(&quote_pathname_bytes(&file, quote_path))
                .map_err(write_err)?;
            writer.write_all(b"\n").map_err(write_err)?;
        }
    }

    for file in ignored {
        if null_terminated {
            write!(writer, "! ").map_err(write_err)?;
            write_raw_path(writer, file).map_err(write_err)?;
        } else {
            write!(writer, "! ").map_err(write_err)?;
            writer
                .write_all(&quote_pathname_bytes(file, quote_path))
                .map_err(write_err)?;
        }
        if null_terminated {
            writer.write_all(b"\0").map_err(write_err)?;
        } else {
            writer.write_all(b"\n").map_err(write_err)?;
        }
    }
    Ok(())
}

fn zero_hash_str() -> String {
    ObjectHash::zero_str(git_internal::hash::get_hash_kind())
}

fn write_unmerged_porcelain_v2(
    entry: &UnmergedEntry,
    zero_hash: &str,
    null_terminated: bool,
    quote_path: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);
    let (staged_status, unstaged_status) = entry.xy();
    let mode = |stage| {
        entry
            .stage(stage)
            .map(|stage| format_mode(stage.mode))
            .unwrap_or_else(|| "000000".to_string())
    };
    let hash = |stage| {
        entry
            .stage(stage)
            .map(|stage| stage.hash.to_string())
            .unwrap_or_else(|| zero_hash.to_string())
    };
    write!(
        writer,
        "u {}{} N... {} {} {} {} {} {} {} ",
        staged_status,
        unstaged_status,
        mode(1),
        mode(2),
        mode(3),
        format_mode(get_unmerged_worktree_mode(&entry.path)),
        hash(1),
        hash(2),
        hash(3)
    )
    .map_err(write_err)?;
    // §B.6.6: `-z` carries RAW path bytes (a non-UTF-8 name must survive
    // byte-for-byte), every other mode carries the C-style-escaped form.
    // `display()` did neither — it lossily replaced undecodable bytes and
    // left control characters unescaped, which can break the line format.
    if null_terminated {
        write_raw_path(writer, &entry.path).map_err(write_err)?;
        writer.write_all(b"\0").map_err(write_err)?;
    } else {
        writer
            .write_all(&quote_pathname_bytes(&entry.path, quote_path))
            .map_err(write_err)?;
        writer.write_all(b"\n").map_err(write_err)?;
    }
    Ok(())
}

/// `u`-row worktree mode; the unmerged payload is repository-root-relative
/// like the rest of the projected porcelain data (2026-08-06 R0-5 review:
/// same double-projection fix as `get_worktree_mode`).
fn get_unmerged_worktree_mode(workdir_path: &std::path::Path) -> u32 {
    let abs_path = util::workdir_to_absolute(workdir_path);
    if std::fs::symlink_metadata(&abs_path).is_ok() {
        get_worktree_mode(workdir_path)
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Short format
// ---------------------------------------------------------------------------

/// Core logic for generating short format status without color (for testing).
///
/// LEGACY tuple API (pre-R0 shape, §B.6.0.1): renames are DECOMPOSED into
/// their endpoint states — a staged rename contributes `old = D ` / `new =
/// A `, an unstaged rename contributes an unstaged `D` on its source (merged
/// with any staged state via the ordinary rules) and `??` on its
/// destination — so a chain `a→b` staged + `b→c` unstaged yields `a = D `,
/// `b = AD`, `c = ??` with no duplicate rows. Rename-aware consumers use
/// [`generate_short_status_entries`] instead.
pub fn generate_short_format_status(
    staged: &Changes,
    unstaged: &Changes,
) -> Vec<(std::path::PathBuf, char, char)> {
    generate_short_format_status_with_unmerged(staged, unstaged, &[], &[])
}

fn process_unstaged_changes(
    files: &[PathBuf],
    file_status: &mut HashMap<PathBuf, (char, char)>,
    unstaged_char: char,
) {
    for file in files {
        let staged_status = file_status.get(file).map(|(s, _)| *s);
        if let Some(status) = staged_status {
            file_status.insert(file.clone(), (status, unstaged_char));
        } else {
            file_status.insert(file.clone(), (' ', unstaged_char));
        }
    }
}

/// Shared base XY map: every non-rename change plus unmerged entries. Rename
/// pairs are handled by the caller (decomposed by the legacy API, first-class
/// in [`generate_short_status_entries`]).
fn short_xy_base(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
) -> HashMap<PathBuf, (char, char)> {
    let mut file_status: HashMap<PathBuf, (char, char)> = HashMap::new();

    for file in &staged.new {
        file_status.insert(file.clone(), ('A', ' '));
    }
    for file in &staged.modified {
        file_status.insert(file.clone(), ('M', ' '));
    }
    for file in &staged.deleted {
        file_status.insert(file.clone(), ('D', ' '));
    }

    process_unstaged_changes(&unstaged.modified, &mut file_status, 'M');
    process_unstaged_changes(&unstaged.deleted, &mut file_status, 'D');

    for file in &unstaged.new {
        file_status.insert(file.clone(), ('?', '?'));
    }
    for entry in unmerged {
        file_status.insert(entry.path.clone(), entry.xy());
    }
    file_status
}

pub(crate) fn generate_short_format_status_with_unmerged(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
    intent_to_add: &[PathBuf],
) -> Vec<(std::path::PathBuf, char, char)> {
    let mut file_status: HashMap<PathBuf, (char, char)> = HashMap::new();

    for file in &staged.new {
        file_status.insert(file.clone(), ('A', ' '));
    }
    for file in &staged.modified {
        file_status.insert(file.clone(), ('M', ' '));
    }
    for file in &staged.deleted {
        file_status.insert(file.clone(), ('D', ' '));
    }
    // Pre-R0 decomposition: a staged rename is a delete of the old path plus
    // an add of the new path in this legacy tuple view.
    for (old, new) in &staged.renamed {
        file_status.insert(old.clone(), ('D', ' '));
        file_status.insert(new.clone(), ('A', ' '));
    }

    process_unstaged_changes(&unstaged.modified, &mut file_status, 'M');
    process_unstaged_changes(&unstaged.deleted, &mut file_status, 'D');
    // Pre-R0 decomposition: an unstaged rename is an unstaged delete of its
    // source (merged with any staged state) …
    for (old, _new) in &unstaged.renamed {
        process_unstaged_changes(std::slice::from_ref(old), &mut file_status, 'D');
    }

    for file in &unstaged.new {
        file_status.insert(file.clone(), ('?', '?'));
    }
    // … and an untracked destination.
    for (_old, new) in &unstaged.renamed {
        file_status.insert(new.clone(), ('?', '?'));
    }
    // WT-06: intent-to-add entries are Git's `.A` worktree side. The placeholder
    // itself contributes no staged content, but an ITA path that HEAD still has
    // is a staged DELETION (`DA`, t2203:18) — preserve any existing X column.
    for file in intent_to_add {
        let staged_status = file_status
            .get(file)
            .map(|(staged, _)| *staged)
            .unwrap_or(' ');
        file_status.insert(file.clone(), (staged_status, 'A'));
    }
    for entry in unmerged {
        file_status.insert(entry.path.clone(), entry.xy());
    }

    let mut sorted_files: Vec<_> = file_status.iter().collect();
    // Git's short/porcelain v1 list is grouped: every TRACKED change (X or Y
    // is not `?`) sorted by path, then every untracked `??` row sorted by
    // path. A single global path sort interleaves `?? dir1/x` ahead of
    // `A  dir2/y`, which is not byte-compatible (t7508).
    sorted_files.sort_by(|a, b| {
        let untracked = |(staged, unstaged): (char, char)| staged == '?' && unstaged == '?';
        untracked(*a.1)
            .cmp(&untracked(*b.1))
            .then_with(|| a.0.cmp(b.0))
    });

    sorted_files
        .into_iter()
        .map(|(file, (staged_status, unstaged_status))| {
            (file.clone(), *staged_status, *unstaged_status)
        })
        .collect()
}

/// One short-format / porcelain-v1 render entry (§B.6.1 public API): either a
/// plain per-path change or a first-class rename pair (rendered with Git's
/// `old -> new` arrow instead of two endpoint rows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShortStatusEntry {
    Path {
        path: PathBuf,
        staged: char,
        unstaged: char,
    },
    Rename {
        old: PathBuf,
        new: PathBuf,
        staged: char,
        unstaged: char,
    },
}

impl ShortStatusEntry {
    /// Sort key — Git orders renames by their destination path.
    fn sort_key(&self) -> &Path {
        match self {
            ShortStatusEntry::Path { path, .. } => path,
            ShortStatusEntry::Rename { new, .. } => new,
        }
    }

    /// Whether this entry is the untracked `??` group. Git emits every
    /// tracked change first, then the sorted untracked rows (t7508).
    fn is_untracked(&self) -> bool {
        matches!(
            self,
            ShortStatusEntry::Path {
                staged: '?',
                unstaged: '?',
                ..
            }
        )
    }
}

/// Ordering key for short/porcelain rows: Git sorts by the repository-root
/// path even when the display paths are cwd-relative (`status` from a
/// subdirectory). `sort_base` is the cwd relative to the root; `None` means
/// the paths are already root-relative (porcelain, `status.relativePaths=false`)
/// or the caller wants the legacy display-path order (public API tests).
fn short_order_key(path: &Path, sort_base: Option<&Path>) -> PathBuf {
    match sort_base {
        Some(base) if !base.as_os_str().is_empty() => {
            crate::utils::util::normalize_abs_path(&base.join(path))
        }
        _ => path.to_path_buf(),
    }
}

/// Build the shared short-format / porcelain-v1 entry list (§B.6.1): rename
/// pairs stay first-class — the unstaged column of a staged rename carries
/// the DESTINATION's worktree state (`RM`/`RD`) — and every non-endpoint
/// path renders as an XY tuple. Entries sort by path, renames by their
/// destination.
pub fn generate_short_status_entries(
    staged: &Changes,
    unstaged: &Changes,
) -> Vec<ShortStatusEntry> {
    generate_short_status_entries_with_unmerged(staged, unstaged, &[], &[], None)
}

pub(crate) fn generate_short_status_entries_with_unmerged(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
    intent_to_add: &[PathBuf],
    sort_base: Option<&Path>,
) -> Vec<ShortStatusEntry> {
    let mut entries: Vec<ShortStatusEntry> = Vec::new();
    let mut endpoints: HashSet<PathBuf> = HashSet::new();
    for (old, new) in &staged.renamed {
        endpoints.insert(old.clone());
        endpoints.insert(new.clone());
        // The endpoint rows are suppressed, so this column is the only
        // signal for the destination's worktree state.
        let unstaged_char = if unstaged.modified.contains(new) {
            'M'
        } else if unstaged.deleted.contains(new) {
            'D'
        } else {
            ' '
        };
        entries.push(ShortStatusEntry::Rename {
            old: old.clone(),
            new: new.clone(),
            staged: 'R',
            unstaged: unstaged_char,
        });
    }
    for (old, new) in &unstaged.renamed {
        endpoints.insert(old.clone());
        endpoints.insert(new.clone());
        // The suppressed source row is the only carrier of the SOURCE's
        // staged state: a staged-modify→worktree-rename is `MR`, a
        // staged-add→worktree-rename `AR` — hard-coding a space here
        // erased that component while porcelain v2 derived it correctly
        // (2026-08-06 R0-6 review, mirroring the v2 derivation).
        let staged_char = if staged.modified.contains(old) {
            'M'
        } else if staged.new.contains(old) {
            'A'
        } else {
            ' '
        };
        entries.push(ShortStatusEntry::Rename {
            old: old.clone(),
            new: new.clone(),
            staged: staged_char,
            unstaged: 'R',
        });
    }

    let mut xy = short_xy_base(staged, unstaged, unmerged);
    // WT-06: `.A` for an intent-to-add path, merged with any staged component
    // the same path already has (an ITA path that HEAD tracks is `DA`).
    for path in intent_to_add {
        let staged_char = xy.get(path).map(|(staged, _)| *staged).unwrap_or(' ');
        xy.insert(path.clone(), (staged_char, 'A'));
    }
    let mut base: Vec<_> = xy.into_iter().collect();
    base.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, (staged_char, unstaged_char)) in base {
        if endpoints.contains(&path) {
            continue;
        }
        entries.push(ShortStatusEntry::Path {
            path,
            staged: staged_char,
            unstaged: unstaged_char,
        });
    }
    entries.sort_by(|a, b| {
        a.is_untracked().cmp(&b.is_untracked()).then_with(|| {
            short_order_key(a.sort_key(), sort_base).cmp(&short_order_key(b.sort_key(), sort_base))
        })
    });
    entries
}

/// Short format output — legacy public API used by tests.
pub async fn output_short_format(
    staged: &Changes,
    unstaged: &Changes,
    writer: &mut impl Write,
) -> CliResult<()> {
    output_short_format_with_config(
        staged,
        unstaged,
        &[],
        &[],
        None,
        &OutputConfig::default(),
        false,
        true,
        writer,
    )
    .await
}

/// C-style path quoting for human-short and non-`-z` porcelain output
/// (§B.6.6). Control characters, `"` and `\` are ALWAYS escaped; bytes above
/// 0x7F are additionally escaped as octal `\ooo` only under
/// `core.quotePath=true` (the default, matching Git). A path needing any
/// escape is wrapped in double quotes; `-z` output never calls this.
/// §B.6.0.1 reason taxonomy → JSON reason string + warning code.
pub(crate) fn io_blocked_reason_and_code(
    reason: crate::command::status_probe::IoBlockedReason,
) -> (&'static str, StatusWarningCode) {
    use crate::command::status_probe::IoBlockedReason;

    match reason {
        IoBlockedReason::PermissionDenied => (
            "permission_denied",
            StatusWarningCode::WorktreePermissionDenied,
        ),
        IoBlockedReason::IoError => ("io_error", StatusWarningCode::WorktreeReadFailed),
        IoBlockedReason::IoTimeout => ("io_timeout", StatusWarningCode::WorktreeIoTimeout),
    }
}

/// Write a path under `-z` as RAW OS bytes (Git parity: `-z` never quotes,
/// and on Unix a non-UTF-8 name keeps its exact bytes on the wire). Non-Unix
/// platforms fall back to the platform's stable `display()` encoding.
fn write_raw_path(writer: &mut impl Write, path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        writer.write_all(path.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        write!(writer, "{}", path.display())
    }
}

/// Public so the wave-0 suite can pin the §B.6.6 escape matrix on paths
/// (LF/CR) that cannot be created as files on every filesystem.
pub fn quote_pathname(path: &Path, quote_path: bool) -> String {
    let bytes = quote_pathname_bytes(path, quote_path);
    match String::from_utf8(bytes) {
        Ok(text) => text,
        // String-typed callers can never hold raw non-UTF-8 bytes, so they
        // keep the lossless octal-escaped form (every byte maps 1:1).
        // Byte-oriented writers call `quote_pathname_bytes` and stay raw
        // when `core.quotePath` is off (Git parity).
        Err(_) => {
            String::from_utf8(quote_pathname_bytes(path, true)).unwrap_or_else(|error| {
                // INVARIANT: the quote_path=true form octal-escapes every
                // byte >= 0x80, so it is pure ASCII and always valid UTF-8.
                String::from_utf8_lossy(error.as_bytes()).into_owned()
            })
        }
    }
}

/// Byte-faithful variant of [`quote_pathname`] for byte-oriented writers
/// (§B.6.6): TAB/LF/CR/`"`/`\` are always escaped; bytes >= 0x80 are
/// octal-escaped only while `quote_path` holds — with `core.quotePath=false`
/// they are written RAW, including when the path is not valid UTF-8 (Git
/// parity). `-z` surfaces never call this: they are raw end to end.
pub fn quote_pathname_bytes(path: &Path, quote_path: bool) -> Vec<u8> {
    // Escape the RAW OS path bytes (Unix), not a lossy `display()` copy, so
    // a non-UTF-8 name renders its true bytes (`\377`) instead of U+FFFD
    // replacement bytes. On non-Unix `display()` is the platform's stable
    // encoding.
    #[cfg(unix)]
    let bytes: &[u8] = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes()
    };
    #[cfg(not(unix))]
    let display = path.display().to_string();
    #[cfg(not(unix))]
    let bytes: &[u8] = display.as_bytes();
    let needs_escape =
        |b: u8| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || (quote_path && b >= 0x80);
    if !bytes.iter().copied().any(needs_escape) {
        return bytes.to_vec();
    }
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 8);
    out.push(b'"');
    for &b in bytes {
        match b {
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            _ if b < 0x20 || b == 0x7f || (quote_path && b >= 0x80) => {
                out.extend_from_slice(format!("\\{b:03o}").as_bytes());
            }
            _ => out.push(b),
        }
    }
    out.push(b'"');
    out
}

/// Short format output with color controlled by OutputConfig.
///
/// Renames are rendered as a single `R  <old> -> <new>` line (Git's short
/// rename form), not as two separate `R` rows (§B.6.1). Under `-z` the record
/// is `XY SP <new> NUL <old> NUL` (new before old, matching Git) with RAW
/// unquoted paths; non-`-z` paths go through [`quote_pathname`] (§B.6.6).
#[allow(clippy::too_many_arguments)]
async fn output_short_format_with_config(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
    intent_to_add: &[PathBuf],
    sort_base: Option<&Path>,
    output: &OutputConfig,
    null_terminated: bool,
    quote_path: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let use_colors = should_use_colors(output).await;
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);

    for entry in generate_short_status_entries_with_unmerged(
        staged,
        unstaged,
        unmerged,
        intent_to_add,
        sort_base,
    ) {
        match entry {
            ShortStatusEntry::Path {
                path,
                staged: x,
                unstaged: y,
            } => {
                if null_terminated {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    write_raw_path(writer, &path).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                } else if use_colors {
                    // Only the XY letters are colored; the path is appended
                    // byte-faithfully so `core.quotePath=false` keeps raw
                    // high bytes even under forced color.
                    let head = format_colored_status(x, y, "");
                    writer.write_all(head.as_bytes()).map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&path, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b"\n").map_err(write_err)?;
                } else {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&path, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b"\n").map_err(write_err)?;
                }
            }
            ShortStatusEntry::Rename {
                old,
                new,
                staged: x,
                unstaged: y,
            } => {
                if null_terminated {
                    // `XY SP <new> NUL <old> NUL` (§B.6.1), raw path bytes.
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    write_raw_path(writer, &new).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                    write_raw_path(writer, &old).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                } else if use_colors {
                    // Same byte-faithful shape as the Path arm: colored XY,
                    // then raw quoted bytes, then the arrow and the new path.
                    let head = format_colored_status(x, y, "");
                    writer.write_all(head.as_bytes()).map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&old, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b" -> ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&new, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b"\n").map_err(write_err)?;
                } else {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&old, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b" -> ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&new, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b"\n").map_err(write_err)?;
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Color control — unified with OutputConfig
// ---------------------------------------------------------------------------

/// Check if colors should be used, respecting OutputConfig overrides first,
/// then falling back to config-based / TTY detection.
async fn should_use_colors(output: &OutputConfig) -> bool {
    use std::io::IsTerminal;

    match output.color {
        ColorChoice::Never => return false,
        ColorChoice::Always => return true,
        ColorChoice::Auto => {}
    }

    // Auto: check git-style config, then TTY
    if let Some(color_setting) = ConfigKv::get("color.status.short")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
    {
        match color_setting.as_str() {
            "always" => return true,
            "never" | "false" => return false,
            "auto" | "true" => return io::stdout().is_terminal(),
            _ => return false,
        }
    }

    if let Some(color_setting) = ConfigKv::get("color.ui")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
    {
        match color_setting.as_str() {
            "always" => return true,
            "never" | "false" => return false,
            "auto" | "true" => return io::stdout().is_terminal(),
            _ => return false,
        }
    }

    io::stdout().is_terminal()
}

fn format_colored_status(staged_status: char, unstaged_status: char, file: &str) -> String {
    use colored::Colorize;

    let colored_staged = match staged_status {
        'A' => staged_status.to_string().green(),
        'M' => staged_status.to_string().green(),
        'D' => staged_status.to_string().red(),
        'R' => staged_status.to_string().yellow(),
        'C' => staged_status.to_string().yellow(),
        'U' => staged_status.to_string().red(),
        '?' => staged_status.to_string().bright_red(),
        ' ' => staged_status.to_string().into(),
        _ => staged_status.to_string().into(),
    };

    let colored_unstaged = match unstaged_status {
        'M' => unstaged_status.to_string().red(),
        'D' => unstaged_status.to_string().red(),
        'U' => unstaged_status.to_string().red(),
        '?' => unstaged_status.to_string().bright_red(),
        '!' => unstaged_status.to_string().bright_red(),
        ' ' => unstaged_status.to_string().into(),
        _ => unstaged_status.to_string().into(),
    };

    format!("{colored_staged}{colored_unstaged} {file}")
}

// ---------------------------------------------------------------------------
// Branch info helpers (short / porcelain)
// ---------------------------------------------------------------------------

/// Print branch info line for short / porcelain v1 `--branch`.
fn print_branch_info(
    head: &Head,
    upstream: Option<&UpstreamInfo>,
    show_ahead_behind: bool,
    null_terminated: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);
    match head {
        Head::Detached(commit_hash) => {
            let line = format!("## HEAD (detached at {})", &commit_hash.to_string()[..8]);
            if null_terminated {
                write!(writer, "{line}").map_err(write_err)?;
                writer.write_all(b"\0").map_err(write_err)?;
            } else {
                writeln!(writer, "{line}").map_err(write_err)?;
            }
        }
        Head::Branch(branch) => {
            let line = if let Some(u) = upstream {
                let tracking = format!("{}...{}", branch, u.remote_ref);
                if u.gone {
                    format!("## {tracking} [gone]")
                } else if show_ahead_behind {
                    match (u.ahead, u.behind) {
                        (Some(ahead), Some(behind)) if ahead > 0 && behind > 0 => {
                            format!("## {tracking} [ahead {ahead}, behind {behind}]")
                        }
                        (Some(ahead), Some(_)) if ahead > 0 => {
                            format!("## {tracking} [ahead {ahead}]")
                        }
                        (Some(_), Some(behind)) if behind > 0 => {
                            format!("## {tracking} [behind {behind}]")
                        }
                        // Up to date, or no counts (unborn branch, or counts
                        // that could not be computed).
                        _ => format!("## {tracking}"),
                    }
                } else {
                    format!("## {tracking}")
                }
            } else {
                format!("## {branch}")
            };
            if null_terminated {
                write!(writer, "{line}").map_err(write_err)?;
                writer.write_all(b"\0").map_err(write_err)?;
            } else {
                writeln!(writer, "{line}").map_err(write_err)?;
            }
        }
    }
    Ok(())
}

/// Write branch information in porcelain v2 style.
fn write_branch_info_v2(
    head: &Head,
    head_oid: Option<&ObjectHash>,
    upstream: Option<&UpstreamInfo>,
    show_ahead_behind: bool,
    null_terminated: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);
    let term = if null_terminated { b"\0" } else { b"\n" };

    match head {
        Head::Detached(_) => {
            write!(writer, "# branch.head (detached)").map_err(write_err)?;
            writer.write_all(term).map_err(write_err)?;
        }
        Head::Branch(name) => {
            write!(writer, "# branch.head {}", name).map_err(write_err)?;
            writer.write_all(term).map_err(write_err)?;
        }
    }

    if let Some(oid) = head_oid {
        write!(writer, "# branch.oid {oid}").map_err(write_err)?;
    } else {
        write!(writer, "# branch.oid (initial)").map_err(write_err)?;
    }
    writer.write_all(term).map_err(write_err)?;

    if let Some(u) = upstream {
        write!(writer, "# branch.upstream {}", u.remote_ref).map_err(write_err)?;
        writer.write_all(term).map_err(write_err)?;
        // Like Git, `# branch.ab` is only written when the counts exist: an
        // unborn branch or uncountable history omits the line rather than
        // claiming `+0 -0`.
        if !u.gone
            && show_ahead_behind
            && let (Some(ahead), Some(behind)) = (u.ahead, u.behind)
        {
            write!(writer, "# branch.ab +{ahead} -{behind}").map_err(write_err)?;
            writer.write_all(term).map_err(write_err)?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Upstream tracking resolution
// ---------------------------------------------------------------------------

// end status output section
