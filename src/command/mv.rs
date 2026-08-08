//! Implementation of `git mv` command, which moves/renames files and directories in the working directory and updates the index accordingly.
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use clap::Parser;
use git_internal::internal::index::Index;
use serde::Serialize;

use crate::{
    command::calc_file_blob_hash,
    utils::{
        error::{CliError, CliResult},
        output::{OutputConfig, emit_json_data},
        path, util,
    },
};

/// `--help` examples shown in `libra mv --help` output.
///
/// `mv` accepts `<source>... <destination>` with optional `--dry-run`,
/// `--force`, `--verbose`, `--skip-errors`, and `--sparse`. The banner
/// covers the rename, move-into-dir, multi-source, dry-run, force-overwrite,
/// skip-errors, sparse no-op, and JSON-for-agents forms so users can map
/// intent to invocation without reading the design doc.
/// Cross-cutting `--help` EXAMPLES rollout per
/// `docs/development/commands/_general.md` item B.
pub const MV_EXAMPLES: &str = "\
EXAMPLES:
    libra mv old.txt new.txt              Rename a single tracked file
    libra mv src/file.rs lib/             Move file into an existing directory
    libra mv a.txt b.txt subdir/          Move multiple files into a directory
    libra mv -n old.txt new.txt           Dry-run: preview the rename without touching the index
    libra mv -f stale.txt fresh.txt       Overwrite the destination if it already exists
    libra mv -k missing.txt tracked.txt dest/    Skip invalid sources and move the valid ones
    libra mv --sparse old.txt new.txt     Accept Git's sparse-checkout flag as a no-op
    libra mv -v old.txt new.txt           Verbose: print each move as it happens
    libra mv --json src/foo.rs src/bar.rs    Structured JSON output for agents";

#[derive(Parser, Debug)]
#[command(after_help = MV_EXAMPLES)]
pub struct MvArgs {
    /// Path list: one or more `<source>` paths followed by a `<destination>`. The `<destination>` is required and must be the last argument; it can be a file or a directory. When multiple `<source>` paths are given, `<destination>` must be an existing directory
    pub paths: Vec<String>,

    /// Enable verbose output.
    #[clap(short = 'v', long)]
    pub verbose: bool,

    /// Perform a dry run.
    #[clap(short = 'n', long)]
    pub dry_run: bool,

    /// Force move/rename even if the destination already exists (overwriting it)
    #[clap(short = 'f', long)]
    pub force: bool,

    /// Skip invalid sources and continue with the remaining valid moves.
    #[clap(short = 'k', long = "skip-errors")]
    pub skip_errors: bool,

    /// Accept Git's sparse-checkout flag. Libra has no sparse-checkout state, so this is a no-op.
    #[clap(long)]
    pub sparse: bool,
}

#[derive(Default)]
struct MovePlan {
    fs_moves: Vec<(PathBuf, PathBuf)>,
    index_updates: Vec<(PathBuf, PathBuf)>,
}

impl MovePlan {
    fn extend(&mut self, mut other: MovePlan) {
        self.fs_moves.append(&mut other.fs_moves);
        self.index_updates.append(&mut other.index_updates);
    }
}

#[derive(Debug, Serialize)]
struct MovePair {
    source: String,
    destination: String,
}

#[derive(Debug, Serialize)]
struct MvOutput {
    moves: Vec<MovePair>,
    index_updates: Vec<MovePair>,
    dry_run: bool,
    forced: bool,
    verbose: bool,
    /// Sources skipped under `-k`/`--skip-errors`, with the reason each was
    /// dropped. Empty (and omitted from JSON) when nothing was skipped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    skipped: Vec<MvSkipped>,
}

/// A source dropped by `-k`/`--skip-errors`, paired with why it was skipped.
#[derive(Debug, Serialize)]
struct MvSkipped {
    source: String,
    reason: String,
}

