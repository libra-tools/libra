//! `libra merge-file` — file-level three-way merge, a focused subset of
//! `git merge-file`. Merges `<current>` and `<other>` relative to their common
//! ancestor `<base>`, reusing the same `diffy` three-way merge that `merge` uses
//! for blob contents, so conflict markers are identical (`<<<<<<< ours` /
//! `======= ` / `>>>>>>> theirs`, plus `||||||| original` with `--diff3`).
//!
//! This does NOT touch the branch merge sequencer — it is a standalone text
//! merge over three files on disk.

use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
};

use clap::Parser;
use diffy::{ConflictStyle, MergeOptions};
use serde::Serialize;

use crate::utils::{
    error::{CliError, CliResult, StableErrorCode},
    output::{OutputConfig, emit_json_data},
    util,
};

/// `--help` examples (cross-cutting EXAMPLES contract, `_general.md`).
pub const MERGE_FILE_EXAMPLES: &str = "\
EXAMPLES:
    libra merge-file -p ours.txt base.txt theirs.txt   Print the merged result
    libra merge-file ours.txt base.txt theirs.txt      Merge in place into ours.txt
    libra merge-file --diff3 -p a b c                  Include the base in conflict markers
    libra --json merge-file -p a b c                   Structured { conflict, written }";

/// Three-way merge `<current>` and `<other>` relative to `<base>`.
#[derive(Parser, Debug)]
#[command(after_help = MERGE_FILE_EXAMPLES)]
pub struct MergeFileArgs {
    /// Send the merged result to stdout instead of overwriting `<current>`.
    #[clap(short = 'p', long = "stdout")]
    pub stdout: bool,

    /// Use diff3-style conflict markers (include the `<base>` section).
    #[clap(long = "diff3")]
    pub diff3: bool,

    /// Do not warn about conflicts on stderr.
    #[clap(short = 'q', long = "quiet")]
    pub quiet: bool,

    /// The "current"/ours file (overwritten with the result unless `-p`).
    #[clap(value_name = "CURRENT")]
    pub current: String,

    /// The common ancestor / base file.
    #[clap(value_name = "BASE")]
    pub base: String,

    /// The "other"/theirs file.
    #[clap(value_name = "OTHER")]
    pub other: String,
}

#[derive(Debug, Serialize)]
struct MergeFileOutput {
    /// Whether the merge produced conflict markers.
    conflict: bool,
    /// `true` if the result was written to `<current>`, `false` for `-p`.
    written: bool,
    /// The merged text, included only for `-p` (carried here so JSON mode does
    /// not mix raw output with the envelope on stdout).
    #[serde(skip_serializing_if = "Option::is_none")]
    merged: Option<String>,
}

pub async fn execute(args: MergeFileArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

/// Safe entry point. Exit codes follow `git merge-file`: 0 on a clean merge,
/// 1 on conflicts (Libra fixes this at 1 regardless of conflict count, per the
/// grit-gap plan), and 128 on error (missing/unreadable/binary inputs).
pub async fn execute_safe(args: MergeFileArgs, output: &OutputConfig) -> CliResult<()> {
    let current = read_input(&args.current)?;
    let base = read_input(&args.base)?;
    let other = read_input(&args.other)?;

    // Binary detection: refuse if any side contains a NUL byte (matching Git).
    for (label, bytes) in [
        (&args.current, &current),
        (&args.base, &base),
        (&args.other, &other),
    ] {
        if bytes.contains(&0) {
            return Err(
                CliError::fatal(format!("cannot merge binary files: {label}"))
                    .with_exit_code(128)
                    .with_stable_code(StableErrorCode::Unsupported),
            );
        }
    }

    let mut options = MergeOptions::new();
    if args.diff3 {
        options.set_conflict_style(ConflictStyle::Diff3);
    }
    let (merged, conflict) = match options.merge_bytes(&base, &current, &other) {
        Ok(clean) => (clean, false),
        Err(conflicted) => (conflicted, true),
    };

    let written = !args.stdout;
    // In JSON mode the merged text travels inside the envelope (below) so we
    // never mix a raw dump with the JSON document on stdout.
    let mut merged_for_json = None;
    if args.stdout {
        if output.is_json() {
            merged_for_json = Some(String::from_utf8_lossy(&merged).into_owned());
        } else {
            io::stdout().write_all(&merged).map_err(|error| {
                CliError::fatal(format!("failed to write merged output: {error}"))
                    .with_exit_code(128)
                    .with_stable_code(StableErrorCode::IoWriteFailed)
            })?;
            if conflict && !args.quiet {
                eprintln!("warning: conflicts during merge");
            }
        }
    } else {
        write_with_backup(&args.current, &current, &merged, conflict, args.quiet)?;
    }

    if output.is_json() {
        emit_json_data(
            "merge-file",
            &MergeFileOutput {
                conflict,
                written,
                merged: merged_for_json,
            },
            output,
        )?;
    }

    if conflict {
        // Conflicts are not an error: the merged result (with markers) has
        // already been produced. Signal them with a silent exit 1.
        return Err(CliError::silent_exit(1));
    }
    Ok(())
}

/// Read an input file, mapping any IO error to a 128 exit.
fn read_input(path: &str) -> CliResult<Vec<u8>> {
    fs::read(path).map_err(|error| {
        CliError::fatal(format!("cannot read '{path}': {error}"))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::IoReadFailed)
    })
}

/// Overwrite `<current>` with the merged result, backing up the original first
/// (under `.libra/merge-file-backup/` when inside a repository). The backup is
/// removed on a clean merge and kept (with a note) when conflicts remain.
fn write_with_backup(
    current_path: &str,
    original: &[u8],
    merged: &[u8],
    conflict: bool,
    quiet: bool,
) -> CliResult<()> {
    let write_err = |error: io::Error| {
        CliError::fatal(format!(
            "failed to write merged result to '{current_path}': {error}"
        ))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::IoWriteFailed)
    };

    let backup = backup_path(current_path);
    if let Some(backup_path) = &backup {
        if let Some(parent) = backup_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                CliError::fatal(format!(
                    "failed to create merge-file backup directory: {error}"
                ))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::IoWriteFailed)
            })?;
        }
        fs::write(backup_path, original).map_err(|error| {
            CliError::fatal(format!("failed to back up '{current_path}': {error}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
    }

    fs::write(current_path, merged).map_err(write_err)?;

    match (&backup, conflict) {
        (Some(backup_path), false) => {
            // Clean merge: the backup is no longer needed.
            let _ = fs::remove_file(backup_path);
        }
        (Some(backup_path), true) if !quiet => {
            eprintln!(
                "warning: conflicts during merge of '{current_path}'; original backed up at {}",
                backup_path.display()
            );
        }
        (None, true) if !quiet => {
            eprintln!("warning: conflicts during merge of '{current_path}'");
        }
        _ => {}
    }
    Ok(())
}

/// The backup path for `<current>` under the WORKTREE-LOCAL gitdir's
/// `merge-file-backup/` (W2 §C.4.3), or `None` when not inside a repository
/// (the merge still proceeds, without a backup). Worktree-local because two
/// worktrees merging same-named files must not overwrite or clean up each
/// other's backups — the file being backed up belongs to THIS worktree.
fn backup_path(current_path: &str) -> Option<PathBuf> {
    let gitdir = util::try_get_worktree_gitdir(None).ok()?;
    let sanitized = current_path.replace(['/', '\\'], "_");
    Some(gitdir.join("merge-file-backup").join(sanitized))
}
