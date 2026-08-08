//! Implements `clean` to remove untracked files from the working tree.

use std::{
    fs,
    path::{Path, PathBuf},
};

use clap::Parser;
use git_internal::internal::index::Index;
use serde::Serialize;

use crate::utils::{
    error::{CliError, CliResult, StableErrorCode},
    ignore::{self, IgnorePolicy},
    output::{OutputConfig, emit_json_data},
    path,
    pathspec::PathspecSet,
    util, worktree,
};

const CLEAN_EXAMPLES: &str = "\
EXAMPLES:
    libra clean -n                      Preview what would be removed (dry-run)
    libra clean -f                      Remove untracked files (files only)
    libra clean -fd                     Also remove untracked directories
    libra clean -fx                     Remove untracked files including ignored ones
    libra clean -fX                     Remove only ignored files
    libra clean -f -e '*.log'           Exclude a pattern (-e is short for --exclude)
    libra clean -f untracked.txt        Remove only files matching the pathspec
    libra clean -fd build/              Remove untracked paths under a directory pathspec
    libra clean -n --json               Structured JSON output for agents";

#[derive(Parser, Debug, Clone)]
#[command(after_help = CLEAN_EXAMPLES)]
pub struct CleanArgs {
    /// Show what would be removed without actually removing
    #[clap(short = 'n', long)]
    pub dry_run: bool,
    /// Force removal of untracked files
    #[clap(short, long)]
    pub force: bool,
    /// Remove untracked directories in addition to untracked files
    #[clap(short = 'd', long = "dir")]
    pub directories: bool,
    /// Remove all untracked files, including those in .gitignore/.libraignore
    #[clap(short = 'x')]
    pub ignored: bool,
    /// Remove only untracked files that are in .gitignore/.libraignore
    #[clap(short = 'X')]
    pub only_ignored: bool,
    /// Exclude files matching the given pattern (can be repeated)
    #[clap(short = 'e', long = "exclude", value_name = "pattern")]
    pub exclude: Vec<String>,
    /// Limit cleaning to paths matching the given pathspecs (shared engine:
    /// glob, `:(exclude)`, `:(top)`, `:(icase)`, `:(literal)`, `:(glob)`,
    /// subdirectory-relative semantics — same matcher as ls-files/status)
    #[clap(value_name = "pathspec")]
    pub pathspec: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct CleanOutput {
    dry_run: bool,
    removed: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
enum CleanError {
    #[error("clean requires -f or -n (use -f to remove files, -n to dry-run)")]
    MissingMode,
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    #[error("failed to load index: {0}")]
    LoadIndex(String),
    #[error("{0}")]
    ScanUntracked(String),
    #[error("failed to resolve working directory: {0}")]
    ResolveWorkdir(String),
    #[error("failed to resolve path {path}: {detail}")]
    ResolvePath { path: String, detail: String },
    #[error("refusing to remove path outside workdir: {0}")]
    OutsideWorkdir(String),
    #[error("failed to remove {path}: {detail}")]
    RemoveFile { path: String, detail: String },
}

pub async fn execute(args: CleanArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting.
///
/// # Side Effects
/// - Scans the working tree for untracked files.
/// - Removes matching files unless `--dry-run` is active.
/// - Renders removed or would-remove paths in human or JSON form.
///
/// # Errors
/// Returns [`CliError`] when the command is run outside a repository, candidate
/// paths cannot be resolved safely, a path escapes the worktree, or removal
/// fails.
pub async fn execute_safe(args: CleanArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    // PD-07: compile <pathspec>... through the shared engine (same matcher
    // semantics as ls-files/status, add/rm parity for write commands). The
    // compile happens before any filesystem work so invalid magic fails
    // closed, and the empty string stays rejected — under a full-tree
    // fallback it would silently widen the DELETION set.
    let pathspecs = if args.pathspec.is_empty() {
        None
    } else {
        if args.pathspec.iter().any(|raw| raw.is_empty()) {
            return Err(clean_cli_error(CleanError::InvalidArgs(
                "empty string is not a valid pathspec".to_string(),
            )));
        }
        let current_dir = std::env::current_dir()
            .map_err(|error| clean_cli_error(CleanError::ResolveWorkdir(error.to_string())))?;
        let workdir = util::working_dir();
        let ignore_case = crate::utils::path_case::effective_ignore_case()
            .await
            .map_err(|error| clean_cli_error(CleanError::ResolveWorkdir(error.to_string())))?;
        Some(
            PathspecSet::from_workdir_with_default_icase(
                &args.pathspec,
                &current_dir,
                &workdir,
                ignore_case,
            )
            .map_err(|error| {
                clean_cli_error(CleanError::InvalidArgs(error.to_string()))
                    .with_hint("use supported pathspec magic: top, exclude, icase, literal, glob")
            })?,
        )
    };

    // lore.md 2.4 / §C.4.1.1 / §C.10: load THIS worktree's layer-exclusion
    // snapshot before enumerating anything, under the layer MUTATION LOCK held
    // across the deletion.
    //
    // Three separate hazards, all ending in a deleted overlay that only a
    // re-apply could restore: a fresh process that never refreshed sees an
    // EMPTY snapshot; a failed ownership read that resolves to an empty set
    // looks the same; and without the lock a concurrent `layer apply` can
    // record and materialize between the snapshot and the delete. So: strict
    // (fallible) refresh, and the lock spans snapshot-plus-delete.
    let layer_scope = crate::internal::worktree_scope::WorktreeScope::for_request();
    let _layer_lock =
        crate::internal::layer::layer_mutation_lock(&layer_scope).map_err(|error| {
            clean_cli_error(CleanError::ScanUntracked(format!(
                "cannot take this worktree's layer lock (refusing to delete without it): {error}"
            )))
        })?;
    crate::internal::layer::refresh_exclusion_snapshot_strict(&layer_scope)
        .await
        .map_err(|error| {
            clean_cli_error(CleanError::ScanUntracked(format!(
                "{error} — refusing to delete without knowing which paths layer overlays own"
            )))
        })?;

    let clean_output = run_clean(args, pathspecs).map_err(clean_cli_error)?;

    if output.is_json() {
        emit_json_data("clean", &clean_output, output)?;
    } else if !output.quiet {
        for path in &clean_output.removed {
            if clean_output.dry_run {
                println!("Would remove {path}");
            } else {
                println!("Removing {path}");
            }
        }
    }

    Ok(())
}

fn run_clean(args: CleanArgs, pathspecs: Option<PathspecSet>) -> Result<CleanOutput, CleanError> {
    if !args.force && !args.dry_run {
        return Err(CleanError::MissingMode);
    }

    // Validate mutually exclusive flags
    if args.ignored && args.only_ignored {
        return Err(CleanError::InvalidArgs(
            "cannot use -x and -X together".to_string(),
        ));
    }

    let index_path = path::index();
    let index = match Index::load(&index_path) {
        Ok(index) => index,
        Err(e) => {
            if !index_path.exists() {
                Index::new()
            } else {
                return Err(CleanError::LoadIndex(e.to_string()));
            }
        }
    };

    // Determine the ignore policy based on flags
    let policy = if args.only_ignored {
        IgnorePolicy::OnlyIgnored
    } else if args.ignored {
        IgnorePolicy::IncludeIgnored
    } else {
        IgnorePolicy::Respect
    };

    // Collect workdir files and apply ignore policy. The default path can prune ignored
    // directories; -x/-X must still inspect ignored files because those modes target them.
    let workdir_files = match policy {
        IgnorePolicy::Respect => util::list_workdir_files(),
        IgnorePolicy::IncludeIgnored | IgnorePolicy::OnlyIgnored => {
            util::list_workdir_files_unfiltered()
        }
    }
    .map_err(|e| CleanError::ScanUntracked(e.to_string()))?;
    // ONE snapshot for this clean (§C.4.1.1), applied below to files AND to the
    // directory scan. lore.md 2.4: a materialized layer overlay is protected
    // from `clean` under EVERY policy — including `-x`, where the ignore
    // engine's `IncludeIgnored` deliberately stops consulting layers (that
    // policy exists for force-ADD, where the staging guard is the backstop).
    // Here the stake is deletion, and only a re-apply could restore the file.
    let layers = crate::internal::layer::ExclusionSnapshot::for_request();
    let filtered_files = ignore::filter_workdir_paths(workdir_files, policy, &index)
        .into_iter()
        .filter(|path| {
            layers.is_empty()
                || !crate::internal::layer::normalize_key(path)
                    .is_some_and(|key| layers.is_owned(&key))
        })
        .collect::<Vec<_>>();

    // Find untracked files
    let mut untracked: Vec<PathBuf> = Vec::new();
    for path in filtered_files {
        let path_str = path.to_str().ok_or_else(|| {
            CleanError::ScanUntracked(format!("path {:?} is not valid UTF-8", path))
        })?;
        if !worktree::index_has_any_stage(&index, path_str) {
            untracked.push(path);
        }
    }

    // If -d, also find untracked directories
    if args.directories {
        let untracked_dirs = find_untracked_dirs(&index, policy, &layers)?;
        for dir in untracked_dirs {
            // Skip the root directory (empty path)
            if dir.as_os_str().is_empty() {
                continue;
            }
            // Remove any files that are inside this directory from the untracked list
            // since the directory itself will be removed
            untracked.retain(|p| !p.starts_with(&dir));
            // Add the directory if it's not already covered by a parent directory
            if !untracked.iter().any(|p| dir.starts_with(p)) {
                untracked.push(dir);
            }
        }
    }

    // Apply --exclude patterns
    if !args.exclude.is_empty() {
        untracked.retain(|path| {
            let path_str = path.display().to_string();
            !args
                .exclude
                .iter()
                .any(|pattern| matches_exclude_pattern(&path_str, pattern))
        });
    }

    // PD-07: apply <pathspec>... limiting through the shared engine. The
    // SAME filtered list feeds both the -n preview and the -f deletion pass
    // below, so the preview set is definitionally the deletion set; a
    // pathspec (including `:(exclude)` magic) can only NARROW the
    // untracked-only candidate list built above, never widen it.
    if let Some(pathspecs) = &pathspecs {
        untracked.retain(|path| pathspecs.matches_path(path));
    }

    if untracked.is_empty() {
        return Ok(CleanOutput {
            dry_run: args.dry_run,
            removed: Vec::new(),
        });
    }

    if args.dry_run {
        return Ok(CleanOutput {
            dry_run: true,
            removed: untracked
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
        });
    }

    let workdir = fs::canonicalize(util::working_dir())
        .map_err(|e| CleanError::ResolveWorkdir(e.to_string()))?;
    let mut removed = Vec::new();
    for path in untracked {
        let abs_path = util::workdir_to_absolute(&path);
        if abs_path.exists() {
            let resolved = fs::canonicalize(&abs_path).map_err(|e| CleanError::ResolvePath {
                path: abs_path.display().to_string(),
                detail: e.to_string(),
            })?;
            if !resolved.starts_with(&workdir) {
                return Err(CleanError::OutsideWorkdir(abs_path.display().to_string()));
            }
            if abs_path.is_dir() {
                fs::remove_dir_all(&abs_path).map_err(|e| CleanError::RemoveFile {
                    path: abs_path.display().to_string(),
                    detail: e.to_string(),
                })?;
            } else {
                fs::remove_file(&abs_path).map_err(|e| CleanError::RemoveFile {
                    path: abs_path.display().to_string(),
                    detail: e.to_string(),
                })?;
            }
            removed.push(path.display().to_string());
        }
    }
    Ok(CleanOutput {
        dry_run: false,
        removed,
    })
}

/// Find untracked directories based on the ignore policy.
/// A directory is considered untracked if it does not contain any tracked files.
fn find_untracked_dirs(
    index: &Index,
    policy: IgnorePolicy,
    layers: &crate::internal::layer::ExclusionSnapshot,
) -> Result<Vec<PathBuf>, CleanError> {
    let workdir = util::working_dir();
    let mut untracked_dirs = Vec::new();

    fn scan_dir(
        dir: &Path,
        workdir: &Path,
        index: &Index,
        policy: IgnorePolicy,
        layers: &crate::internal::layer::ExclusionSnapshot,
        untracked_dirs: &mut Vec<PathBuf>,
        // Returns whether this subtree holds content that must SURVIVE (a
        // tracked file or a materialized layer overlay), so the caller can
        // refuse to queue the tree that contains it.
    ) -> Result<bool, CleanError> {
        let entries = fs::read_dir(dir).map_err(|e| CleanError::ScanUntracked(e.to_string()))?;
        let mut has_tracked = false;
        let mut subdirs = Vec::new();

        for entry in entries {
            let entry = entry.map_err(|e| CleanError::ScanUntracked(e.to_string()))?;
            let path = entry.path();
            let relative = path
                .strip_prefix(workdir)
                .map_err(|e| CleanError::ScanUntracked(e.to_string()))?;

            if path.is_dir() {
                let name = path.file_name().unwrap_or_default();
                if name == ".git" || name == util::ROOT_DIR {
                    continue;
                }
                if policy == IgnorePolicy::Respect
                    && ignore::should_ignore(relative, IgnorePolicy::Respect, index)
                {
                    continue;
                }
                subdirs.push(path.clone());
            } else if let Some(path_str) = relative.to_str() {
                // Check if this file is tracked
                if index.tracked(path_str, 0) {
                    has_tracked = true;
                }
                // A materialized layer overlay counts as content that must
                // SURVIVE, exactly like a tracked file: the file itself is
                // protected, so removing the directory holding it would
                // destroy it anyway (`clean -d` removes the tree).
                if !layers.is_empty()
                    && crate::internal::layer::normalize_key(relative)
                        .is_some_and(|key| layers.is_owned(&key))
                {
                    has_tracked = true;
                }
            }
        }

        // RECURSE FIRST, then decide about this directory. `clean -d` removes a
        // directory TREE, so content that must survive anywhere beneath it
        // protects the whole path — deciding before the recursion queued
        // `dst` for removal while `dst/nested/overlay.txt` was still
        // undiscovered, and the tree took the protected file with it.
        for subdir in subdirs {
            if scan_dir(&subdir, workdir, index, policy, layers, untracked_dirs)? {
                has_tracked = true;
            }
        }

        if !has_tracked {
            // Check if this directory should be ignored
            let relative = dir
                .strip_prefix(workdir)
                .map_err(|e| CleanError::ScanUntracked(e.to_string()))?;
            let should_include = match policy {
                IgnorePolicy::Respect => {
                    // Only include if not ignored
                    !ignore::should_ignore(relative, policy, index)
                }
                IgnorePolicy::IncludeIgnored => true,
                IgnorePolicy::OnlyIgnored => {
                    // Only include if ignored
                    ignore::should_ignore(relative, IgnorePolicy::Respect, index)
                }
            };
            if should_include {
                untracked_dirs.push(relative.to_path_buf());
            }
        }

        // Report upward whether anything here must survive, so a parent cannot
        // queue a tree that contains it.
        Ok(has_tracked)
    }

    scan_dir(
        &workdir,
        &workdir,
        index,
        policy,
        layers,
        &mut untracked_dirs,
    )?;
    Ok(untracked_dirs)
}

/// Check if a path matches an exclude pattern using glob-style matching.
/// Supports * (match any characters) and ? (match single character).
fn matches_exclude_pattern(path: &str, pattern: &str) -> bool {
    // Escape special regex characters, then convert glob patterns
    let mut regex_pattern = String::new();
    regex_pattern.push('^');
    let chars = pattern.chars();
    for c in chars {
        match c {
            '*' => regex_pattern.push_str(".*"),
            '?' => regex_pattern.push('.'),
            '.' | '+' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' | '\\' => {
                regex_pattern.push('\\');
                regex_pattern.push(c);
            }
            _ => regex_pattern.push(c),
        }
    }
    regex_pattern.push('$');

    if let Ok(re) = regex::Regex::new(&regex_pattern) {
        re.is_match(path)
    } else {
        // Fallback to simple string matching
        path.contains(pattern)
    }
}

fn clean_cli_error(error: CleanError) -> CliError {
    match error {
        CleanError::MissingMode => CliError::fatal(error.to_string())
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint("use 'libra clean -n' to preview removals.")
            .with_hint("use 'libra clean -f' to remove untracked files."),
        CleanError::InvalidArgs(message) => {
            CliError::fatal(format!("invalid arguments: {message}"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        }
        CleanError::LoadIndex(message) => {
            CliError::fatal(format!("failed to load index: {message}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        }
        CleanError::ScanUntracked(message) => {
            CliError::fatal(message).with_stable_code(StableErrorCode::IoReadFailed)
        }
        CleanError::ResolveWorkdir(message) => {
            CliError::fatal(format!("failed to resolve working directory: {message}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        }
        CleanError::ResolvePath { path, detail } => {
            CliError::fatal(format!("failed to resolve path {path}: {detail}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        }
        CleanError::OutsideWorkdir(path) => {
            CliError::fatal(format!("refusing to remove path outside workdir: {path}"))
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
        }
        CleanError::RemoveFile { path, detail } => {
            CliError::fatal(format!("failed to remove {path}: {detail}"))
                .with_stable_code(StableErrorCode::IoWriteFailed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CleanError, clean_cli_error};
    use crate::utils::error::StableErrorCode;

    #[test]
    fn resolve_workdir_cli_error_keeps_context() {
        let error = clean_cli_error(CleanError::ResolveWorkdir("permission denied".to_string()));

        assert_eq!(error.stable_code(), StableErrorCode::IoReadFailed);
        assert!(
            error
                .message()
                .contains("failed to resolve working directory"),
            "unexpected error message: {}",
            error.message()
        );
    }

    /// Pin the `Display` format for every variant of [`CleanError`].
    /// These strings are used as the CliError message via
    /// `clean_cli_error` and surface in both human and `--json`
    /// envelopes for the `clean` subcommand.
    #[test]
    fn clean_error_display_pins_each_variant() {
        assert_eq!(
            CleanError::MissingMode.to_string(),
            "clean requires -f or -n (use -f to remove files, -n to dry-run)",
        );
        assert_eq!(
            CleanError::InvalidArgs("--fff is not a valid flag".to_string()).to_string(),
            "invalid arguments: --fff is not a valid flag",
        );
        assert_eq!(
            CleanError::LoadIndex("index file corrupt".to_string()).to_string(),
            "failed to load index: index file corrupt",
        );
        // ScanUntracked echoes the inner string verbatim.
        assert_eq!(
            CleanError::ScanUntracked("walk failed at /tmp".to_string()).to_string(),
            "walk failed at /tmp",
        );
        assert_eq!(
            CleanError::ResolveWorkdir("permission denied".to_string()).to_string(),
            "failed to resolve working directory: permission denied",
        );
        assert_eq!(
            CleanError::ResolvePath {
                path: "src/foo.rs".to_string(),
                detail: "no such file".to_string(),
            }
            .to_string(),
            "failed to resolve path src/foo.rs: no such file",
        );
        assert_eq!(
            CleanError::OutsideWorkdir("/tmp/elsewhere".to_string()).to_string(),
            "refusing to remove path outside workdir: /tmp/elsewhere",
        );
        assert_eq!(
            CleanError::RemoveFile {
                path: "build/artifact.o".to_string(),
                detail: "permission denied".to_string(),
            }
            .to_string(),
            "failed to remove build/artifact.o: permission denied",
        );
    }
}