pub async fn execute(args: MvArgs) {
    if let Err(e) = execute_safe(args, &OutputConfig::default()).await {
        e.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting. Moves or renames files in the working directory and
/// updates the index accordingly.
pub async fn execute_safe(args: MvArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    let result = execute_inner(args, output)
        .await
        .map_err(CliError::from_legacy_string)?;
    if output.is_json() {
        emit_json_data("mv", &result, output)?;
    }
    Ok(())
}

async fn execute_inner(args: MvArgs, output: &OutputConfig) -> Result<MvOutput, String> {
    // If the user just types `git mv` without enough arguments, print usage information instead of an error message.
    if args.paths.len() < 2 {
        return Err(
            "usage: libra mv [<options>] <source>... <destination>\n\n-v, --verbose         be verbose\n-n, --dry-run         dry run\n-f, --force           force move/rename even if target exists\n-k, --skip-errors     skip invalid sources\n    --sparse          accept Git sparse-checkout flag as no-op"
                .to_string(),
        );
    }

    let paths: Vec<PathBuf> = args.paths.iter().map(PathBuf::from).collect();
    let sources: Vec<PathBuf> = paths[0..paths.len() - 1]
        .iter()
        .map(to_absolute_path)
        .collect();
    let destination = to_absolute_path(&paths[paths.len() - 1]);

    for src in &sources {
        validate_path_within_workdir(src)?;
    }
    validate_path_within_workdir(&destination)?;

    // Check if the destination is a directory (if it exists), which affects how we handle multiple sources.
    let destination_is_dir = destination.is_dir();
    // If there are multiple sources, the destination must be an existing directory.
    if sources.len() > 1 && !destination_is_dir {
        return Err(format!(
            "fatal: destination '{}' is not a directory",
            util::to_workdir_path(&destination).display()
        ));
    }

    // Check the validity of all sources and collect the valid move operations.
    let mut move_plan = MovePlan::default();
    let index_file = path::index();
    let mut index = match Index::load(&index_file) {
        Ok(index) => index,
        Err(err) => {
            return Err(format!("fatal: failed to load index: {err}"));
        }
    };
    let mut skipped: Vec<MvSkipped> = Vec::new();
    for src in &sources {
        match validate_source_and_collect_moves(
            src,
            &destination,
            destination_is_dir,
            &index,
            args.force,
        ) {
            Ok(plan) => {
                if args.skip_errors && plan_targets_existing_planned_destination(&move_plan, &plan)
                {
                    skipped.push(MvSkipped {
                        source: util::path_to_string(&util::to_workdir_path(src)),
                        reason: "destination is already targeted by an earlier source".to_string(),
                    });
                    continue;
                }
                move_plan.extend(plan);
            }
            Err(err) if args.skip_errors => {
                skipped.push(MvSkipped {
                    source: util::path_to_string(&util::to_workdir_path(src)),
                    reason: strip_fatal_prefix(&err),
                });
                continue;
            }
            Err(err) => {
                return Err(err);
            }
        }
    }

    if has_duplicate_target(&move_plan.fs_moves) {
        return Err(format!(
            "fatal: multiple sources moving to the same target path, source={}, destination={}",
            util::to_workdir_path(&sources[sources.len() - 1]).display(),
            util::to_workdir_path(&destination).display()
        ));
    }
    perform_moves(
        move_plan,
        args.verbose,
        args.dry_run,
        args.force,
        skipped,
        &mut index,
        output,
    )
}

/// Strip a leading `fatal: ` from a validation error so the reason reads cleanly
/// when it is reported as a skipped-source diagnostic rather than a fatal error.
fn strip_fatal_prefix(message: &str) -> String {
    message
        .strip_prefix("fatal: ")
        .unwrap_or(message)
        .to_string()
}
/// Validates a source path and builds the move plan.
///
/// Returns:
/// - `Ok(MovePlan)`: a move plan with move pairs.
///   - `fs_moves`: filesystem move pairs `(src_abs, dst_abs)`.
///   - `index_updates`: index update pairs `(src_abs, dst_abs)`.
///   - Both source and destination paths in pairs are absolute paths.
/// - `Err(String)`: a formatted fatal error message for invalid input or unsupported move.
fn validate_source_and_collect_moves(
    src: &Path,
    destination: &Path,
    destination_is_dir: bool,
    index: &Index,
    force: bool,
) -> Result<MovePlan, String> {
    if !src.exists() {
        return Err(format!(
            "fatal: bad source, source={}, destination={}",
            util::to_workdir_path(src).display(),
            util::to_workdir_path(destination).display()
        ));
    }

    if src == destination {
        return Err(format!(
            "fatal: can not move directory into itself, source={}, destination={}",
            util::to_workdir_path(src).display(),
            util::to_workdir_path(destination).display()
        ));
    }

    if src.is_dir() {
        // Case-only DIRECTORY rename on a case-insensitive FS: `mv Dir dir`
        // makes the destination stat resolve to the source itself — without
        // this check it would route into move-INTO-directory (`dir/Dir`).
        if destination_is_dir
            && crate::utils::path_case::is_case_only_pair(
                &util::path_to_string(&util::to_workdir_path(src)),
                &util::path_to_string(&util::to_workdir_path(destination)),
            )
            && crate::utils::path_case::same_file_entry(src, destination)
        {
            return plan_case_only_directory_rename(src, destination, index);
        }
        return validate_source_directory(src, destination, destination_is_dir, index);
    }

    validate_source_file(src, destination, destination_is_dir, index, force)
}
/// Validates a source directory and builds the directory move plan.
///
/// Returns:
/// - `Ok(MovePlan)`: directory move plan where each pair is `(src_abs, dst_abs)`.
/// - `Err(String)`: a formatted fatal error message.
fn validate_source_directory(
    src: &Path,
    destination: &Path,
    destination_is_dir: bool,
    index: &Index,
) -> Result<MovePlan, String> {
    // For directory move, we require the destination to be an existing directory
    if !destination_is_dir {
        return Err(format!(
            "fatal: destination '{}' is not a directory",
            util::to_workdir_path(destination).display()
        ));
    }

    let src_name = src.file_name().ok_or_else(|| {
        format!(
            "fatal: bad source, source={}, destination={}",
            util::to_workdir_path(src).display(),
            util::to_workdir_path(destination).display()
        )
    })?;

    if destination.starts_with(src) {
        return Err(format!(
            "fatal: can not move directory into itself, source={}, destination={}",
            util::to_workdir_path(src).display(),
            util::to_workdir_path(destination).display()
        ));
    }

    if destination.join(src_name).exists() {
        return Err(format!(
            "fatal: destination already exists, source={}, destination={}",
            util::to_workdir_path(src).display(),
            util::to_workdir_path(destination).display()
        ));
    }

    resolve_move_directory(src, destination, index)
}
/// Validates a source file and builds the file move plan.
///
/// Returns:
/// - `Ok(MovePlan)`: file move plan where each pair is `(src_abs, dst_abs)`.
/// - `Err(String)`: a formatted fatal error message.
fn validate_source_file(
    src: &Path,
    destination: &Path,
    destination_is_dir: bool,
    index: &Index,
    force: bool,
) -> Result<MovePlan, String> {
    if !index.tracked(&util::path_to_string(&util::to_workdir_path(src)), 0) {
        return Err(format!(
            "fatal: not under version control, source={}, destination={}",
            util::to_workdir_path(src).display(),
            util::to_workdir_path(destination).display()
        ));
    }

    if is_conflicted_in_index(index, src) {
        return Err(format!(
            "fatal: conflicted, source={}, destination={}",
            util::to_workdir_path(src).display(),
            util::to_workdir_path(destination).display()
        ));
    }

    let target = if destination_is_dir {
        let src_name = src.file_name().ok_or_else(|| {
            format!(
                "fatal: bad source, source={}, destination={}",
                util::to_workdir_path(src).display(),
                util::to_workdir_path(destination).display()
            )
        })?;
        destination.join(src_name)
    } else {
        destination.to_path_buf()
    };

    if let Ok(meta) = std::fs::symlink_metadata(&target) {
        // Case-only rename on a case-insensitive FS: the destination stat
        // resolves to the SOURCE itself (same inode, fold-equal path). This
        // is the DELIBERATE case-rename mechanism (lore.md 1.14) — allowed
        // without --force, and perform_moves must never remove_file it (the
        // old --force path deleted the source's own inode: data loss).
        let case_only = crate::utils::path_case::is_case_only_pair(
            &util::path_to_string(&util::to_workdir_path(src)),
            &util::path_to_string(&util::to_workdir_path(&target)),
        ) && crate::utils::path_case::same_file_entry(src, &target);
        if !case_only {
            if !force {
                return Err(format!(
                    "fatal: destination already exists, source={}, destination={}",
                    util::to_workdir_path(src).display(),
                    util::to_workdir_path(&target).display()
                ));
            }
            let file_type = meta.file_type();
            if !(file_type.is_file() || file_type.is_symlink()) {
                return Err(format!(
                    "fatal: cannot overwrite, source={}, destination={}",
                    util::to_workdir_path(src).display(),
                    util::to_workdir_path(&target).display()
                ));
            }
        }
    }

    Ok(MovePlan {
        fs_moves: vec![(src.to_path_buf(), target.clone())],
        index_updates: vec![(src.to_path_buf(), target)],
    })
}

fn to_absolute_path(path: impl AsRef<Path>) -> PathBuf {
    let workdir_relative = util::to_workdir_path(path.as_ref());
    util::workdir_to_absolute(workdir_relative)
}

fn validate_path_within_workdir(path: &Path) -> Result<(), String> {
    let workdir = util::working_dir();
    if !util::is_sub_path(path, &workdir) {
        return Err(format!(
            "fatal: '{}' is outside of the repository at '{}'",
            path.display(),
            workdir.display()
        ));
    }
    Ok(())
}

/// Builds a move plan for a directory source.
/// - Moves the whole directory in the filesystem (tracked + untracked + empty dirs).
/// - Updates the index only for tracked files under the source directory.
/// - Untracked files are moved with the directory rename and are not added to the index.
///
/// Returns:
/// - `Ok(MovePlan)`: move plan with absolute-path pairs `(src_abs, dst_abs)`.
/// - `Err(String)`: a formatted fatal error message.
fn resolve_move_directory(src: &Path, dst: &Path, index: &Index) -> Result<MovePlan, String> {
    let src_name = src.file_name().ok_or_else(|| {
        format!(
            "fatal: bad source, source={}, destination={}",
            util::to_workdir_path(src).display(),
            util::to_workdir_path(dst).display()
        )
    })?;
    let target_dir = dst.join(src_name);

    let files = util::list_files(src).map_err(|err| {
        format!(
            "fatal: failed to list source directory, source={}, destination={}, error={}",
            util::to_workdir_path(src).display(),
            util::to_workdir_path(dst).display(),
            err
        )
    })?;

    let tracked_updates: Vec<(PathBuf, PathBuf)> = files
        .into_iter()
        .filter(|file| index.tracked(&util::path_to_string(file), 0))
        .map(|file| {
            let relative_path = util::to_relative(&file, src);
            (
                util::workdir_to_absolute(&file),
                util::workdir_to_absolute(target_dir.join(relative_path)),
            )
        })
        .collect();

    Ok(MovePlan {
        fs_moves: vec![(src.to_path_buf(), target_dir)],
        index_updates: tracked_updates,
    })
}

/// Checks whether the source file is conflicted in the index.
fn is_conflicted_in_index(index: &Index, src: &Path) -> bool {
    let src_str = util::path_to_string(&util::to_workdir_path(src));
    (1..=3).any(|stage| index.tracked(&src_str, stage))
}
/// Checks whether multiple move operations target the same destination path.
/// Case-only directory rename plan: one fs rename `Dir` → `dir`, with every
/// tracked entry under the old prefix rekeyed to the new prefix.
fn plan_case_only_directory_rename(
    src: &Path,
    destination: &Path,
    index: &Index,
) -> Result<MovePlan, String> {
    let src_rel = util::to_workdir_path(src);
    let dst_rel = util::to_workdir_path(destination);
    let mut index_updates = Vec::new();
    for tracked in index.tracked_files() {
        if let Ok(suffix) = tracked.strip_prefix(&src_rel) {
            index_updates.push((
                util::workdir_to_absolute(&tracked),
                util::workdir_to_absolute(dst_rel.join(suffix)),
            ));
        }
    }
    if index_updates.is_empty() {
        return Err(format!(
            "fatal: not under version control, source={}, destination={}",
            src_rel.display(),
            dst_rel.display()
        ));
    }
    Ok(MovePlan {
        fs_moves: vec![(src.to_path_buf(), destination.to_path_buf())],
        index_updates,
    })
}

fn has_duplicate_target(moves: &[(PathBuf, PathBuf)]) -> bool {
    let mut target_paths = HashSet::new();
    for (_, target) in moves {
        if !target_paths.insert(target.clone()) {
            return true;
        }
    }
    false
}

fn plan_targets_existing_planned_destination(existing: &MovePlan, candidate: &MovePlan) -> bool {
    candidate.fs_moves.iter().any(|(_, candidate_target)| {
        existing
            .fs_moves
            .iter()
            .any(|(_, existing_target)| existing_target == candidate_target)
    })
}

fn remove_index_entry_all_stages(index: &mut Index, path: &str) {
    for stage in 0..=3 {
        let _ = index.remove(path, stage);
    }
}

fn perform_moves(
    plan: MovePlan,
    verbose: bool,
    dry_run: bool,
    force: bool,
    skipped: Vec<MvSkipped>,
    index: &mut Index,
    output: &OutputConfig,
) -> Result<MvOutput, String> {
    let mut moved_count = 0usize;

    // `-k`/`--skip-errors` stays silent on stderr in human mode (matching Git's
    // `mv -k`); the dropped sources are surfaced only via `MvOutput.skipped` in
    // the structured `--json`/`--machine` output.
    let output_result = MvOutput {
        moves: move_pairs_for_output(&plan.fs_moves),
        index_updates: move_pairs_for_output(&plan.index_updates),
        dry_run,
        forced: force,
        verbose,
        skipped,
    };

    for (src, dst) in &plan.fs_moves {
        let src_workdir = util::to_workdir_path(src);
        let dst_workdir = util::to_workdir_path(dst);

        // If it's a dry run, we just print the move operations without performing them.
        if dry_run {
            if !output.is_json() && !output.quiet {
                println!(
                    "Checking rename of '{}' to '{}'",
                    src_workdir.display(),
                    dst_workdir.display()
                );
                println!(
                    "Renaming {} to {}",
                    src_workdir.display(),
                    dst_workdir.display()
                );
            }
            continue;
        }
        // For actual move, we first check if the parent directory of the destination exists, if not, we try to create it.
        if let Some(parent) = dst.parent()
            && let Err(err) = std::fs::create_dir_all(parent)
        {
            return Err(format!(
                "fatal: failed to create destination directory, source={}, destination={}, error={}",
                src_workdir.display(),
                dst_workdir.display(),
                err
            ));
        }

        // Case-only pair (fold-equal path, same inode): the destination IS
        // the source — force-removing it would delete the only copy.
        let case_only = crate::utils::path_case::is_case_only_pair(
            &util::path_to_string(&src_workdir),
            &util::path_to_string(&dst_workdir),
        ) && crate::utils::path_case::same_file_entry(src, dst);
        if force
            && !case_only
            && let Ok(meta) = std::fs::symlink_metadata(dst)
        {
            let file_type = meta.file_type();
            if (file_type.is_file() || file_type.is_symlink())
                && let Err(err) = std::fs::remove_file(dst)
            {
                return Err(format!(
                    "fatal: failed to remove destination before force move, source={}, destination={}, error={}",
                    src_workdir.display(),
                    dst_workdir.display(),
                    err
                ));
            }
        }

        // Perform the move operation in the filesystem. A direct rename is
        // an atomic in-place case change on APFS/NTFS (Git relies on this);
        // fall back to a two-step rename through a temp name only when the
        // direct rename fails for a case-only pair.
        if let Err(e) = std::fs::rename(src, dst) {
            let mut renamed = false;
            if case_only {
                // Collision-free temp name in the same directory: rename(2)
                // REPLACES an existing destination, so a predictable temp
                // path could destroy an unrelated file. Probe candidates
                // until one is genuinely absent; give up (source untouched)
                // rather than ever overwriting.
                let mut tmp = None;
                for attempt in 0..64u32 {
                    let candidate = dst
                        .with_file_name(format!(".libra-mv-{}-{attempt}-tmp", std::process::id()));
                    if std::fs::symlink_metadata(&candidate).is_err() {
                        tmp = Some(candidate);
                        break;
                    }
                }
                if let Some(tmp) = tmp
                    && std::fs::rename(src, &tmp).is_ok()
                {
                    match std::fs::rename(&tmp, dst) {
                        Ok(()) => renamed = true,
                        Err(step2) => {
                            // Restore rather than leaving the temp name; if
                            // even the restore fails, say EXACTLY where the
                            // file is — never fail silently with a strand.
                            if let Err(restore) = std::fs::rename(&tmp, src) {
                                return Err(format!(
                                    "fatal: case rename failed mid-way and the file could not \
                                     be restored: it is at '{}' (intended '{}'); step error={}, \
                                     restore error={}",
                                    tmp.display(),
                                    dst_workdir.display(),
                                    step2,
                                    restore
                                ));
                            }
                        }
                    }
                }
            }
            if !renamed {
                return Err(format!(
                    "fatal: failed to move, source={}, destination={}, error={}",
                    src_workdir.display(),
                    dst_workdir.display(),
                    e
                ));
            }
        }

        moved_count += 1;

        // Print the move operation if verbose is enabled.
        if verbose && !output.is_json() && !output.quiet {
            println!(
                "Renaming {} to {}",
                src_workdir.display(),
                dst_workdir.display()
            );
        }
    }

    if dry_run {
        return Ok(output_result);
    }

    // Update index only after all filesystem moves succeeded.
    for (src, dst) in &plan.index_updates {
        let src_rel = util::path_to_string(&util::to_workdir_path(src));
        let dst_workdir = util::to_workdir_path(dst);
        let dst_rel = util::path_to_string(&dst_workdir);

        remove_index_entry_all_stages(index, &dst_rel);

        if index.remove(&src_rel, 0).is_some() {
            // Stat BEFORE hashing the moved file — see verified_index_entry
            // (2026-08-06 R0-8 review).
            let pre_read = std::fs::symlink_metadata(dst).ok();
            let new_entry = calc_file_blob_hash(dst)
                .map_err(|err| {
                    format!(
                        "failed to calculate hash for moved file, source={}, destination={}, error={}",
                        src_rel, dst_rel, err
                    )
                })
                .and_then(|hash| {
                    crate::command::verified_index_entry(
                        &dst_workdir,
                        hash,
                        &util::working_dir(),
                        pre_read.as_ref(),
                    )
                    .map_err(|err| {
                        format!(
                            "failed to build index entry for moved file, source={}, destination={}, error={}",
                            src_rel, dst_rel, err
                        )
                    })
                });

            match new_entry {
                Ok(entry) => index.add(entry),
                Err(err) => {
                    return Err(format!("fatal: {err}"));
                }
            }
        }
    }

    // After performing all moves, save the index if there were any moves.
    if moved_count > 0
        && let Err(e) = index.save(path::index())
    {
        return Err(format!("fatal: failed to save index after mv: {e}"));
    }

    Ok(output_result)
}

fn move_pairs_for_output(pairs: &[(PathBuf, PathBuf)]) -> Vec<MovePair> {
    pairs
        .iter()
        .map(|(source, destination)| MovePair {
            source: util::to_workdir_path(source).display().to_string(),
            destination: util::to_workdir_path(destination).display().to_string(),
        })
        .collect()
}
