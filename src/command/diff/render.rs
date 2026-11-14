//! Diff output rendering: word diff, patch/stat summary, shortstat/stat and colorization.
//! These renderers consume the `DiffOutput` produced by the compare pipeline and never
//! re-scan or re-compare repository state.
#![allow(unused_imports)]
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    fmt::Write as _,
    io::{self, IsTerminal},
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use colored::Colorize;
use git_internal::{
    Diff,
    hash::{HashKind, ObjectHash, set_hash_kind},
    internal::{
        index::{Index, IndexEntry, Time},
        object::{
            ObjectTrait,
            blob::Blob,
            commit::Commit,
            tree::{Tree, TreeItemMode},
            types::ObjectType,
        },
        pack::utils::calculate_object_hash,
    },
};
use serde::Serialize;
use similar::{Algorithm, ChangeTag, TextDiff};
use tempfile::NamedTempFile;

use super::*;
use crate::utils::{output::emit_json_data, pager::Pager};
#[cfg(test)]
use crate::{
    command::{
        load_object, read_worktree_blob_bytes,
        unmerged::{self, UnmergedEntry},
    },
    internal::{config::ConfigKv, head::Head},
    utils::{
        attributes,
        client_storage::ClientStorage,
        error::{CliError, CliResult, StableErrorCode},
        ignore::{self, IgnorePolicy},
        output::{ColorChoice, OutputConfig, ProgressMode},
        path,
        pathspec::{PathspecError, PathspecSet},
        preview_object, util,
    },
};

pub(super) fn render_word_diff(
    old: &str,
    new: &str,
    mode: WordDiffMode,
    color: bool,
    regex: Option<&regex::Regex>,
) -> String {
    if let Some(regex) = regex {
        let owned_changes = regex_word_changes(old, new, regex);
        let changes = owned_changes
            .iter()
            .map(|(tag, text)| (*tag, text.as_str()))
            .collect::<Vec<_>>();
        render_word_changes(&changes, mode, color)
    } else {
        let old_toks = word_tokens(old);
        let new_toks = word_tokens(new);
        let diff = TextDiff::from_slices(&old_toks, &new_toks);
        let changes = normalize_word_changes(
            diff.iter_all_changes()
                .map(|change| (change.tag(), change.value()))
                .collect(),
        );
        render_word_changes(&changes, mode, color)
    }
}

