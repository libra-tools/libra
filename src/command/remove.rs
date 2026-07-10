//! Removes paths from the index and working tree according to pathspecs, supporting recursive deletion and cache-only modes.

use std::path::PathBuf;

use clap::Parser;
use colored::Colorize;
use git_internal::internal::index::Index;
use serde::Serialize;
use tokio::fs;

use crate::{
    command::status::{changes_to_be_committed_safe, changes_to_be_staged},
    utils::{
        error::{CliError, CliResult},
        output::{OutputConfig, emit_json_data},
        path,
        pathspec::PathspecSet,
        util::{self, path_to_string},
    },
};

/// `--help` examples shown in `libra rm --help` output.
///
/// `rm` (aliased to `remove` / `delete`) removes paths from the index
/// and (unless `--cached`) the working tree. The banner pins single-file
/// removal, recursive directory removal, `--cached` for "keep file but
/// untrack", `--force` for overriding the conflicting-state safety
/// rules, `--dry-run` for previewing, pathspec-from-file for scripts,
/// and a JSON variant for agents. Cross-cutting `--help` EXAMPLES
/// rollout per `docs/development/commands/_general.md` item B.
pub const REMOVE_EXAMPLES: &str = "\
EXAMPLES:
    libra rm stale.txt                      Remove a single tracked file from index and working tree
    libra rm -r logs/                       Recursively remove a tracked directory
    libra rm --cached secrets.env           Untrack the file but keep it on disk
    libra rm -f conflicted.txt              Force removal even if the file has unstaged changes
    libra rm --dry-run --cached '*.tmp'     Preview what would be untracked without applying
    libra rm --pathspec-from-file=todo.txt  Read NUL- or newline-separated pathspecs from a file
    libra rm --sparse stale.txt             Accept Git's sparse-checkout flag as a no-op
    libra rm --json stale.txt               Structured JSON output for agents";

#[derive(Parser, Debug, Clone)]
#[command(after_help = REMOVE_EXAMPLES)]
pub struct RemoveArgs {
    /// file or dir to remove
    pub pathspec: Vec<String>,
    /// whether to remove from index
    #[clap(long)]
    pub cached: bool,
    /// indicate recursive remove dir
    #[clap(short, long)]
    pub recursive: bool,
    /// force removal, skip validation
    #[clap(short, long)]
    pub force: bool,
    /// show what would be removed without actually removing
    #[clap(long)]
    pub dry_run: bool,
    /// Exit with a zero status even if no files matched.
    #[clap(long)]
    pub ignore_unmatch: bool,
    /// Read pathspecs from file
    #[clap(long = "pathspec-from-file")]
    pub pathspec_from_file: Option<String>,
    /// Pathspec file is NUL separated
    #[clap(long = "pathspec-file-nul")]
    pub pathspec_file_nul: bool,

    /// Accept Git's `--sparse` flag. Git uses it to allow removing index
    /// entries outside the sparse-checkout cone; Libra has no sparse-checkout
    /// state (every path is always "in cone"), so this is a no-op.
    #[clap(long)]
    pub sparse: bool,
}

//  ==============================================
//  Scenarios where --cached is recommended
//  ==============================================
//  1. Files have local modifications:
//     When the file in the working tree differs from the index,
//     the error message will prompt to use --cached to keep the local file.
//
//  2. Index has staged changes:
//     When the content in the index differs from HEAD,
//     the error message will also prompt to use --cached.