pub(super) fn render_word_changes(
    changes: &[(ChangeTag, &str)],
    mode: WordDiffMode,
    color: bool,
) -> String {
    if mode == WordDiffMode::Porcelain {
        return render_word_porcelain(changes);
    }

    // Plain / color: emit a running line per output line; removed-word runs are
    // wrapped `[-…-]` and added runs `{+…+}` (or colored, bracket-less, when
    // `color`). A newline token closes any open marker and breaks the line.
    let mut out = String::new();
    let mut run: Vec<&str> = Vec::new();
    let mut run_tag = ChangeTag::Equal;
    let flush = |out: &mut String, run: &mut Vec<&str>, tag: ChangeTag| {
        if run.is_empty() {
            return;
        }
        let text = run.concat();
        match tag {
            ChangeTag::Equal => out.push_str(&text),
            ChangeTag::Delete => {
                if color {
                    out.push_str("\x1b[31m");
                    out.push_str(&text);
                    out.push_str("\x1b[0m");
                } else {
                    out.push_str("[-");
                    out.push_str(&text);
                    out.push_str("-]");
                }
            }
            ChangeTag::Insert => {
                if color {
                    out.push_str("\x1b[32m");
                    out.push_str(&text);
                    out.push_str("\x1b[0m");
                } else {
                    out.push_str("{+");
                    out.push_str(&text);
                    out.push_str("+}");
                }
            }
        }
        run.clear();
    };
    for &(tag, token) in changes {
        if token == "\n" {
            flush(&mut out, &mut run, run_tag);
            out.push('\n');
            continue;
        }
        if tag != run_tag {
            flush(&mut out, &mut run, run_tag);
            run_tag = tag;
        }
        run.push(token);
    }
    flush(&mut out, &mut run, run_tag);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

pub(super) fn render_word_porcelain(changes: &[(ChangeTag, &str)]) -> String {
    let mut out = String::new();
    let mut run: Vec<&str> = Vec::new();
    let mut run_tag = ChangeTag::Equal;
    let flush = |out: &mut String, run: &mut Vec<&str>, tag: ChangeTag| {
        if run.is_empty() {
            return;
        }
        let prefix = match tag {
            ChangeTag::Equal => ' ',
            ChangeTag::Delete => '-',
            ChangeTag::Insert => '+',
        };
        out.push(prefix);
        out.push_str(&run.concat());
        out.push('\n');
        run.clear();
    };
    for &(tag, token) in changes {
        if token == "\n" {
            flush(&mut out, &mut run, run_tag);
            out.push_str("~\n");
            continue;
        }
        if tag != run_tag {
            flush(&mut out, &mut run, run_tag);
            run_tag = tag;
        }
        run.push(token);
    }
    flush(&mut out, &mut run, run_tag);
    out
}

pub(super) fn render_diff_check(result: &DiffOutput) -> CliResult<bool> {
    let problems: Vec<String> = result
        .files
        .iter()
        .flat_map(|file| {
            check_whitespace_in_file(&file.path, &file.raw_diff, file.check_trailing_blank_start)
        })
        .collect();
    if problems.is_empty() {
        return Ok(false);
    }
    println!("{}", problems.join("\n"));
    Ok(true)
}

pub(super) fn render_diff_output(
    args: &DiffArgs,
    result: &DiffOutput,
    output: &OutputConfig,
) -> CliResult<()> {
    // Validate `--color-moved[=<mode>]` up front (even for non-colored paths, so a
    // bad mode is rejected like Git does at parse time).
    let color_moved = color_moved_active(args)?;
    // `--check` replaces the normal diff output with whitespace-error warnings.
    // Its status is 2 when it finds damage, and `--exit-code` independently
    // contributes 1 when there is any difference at all. Git adds the two, so a
    // damaged change asked with both is 3 and a clean change asked with both is
    // still 1 — returning early here would have thrown that second bit away.
    if args.check {
        let damaged = render_diff_check(result)?;
        let code =
            i32::from(args.exit_code && result.files_changed > 0) + if damaged { 2 } else { 0 };
        return if code == 0 {
            Ok(())
        } else {
            Err(CliError::silent_exit(code))
        };
    }
    // Plumbing `--quiet` promises an empty stdout, including under `--json`.
    // The global quiet flag cannot do this: `--machine` sets quiet and still
    // emits JSON.
    if args.plumbing_quiet {
        return if args.exit_code && result.files_changed > 0 {
            Err(CliError::silent_exit(1))
        } else {
            Ok(())
        };
    }
    if output.is_json() {
        emit_json_data("diff", result, output)?;
        // `--exit-code` applies regardless of output format: emit the JSON, then
        // signal differences via the process status.
        return diff_exit_result(args, result);
    }

    if output.quiet && args.output.is_none() {
        return if result.files_changed > 0 {
            Err(CliError::silent_exit(1))
        } else {
            Ok(())
        };
    }

    // --output writes are an explicit side-effect and must be honored even
    // when --quiet is set (quiet only suppresses stdout, not file writes).
    // `-z` NUL-terminates each record; `--name-status` then separates the
    // status and path with a NUL instead of a tab.
    let rendered = if args.raw && !args.plumbing_raw_with_patch {
        format_diff_raw(result, args.null, args.plumbing_raw)
    } else if args.name_only {
        join_diff_records(result.files.iter().map(|file| file.path.clone()), args.null)
    } else if args.name_status {
        let field_sep = if args.null { '\0' } else { '\t' };
        join_diff_records(
            result.files.iter().map(|file| {
                if file.status == "renamed" {
                    // `R<score>` then old + new paths (Git pads the score to 3 digits).
                    format!(
                        "R{:03}{sep}{}{sep}{}",
                        file.similarity.unwrap_or(0),
                        file.rename_from.as_deref().unwrap_or(""),
                        file.path,
                        sep = field_sep,
                    )
                } else {
                    format!("{}{}{}", diff_status_code(file), field_sep, file.path)
                }
            }),
            args.null,
        )
    } else if args.numstat {
        join_diff_records(
            result.files.iter().map(|file| {
                // Binary files report `-` for both counts (matching Git).
                let (ins, del) = if file.binary.is_some() {
                    ("-".to_string(), "-".to_string())
                } else {
                    (file.insertions.to_string(), file.deletions.to_string())
                };
                if file.status == "renamed" {
                    let from = file.rename_from.as_deref().unwrap_or("");
                    if args.null {
                        // `<ins>\t<del>\t\0<old>\0<new>` (empty path column, then NUL-separated).
                        format!("{ins}\t{del}\t\0{from}\0{}", file.path)
                    } else {
                        format!("{ins}\t{del}\t{}", rename_display(from, &file.path))
                    }
                } else {
                    format!("{ins}\t{del}\t{}", file.path)
                }
            }),
            args.null,
        )
    } else if args.stat || args.compact_summary {
        format_diff_stat_output_with_compact(result, args.compact_summary)
    } else if args.shortstat {
        format_diff_shortstat_output(result)
    } else if args.summary {
        format_diff_summary(result)
    } else if let Some(param) = &args.dirstat {
        format_diff_dirstat(result, param)?
    } else if args.no_patch {
        // `-s` / `--no-patch`: suppress the patch body (used for status-only
        // checks, typically with `--exit-code`).
        String::new()
    } else if result.external_diff_applied || result.binary_patch {
        // External-driver and `--binary` output is emitted verbatim — exact
        // concatenation, no trailing-newline normalization (a `GIT binary patch`
        // ends with a blank line that Git's parser requires), no coloring.
        result
            .files
            .iter()
            .map(|file| file.raw_diff.as_str())
            .collect()
    } else {
        format_unified_diff(result)
    };

    // Git's `diff -p --raw` prints the raw records, a blank line, then the
    // patch. Plumbing `-p --raw` requests the same combination.
    let rendered = if args.raw && args.plumbing_raw_with_patch {
        let raw = format_diff_raw(result, args.null, args.plumbing_raw);
        if raw.is_empty() {
            rendered
        } else if rendered.is_empty() {
            raw
        } else {
            format!("{raw}\n\n{rendered}")
        }
    } else {
        rendered
    };

    if let Some(path) = &args.output {
        std::fs::write(path, rendered.as_bytes())
            .map_err(|e| DiffError::OutputWrite {
                path: path.clone(),
                detail: e.to_string(),
            })
            .map_err(CliError::from)?;
        if output.quiet && result.files_changed > 0 {
            return Err(CliError::silent_exit(1));
        }
        return diff_exit_result(args, result);
    }

    if output.quiet {
        if result.files_changed > 0 {
            return Err(CliError::silent_exit(1));
        }
        return Ok(());
    }

    if rendered.is_empty() {
        return diff_exit_result(args, result);
    }
    let mut pager = Pager::with_config(output)?;
    let rendered = if args.name_only
        || args.name_status
        || args.numstat
        || args.stat
        || args.compact_summary
        || args.shortstat
        || args.summary
        || args.raw
        || word_diff_active(args)
        || result.external_diff_applied
        || result.binary_patch
    {
        rendered
    } else {
        // Honor `--color`: `always` forces color even when piped (the global
        // `colored` override is already set), `never` disables it, `auto` follows
        // the terminal. (Previously this only checked the terminal, so
        // `--color=always | pipe` produced no color — and no moved-line color.)
        let should_colorize = match output.color {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => io::stdout().is_terminal(),
        };
        maybe_colorize_diff(&rendered, should_colorize, color_moved)
    };
    // `-z` records carry their own NUL terminators, and external-driver output is
    // emitted byte-for-byte, so neither gets an appended trailing newline.
    let z_records = args.null && (args.name_only || args.name_status || args.numstat || args.raw);
    // The verbatim (no trailing-newline) write path applies only when the PATCH
    // body is actually rendered — `--binary --stat`/`--numstat` still get the
    // normal trailing newline even though `binary_patch` is set.
    let verbatim_patch =
        result.external_diff_applied || (result.binary_patch && patch_body_is_shown(args));
    if z_records || verbatim_patch {
        pager.write_str(&rendered)?;
    } else {
        pager.write_str(&format!("{rendered}\n"))?;
    }
    pager.finish()?;
    diff_exit_result(args, result)
}

/// Render `--dirstat`: a per-directory distribution of changes. Parameters are
/// `changes` (default; byte damage), `lines`, `files`, `noncumulative` (default),
/// `cumulative`, and a percentage cut-off (default 3%). The shared formatter in
/// `utils::diff_format` owns the Git-compatible thresholds and gathering so
/// `show`/`log` render the same output.
pub(crate) fn format_diff_dirstat(result: &DiffOutput, param: &str) -> CliResult<String> {
    let entries: Vec<crate::utils::diff_format::DirstatEntry<'_>> = result
        .files
        .iter()
        .map(|file| crate::utils::diff_format::DirstatEntry {
            name: file.path.as_str(),
            insertions: file.insertions,
            deletions: file.deletions,
            raw_diff: file.raw_diff.as_str(),
            binary: file.binary,
        })
        .collect();
    crate::utils::diff_format::format_dirstat(&entries, param).map_err(|message| {
        CliError::command_usage(format!("invalid --dirstat parameter: {message}"))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
    })
}

pub(super) fn format_diff_summary(result: &DiffOutput) -> String {
    result
        .files
        .iter()
        .filter_map(summary_line)
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn summary_line(file: &DiffFileStat) -> Option<String> {
    if file.status == "renamed" {
        let mut summary = format!(
            " rename {} ({}%)",
            rename_display(file.rename_from.as_deref().unwrap_or(""), &file.path),
            file.similarity.unwrap_or(0),
        );
        if let (Some(old), Some(new)) = (file.old_mode, file.new_mode)
            && old != new
        {
            summary.push_str(&format!(
                "\n mode change {old:06o} => {new:06o} {}",
                file.path
            ));
        }
        return Some(summary);
    }
    if let (None, Some(new)) = (file.old_mode, file.new_mode) {
        return Some(format!(" create mode {new:06o} {}", file.path));
    }
    if let (Some(old), None) = (file.old_mode, file.new_mode) {
        return Some(format!(" delete mode {old:06o} {}", file.path));
    }
    if let (Some(old), Some(new)) = (file.old_mode, file.new_mode)
        && old != new
    {
        return Some(format!(" mode change {old:06o} => {new:06o} {}", file.path));
    }
    let find = |prefix: &str| {
        file.raw_diff
            .lines()
            .find_map(|l| l.strip_prefix(prefix))
            .map(str::trim)
    };
    if let Some(mode) = find("new file mode ") {
        return Some(format!(" create mode {} {}", mode, file.path));
    }
    if let Some(mode) = find("deleted file mode ") {
        return Some(format!(" delete mode {} {}", mode, file.path));
    }
    None
}

pub(super) fn diff_status_code(file: &DiffFileStat) -> char {
    if file.raw_diff.starts_with("diff --cc ") {
        return 'U';
    }
    if file.status == "renamed" {
        return 'R';
    }
    if file.status == "added" {
        return 'A';
    }
    if file.status == "deleted" {
        return 'D';
    }
    if let (Some(old), Some(new)) = (file.old_mode, file.new_mode)
        && old & 0o170000 != new & 0o170000
    {
        return 'T';
    }
    'M'
}

pub(super) fn format_diff_raw(result: &DiffOutput, null: bool, full_ids: bool) -> String {
    let mut output = String::new();
    for file in &result.files {
        let status = diff_status_code(file);
        let status_field = if status == 'R' {
            format!("R{:03}", file.similarity.unwrap_or(0))
        } else {
            status.to_string()
        };
        let metadata = format!(
            ":{} {} {} {} {status_field}",
            raw_mode(file.old_mode),
            raw_mode(file.new_mode),
            raw_id(file.old_id, full_ids),
            raw_id(file.new_id, full_ids),
        );
        if null {
            output.push_str(&metadata);
            output.push('\0');
            if status == 'R' {
                output.push_str(file.rename_from.as_deref().unwrap_or(""));
                output.push('\0');
            }
            output.push_str(&file.path);
            output.push('\0');
        } else if status == 'R' {
            let _ = writeln!(
                output,
                "{metadata}\t{}\t{}",
                file.rename_from.as_deref().unwrap_or(""),
                file.path
            );
        } else {
            let _ = writeln!(output, "{metadata}\t{}", file.path);
        }
    }
    if !null {
        output.pop();
    }
    output
}

pub(super) fn rename_display(old: &str, new: &str) -> String {
    let oa = old.as_bytes();
    let nb = new.as_bytes();
    let mut pfx = 0;
    let mut i = 0;
    while i < oa.len() && i < nb.len() && oa[i] == nb[i] {
        if oa[i] == b'/' {
            pfx = i + 1;
        }
        i += 1;
    }
    let mut sfx = 0;
    let (mut oi, mut ni) = (oa.len(), nb.len());
    while oi > pfx && ni > pfx && oa[oi - 1] == nb[ni - 1] {
        oi -= 1;
        ni -= 1;
        if oa[oi] == b'/' {
            sfx = oa.len() - oi;
        }
    }
    if pfx == 0 && sfx == 0 {
        format!("{old} => {new}")
    } else {
        format!(
            "{}{{{} => {}}}{}",
            &old[..pfx],
            &old[pfx..oa.len() - sfx],
            &new[pfx..nb.len() - sfx],
            &old[oa.len() - sfx..],
        )
    }
}

/// A plumbing stat-invalid entry with unchanged content has no patch to show.
///
/// `--raw` and `--name-status` still report it (`M` with an all-zero worktree
/// id, Git's pessimistic plumbing contract), but Git emits **no** `-p` body for
/// it because the two sides are byte-identical. `read-tree` makes every entry
/// stat-invalid, so without this filter `diff-index -p` would print a bogus
/// `--- /dev/null` header for an empty tracked file (issues/479 DF-01, TC-0044).
pub(super) fn patch_has_no_content_change(file: &DiffFileStat) -> bool {
    if file.status != "modified" {
        return false;
    }
    let (Some(old), Some(new)) = (file.old_mode, file.new_mode) else {
        return false;
    };
    if old != new || file.binary.is_some() || file.raw_diff.starts_with("diff --cc ") {
        return false;
    }
    if file.raw_diff.contains("Binary files ") || file.raw_diff.contains("GIT binary patch") {
        return false;
    }
    if file.raw_diff.contains("\n@@ ") {
        return false;
    }
    // Only the plumbing stat-only shape qualifies: the engine renders the
    // unhashed worktree side as an all-zero object id (`index <old>..<zeros>`).
    // A header-only patch WITHOUT that marker (a `<LargeFile>` marker, a
    // stripped whitespace-only body, a gitlink change) is real output and must
    // keep flowing.
    let Some(index_line) = file
        .raw_diff
        .lines()
        .find(|line| line.starts_with("index "))
    else {
        return false;
    };
    let ids = index_line.trim_start_matches("index ");
    let Some((_, new_id)) = ids.split_once("..") else {
        return false;
    };
    let new_id = new_id.split_whitespace().next().unwrap_or_default();
    !new_id.is_empty() && new_id.chars().all(|c| c == '0')
}

pub(super) fn format_unified_diff(result: &DiffOutput) -> String {
    result
        .files
        .iter()
        .filter(|file| !patch_has_no_content_change(file))
        .map(|file| file.raw_diff.trim_end_matches('\n'))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn maybe_colorize_diff(
    diff_text: &str,
    should_colorize: bool,
    color_moved: bool,
) -> String {
    if should_colorize {
        colorize_diff(diff_text, color_moved)
    } else {
        diff_text.to_string()
    }
}

pub(super) fn format_diff_shortstat_output(result: &DiffOutput) -> String {
    if result.files.is_empty() {
        return String::new();
    }
    let mut line = format!(
        " {} file{} changed",
        result.files_changed,
        if result.files_changed == 1 { "" } else { "s" }
    );
    if result.total_insertions > 0 {
        line.push_str(&format!(
            ", {} insertion{}(+)",
            result.total_insertions,
            if result.total_insertions == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    if result.total_deletions > 0 {
        line.push_str(&format!(
            ", {} deletion{}(-)",
            result.total_deletions,
            if result.total_deletions == 1 { "" } else { "s" }
        ));
    }
    line
}

pub(super) fn format_diff_stat_output(result: &DiffOutput) -> String {
    format_diff_stat_output_with_compact(result, false)
}

pub(super) fn format_diff_stat_output_with_compact(result: &DiffOutput, compact: bool) -> String {
    if result.files.is_empty() {
        return String::new();
    }

    let mut lines = result
        .files
        .iter()
        .map(|file| {
            let mut name = if file.status == "renamed" {
                rename_display(file.rename_from.as_deref().unwrap_or(""), &file.path)
            } else {
                file.path.clone()
            };
            if compact && let Some(summary) = compact_summary_label(file) {
                name.push_str(" (");
                name.push_str(&summary);
                name.push(')');
            }
            // Binary files show `Bin <old> -> <new> bytes` instead of a graph; an
            // UNCHANGED binary (an exact rename, which keeps a header-only body
            // with no `Binary files`/`GIT binary patch`) shows a bare `Bin`,
            // matching Git.
            if let Some((old_size, new_size)) = file.binary {
                let changed = file.raw_diff.contains("Binary files ")
                    || file.raw_diff.contains("GIT binary patch");
                return if changed {
                    format!(" {name} | Bin {old_size} -> {new_size} bytes")
                } else {
                    format!(" {name} | Bin")
                };
            }
            let total = file.insertions + file.deletions;
            let bar = format!(
                "{}{}",
                "+".repeat(file.insertions.min(40)),
                "-".repeat(file.deletions.min(40))
            );
            // Git omits the trailing space when the change graph is empty
            // (e.g. a pure rename with 0 line changes shows `name | 0`).
            if bar.is_empty() {
                format!(" {} | {}", name, total)
            } else {
                format!(" {} | {} {}", name, total, bar)
            }
        })
        .collect::<Vec<_>>();
    lines.push(format!(
        " {} file{} changed, {} insertion{}(+), {} deletion{}(-)",
        result.files_changed,
        if result.files_changed == 1 { "" } else { "s" },
        result.total_insertions,
        if result.total_insertions == 1 {
            ""
        } else {
            "s"
        },
        result.total_deletions,
        if result.total_deletions == 1 { "" } else { "s" }
    ));
    lines.join("\n")
}

pub(super) fn compact_summary_label(file: &DiffFileStat) -> Option<String> {
    let mode_suffix = |mode: u32| {
        if mode & 0o170000 == 0o120000 {
            Some("+l")
        } else if mode & 0o111 != 0 {
            Some("+x")
        } else {
            None
        }
    };
    match (file.old_mode, file.new_mode) {
        (None, Some(new)) => Some(match mode_suffix(new) {
            Some(suffix) => format!("new {suffix}"),
            None => "new".to_string(),
        }),
        (Some(_), None) => Some("gone".to_string()),
        (Some(old), Some(new)) if old != new => {
            let old_link = old & 0o170000 == 0o120000;
            let new_link = new & 0o170000 == 0o120000;
            let old_exec = old & 0o111 != 0;
            let new_exec = new & 0o111 != 0;
            let mut changes = Vec::new();
            if old_link != new_link {
                changes.push(if new_link { "+l" } else { "-l" });
            }
            if old_exec != new_exec {
                changes.push(if new_exec { "+x" } else { "-x" });
            }
            (!changes.is_empty()).then(|| changes.join(" "))
        }
        _ => None,
    }
}

pub(super) fn colorize_diff(diff_text: &str, color_moved: bool) -> String {
    let mut output = String::with_capacity(diff_text.len() + 500);
    // For `--color-moved`, precompute which line bodies are moved (appear as both
    // a removed and an added line). Moved lines get a distinct color.
    let moved = if color_moved {
        moved_line_bodies(diff_text)
    } else {
        std::collections::HashSet::new()
    };

    // Track hunk state so `-`/`+` are only treated as removals/additions inside a
    // hunk — a body line like `---foo` is a removed `--foo`, not the `--- a/<path>`
    // file header (which precedes the first `@@`).
    let mut in_hunk = false;
    for line in diff_text.lines() {
        let colored_line = if line.starts_with("diff --git") {
            in_hunk = false;
            line.bold().to_string()
        } else if line.starts_with("@@") {
            in_hunk = true;
            line.cyan().to_string()
        } else if in_hunk && line.starts_with('-') {
            // A moved removed line → bold magenta (Git's `oldMoved`); else red.
            if color_moved && moved.contains(&line[1..]) {
                line.magenta().bold().to_string()
            } else {
                line.red().to_string()
            }
        } else if in_hunk && line.starts_with('+') {
            // A moved added line → bold cyan (Git's `newMoved`); else green.
            if color_moved && moved.contains(&line[1..]) {
                line.cyan().bold().to_string()
            } else {
                line.green().to_string()
            }
        } else {
            line.to_string()
        };

        output.push_str(&colored_line);
        output.push('\n');
    }
    output
}