//  ==============================================
//  Scenarios where -f (force) is required
//  ==============================================
//  1. Both index and working tree have modifications:
//     The file's content in the index differs from the working tree,
//     AND the content in the index also differs from HEAD.
//
//  2. Has staged conflicting content:
//     When the staged content of the file differs from both the file itself (working tree) and HEAD,
//     the error message will prompt to use -f to force deletion.
/// Represents the status of files with uncommitted changes, used to determine
/// which files have local modifications, staged changes, or both, relative to the index and HEAD.
#[derive(Debug, Default)]
struct DiffStatus {
    /// Files with local modifications: working tree differs from index.
    index_workingtree: Option<Vec<String>>,
    /// Files with staged changes: index differs from HEAD (commit).
    index_commit: Option<Vec<String>>,
    /// Files with both staged and working tree changes that differ from HEAD.
    index_commit_workingtree: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RemoveOutput {
    pathspecs: Vec<String>,
    paths: Vec<RemovePathOutput>,
    directories: Vec<RemoveDirectoryOutput>,
    cached: bool,
    recursive: bool,
    forced: bool,
    dry_run: bool,
}

#[derive(Debug, Serialize)]
struct RemovePathOutput {
    path: String,
    removed_from_index: bool,
    removed_from_disk: bool,
}

#[derive(Debug, Serialize)]
struct RemoveDirectoryOutput {
    path: String,
    removed_from_disk: bool,
}

pub async fn execute(args: RemoveArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting. Removes paths from the index and optionally from the
/// working tree, supporting recursive and cache-only modes.
pub async fn execute_safe(args: RemoveArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    let result = run_remove(args).await?;
    render_remove_output(&result, output)
}

async fn run_remove(args: RemoveArgs) -> CliResult<RemoveOutput> {
    let idx_file = path::index();
    let mut index = match Index::load(&idx_file) {
        Ok(index) => index,
        Err(err) => {
            return Err(CliError::fatal(err.to_string()));
        }
    };

    // Build effective pathspec list
    let pathspecs: Vec<String> = if let Some(file) = &args.pathspec_from_file {
        let data = match std::fs::read(file) {
            Ok(d) => d,
            Err(e) => {
                return Err(CliError::fatal(format!(
                    "cannot read pathspec file '{}': {}",
                    file, e
                )));
            }
        };

        if args.pathspec_file_nul {
            data.split(|b| *b == b'\0')
                .filter_map(|s| {
                    let s = std::str::from_utf8(s).ok()?.trim();
                    if s.is_empty() {
                        None
                    } else {
                        Some(s.to_string())
                    }
                })
                .collect()
        } else {
            data.split(|b| *b == b'\n')
                .filter_map(|s| {
                    let s = std::str::from_utf8(s).ok()?.trim();
                    if s.is_empty() {
                        None
                    } else {
                        Some(s.to_string())
                    }
                })
                .collect()
        }
    } else {
        args.pathspec.clone()
    };

    if pathspecs.is_empty() {
        return Err(CliError::fatal(
            "No pathspec was given. Which files should I remove?",
        ));
    }

    let current_dir = std::env::current_dir()
        .map_err(|error| CliError::fatal(format!("failed to read current directory: {error}")))?;
    let workdir = util::try_working_dir()
        .map_err(|error| CliError::fatal(format!("failed to determine worktree root: {error}")))?;
    let ignore_case = crate::utils::path_case::effective_ignore_case()
        .await
        .map_err(|error| CliError::fatal(error.to_string()))?;
    let compiled = PathspecSet::from_workdir_with_default_icase(
        &pathspecs,
        &current_dir,
        &workdir,
        ignore_case,
    )
    .map_err(|error| {
        CliError::fatal(error.to_string())
            .with_hint("use supported pathspec magic: top, exclude, icase, literal, glob")
    })?;

    let tracked_files = index.tracked_files();
    if let Some(unmatched) = compiled.unmatched_positive(&tracked_files)
        && !args.ignore_unmatch
    {
        return Err(
            CliError::fatal(format!("pathspec '{unmatched}' did not match any files"))
                .with_hint("run 'libra status' to inspect tracked paths.")
                .with_hint("use '--ignore-unmatch' to ignore missing paths."),
        );
    }

    let mut remove_list = tracked_files
        .iter()
        .filter(|path| compiled.matches_path(path))
        .map(|path| path_to_string(path))
        .collect::<Vec<_>>();
    remove_list.sort();
    remove_list.dedup();
    let matched_dirs = matched_directory_prefixes(&compiled, &workdir);

    if let Some(dir) = matched_dirs.first()
        && !args.recursive
    {
        let error_msg = format!("not removing '{}' recursively without -r", dir.display());
        return Err(CliError::fatal(error_msg));
    }
    let remove_dir_list = if args.recursive && !args.cached {
        plain_directory_prefixes_for_removal(&compiled, &workdir)
    } else {
        Vec::new()
    };

    // Check all input paths for any uncommitted changes.
    let mut diff_status = DiffStatus::default();
    if !args.force {
        let mut error_msg = String::new();
        let changes_staged = match changes_to_be_staged() {
            Ok(c) => c.polymerization(),
            Err(err) => {
                return Err(CliError::from(err));
            }
        };
        let changes_committed = match changes_to_be_committed_safe().await {
            Ok(c) => c.polymerization(),
            Err(err) => {
                return Err(CliError::from(err));
            }
        };
        // Check for both
        let mut buf = Vec::new();
        for path_str in remove_list.iter() {
            if changes_staged.contains(&PathBuf::from(&path_str))
                && changes_committed.contains(&PathBuf::from(&path_str))
            {
                buf.push(path_str.clone());
            }
        }
        if !buf.is_empty() {
            diff_status.index_commit_workingtree = buf
        }
        if !args.cached {
            // Check for unstaged changes in workingtree files
            let mut buf = Vec::new();
            for path_str in remove_list.iter() {
                if changes_staged.contains(&PathBuf::from(path_str))
                    && !diff_status.index_commit_workingtree.contains(path_str)
                {
                    buf.push(path_str.clone());
                }
            }
            if !buf.is_empty() {
                diff_status.index_workingtree = Some(buf)
            }
            // Check for workingtree changes in committed files
            let mut buf = Vec::new();
            for path_str in remove_list.iter() {
                if changes_committed.contains(&PathBuf::from(path_str))
                    && !diff_status.index_commit_workingtree.contains(path_str)
                {
                    buf.push(path_str.clone());
                }
            }
            if !buf.is_empty() {
                diff_status.index_commit = Some(buf)
            }

            // Print error reason
            if let Some(files) = diff_status.index_commit.as_ref() {
                error_msg.push_str(&format!(
                    "the following {} changes staged in the index:\n",
                    if files.len() > 1 {
                        "files have"
                    } else {
                        "file has"
                    }
                ));
                for file in files {
                    error_msg.push_str(&format!("\t{}\n", file));
                }
                error_msg.push_str("(use --cached to keep the file, or -f to force removal)");
            }
            if let Some(files) = diff_status.index_workingtree.as_ref() {
                error_msg.push_str(&format!(
                    "the following {} local modifications:\n",
                    if files.len() > 1 {
                        "files have"
                    } else {
                        "file has"
                    }
                ));
                for file in files {
                    error_msg.push_str(&format!("\t{}\n", file));
                }
                error_msg.push_str("(use --cached to keep the file, or -f to force removal)");
            }
        }
        if !diff_status.index_commit_workingtree.is_empty() {
            error_msg.push_str(&format!(
                "the following {} staged content different from both the\nfile and the HEAD:\n",
                if diff_status.index_commit_workingtree.len() > 1 {
                    "files have"
                } else {
                    "file has"
                }
            ));
            for file in diff_status.index_commit_workingtree {
                error_msg.push_str(&format!("\t{}\n", file));
            }
            error_msg.push_str("(use -f to force removal)");
        }
        if !error_msg.is_empty() {
            return Err(CliError::failure(error_msg.trim_end().to_string()));
        }
    }

    let paths = remove_list
        .iter()
        .map(|path| RemovePathOutput {
            path: path.clone(),
            removed_from_index: !args.dry_run,
            removed_from_disk: !args.cached && !args.dry_run,
        })
        .collect();

    for path_str in remove_list.iter() {
        if !args.dry_run {
            index.remove(path_str, 0);
        }
    }
    if !args.cached && !args.dry_run {
        for path_str in &remove_list {
            let path = workdir.join(path_str);
            if let Err(e) = fs::remove_file(&path).await {
                return Err(CliError::failure(format!(
                    "failed to remove file '{}': {}",
                    path_str, e
                )));
            }
            util::clear_empty_dir(&path);
        }
    }
    let directories = remove_dir_list
        .iter()
        .map(|path| RemoveDirectoryOutput {
            path: path_to_string(path),
            removed_from_disk: !args.cached && !args.dry_run && !workdir.join(path).exists(),
        })
        .collect();

    if index.save(&idx_file).is_err() {
        return Err(CliError::fatal("failed to save index"));
    }

    Ok(RemoveOutput {
        pathspecs,
        paths,
        directories,
        cached: args.cached,
        recursive: args.recursive,
        forced: args.force,
        dry_run: args.dry_run,
    })
}

fn render_remove_output(result: &RemoveOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("rm", result, output);
    }
    if output.quiet {
        return Ok(());
    }
    for path in &result.paths {
        println!("rm '{}'", path.path.bright_yellow());
    }
    Ok(())
}

fn matched_directory_prefixes(pathspecs: &PathspecSet, workdir: &std::path::Path) -> Vec<PathBuf> {
    pathspecs
        .positive_prefixes()
        .into_iter()
        .filter(|path| workdir.join(path).is_dir())
        .collect()
}

fn plain_directory_prefixes_for_removal(
    pathspecs: &PathspecSet,
    workdir: &std::path::Path,
) -> Vec<PathBuf> {
    pathspecs
        .plain_positive_prefixes()
        .unwrap_or_default()
        .into_iter()
        .filter(|path| path != std::path::Path::new(".") && !path.as_os_str().is_empty())
        .filter(|path| workdir.join(path).is_dir())
        .collect()
}
