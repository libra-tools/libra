//! Stages changes for the next commit (`libra add`).
//!
//! Implements the `add` subcommand: parses pathspecs and mode flags, applies
//! ignore policy, classifies each path against the working
//! tree and the on-disk index, writes new blob objects under the repository's
//! object storage, and finally persists the updated index.
//!
//! Non-obvious responsibilities:
//! - Maps low-level [`GitError`] / [`io::Error`] variants into structured
//!   [`AddError`] cases that each carry stable error codes and human-readable
//!   hints (see the `From<AddError> for CliError` impl).
//! - Supports four output channels in [`render_add_output`]: JSON, quiet
//!   (warnings only on stderr), normal (summary), and verbose (per-path).
//! - Provides a "refresh-only" mode that updates index stat metadata without
//!   rewriting blobs.
//! - Filters the running `libra` executable from staging candidates so a
//!   self-build does not accidentally stage its own binary.
//! - Honors the `force` flag by folding ignored paths back into the visible
//!   change set before pathspec validation runs.

use std::{
    env,
    io::{self, Write},
    path::{Path, PathBuf},
};

use clap::Parser;
use git_internal::{
    errors::GitError,
    internal::{
        index::{Index, IndexEntry},
        object::blob::Blob,
    },
};
use serde::Serialize;

use crate::{
    command::{
        read_worktree_blob_bytes,
        status::{self, Changes},
    },
    internal::ai::automation::{VCS_EVENT_POST_ADD, dispatch_current_repo_vcs_event_to_history},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        object_ext::BlobExt,
        output::{self, OutputConfig},
        path,
        pathspec::{PathspecError, PathspecSet},
        util,
    },
};

const ADD_EXAMPLES: &str = "\
EXAMPLES:
    libra add .                        Stage all changes in current directory
    libra add src/main.rs              Stage a specific file
    libra add src/ tests/              Stage multiple paths
    libra add -A                       Stage all changes (adds, modifies, removes)
    libra add -u                       Update tracked files only (no new files)
    libra add --dry-run .              Preview what would be staged
    libra add -f ignored_file.log      Force-add an ignored file
    libra add --refresh                Refresh index metadata without staging";

/// Stage file contents for the next commit.
// EXAMPLES are wired via `#[command(after_help = ADD_EXAMPLES)]` and render
// at the bottom of `libra add --help`. The meta-commentary that used to live
// here as a `///` line leaked into clap's `--help` body (see
// `tests/command/add_test.rs::test_add_help_does_not_leak_impl_meta`).
#[derive(Parser, Debug)]
#[command(after_help = ADD_EXAMPLES)]
pub struct AddArgs {
    /// pathspec... files & dir to add content from.
    #[clap(required = false)]
    pub pathspec: Vec<String>,

    /// Update the index not only where the working tree has a file matching pathspec but also where the index already has an entry. This adds, modifies, and removes index entries to match the working tree.
    ///
    /// If no pathspec is given when -A option is used, all files in the entire working tree are updated
    #[clap(short = 'A', long, group = "mode")]
    pub all: bool,

    /// Update the index just where it already has an entry matching **pathspec**.
    /// This removes as well as modifies index entries to match the working tree, but adds no new files.
    #[clap(short, long, group = "mode")]
    pub update: bool,

    /// Refresh index entries for all files currently in the index.
    ///
    /// This updates only the metadata (e.g. file stat information such as
    /// timestamps, file size, etc.) of existing index entries to match
    /// the working tree, without adding new files or removing entries.
    #[clap(long, group = "mode")]
    pub refresh: bool,

    /// more detailed output
    #[clap(short, long)]
    pub verbose: bool,

    /// allow adding otherwise ignored files
    #[clap(short = 'f', long)]
    pub force: bool,

    /// dry run: show what would be staged without changing the index.
    /// `-n` matches Git; `-d` is kept as a Libra-compatible alias.
    #[clap(short = 'n', long, visible_short_alias = 'd')]
    pub dry_run: bool,

    /// ignore errors
    #[clap(long)]
    pub ignore_errors: bool,

    /// Read pathspecs from a file (one per line, or NUL-separated with --pathspec-file-nul).
    #[clap(long = "pathspec-from-file", value_name = "FILE")]
    pub pathspec_from_file: Option<String>,

    /// Use NUL as the pathspec separator when reading from --pathspec-from-file.
    #[clap(long = "pathspec-file-nul", requires = "pathspec_from_file")]
    pub pathspec_file_nul: bool,

    /// Override the executable bit recorded in the index for the matched paths:
    /// `+x` makes them executable (mode `100755`), `-x` clears it (`100644`).
    /// Mirrors Git's `add --chmod=(+|-)x`.
    #[clap(long = "chmod", value_name = "(+|-)x")]
    pub chmod: Option<String>,

    /// Re-stage tracked files from scratch, rewriting their blobs even when the
    /// content is unchanged (Git's `--renormalize`). Implies `-u`: only tracked
    /// files are processed, never untracked ones.
    #[clap(long)]
    pub renormalize: bool,

    /// Under `--dry-run`, silently skip pathspecs that match no file instead of
    /// failing. Mirrors Git's `add --ignore-missing`, which requires `--dry-run`.
    #[clap(long = "ignore-missing", requires = "dry_run")]
    pub ignore_missing: bool,
}

/// Domain error for `libra add`.
///
/// Each variant maps to a specific failure mode of the staging pipeline and is
/// translated into a [`CliError`] (with a stable code and hints) by the
/// `From<AddError> for CliError` impl below. Variants are not numbered in the
/// public API; classification happens inside that impl.
#[derive(thiserror::Error, Debug)]
pub enum AddError {
    /// No `.libra` directory was found walking up from the CWD. Surfaced as
    /// [`StableErrorCode::RepoNotFound`].
    #[error("not a libra repository (or any of the parent directories): .libra")]
    NotInRepo,
    /// The `lfs.lockEnforce` gate refused the operation (lore.md 2.8); the
    /// carried [`CliError`] already has its stable code and hints.
    #[error("{0}")]
    LockPolicy(CliError),
    /// A layer-owned overlay path was requested for staging (lore.md 2.4).
    /// Layers are purely local and must never enter a commit.
    #[error("'{path}' is a layer overlay path and cannot be staged ({count} such path(s))")]
    LayerPath { path: String, count: usize },
    /// A user-supplied pathspec matched neither tracked files, working-tree
    /// changes, nor an ignored entry — typically a typo. Mapped to
    /// [`StableErrorCode::CliInvalidTarget`].
    #[error("pathspec '{pathspec}' did not match any files")]
    PathspecNotMatched { pathspec: String },
    /// The (canonical) pathspec resolves outside the repository working tree,
    /// for example via `..` traversal or an absolute path to another repo.
    #[error("'{path}' is outside repository at '{repo_root}'")]
    PathOutsideRepo { path: String, repo_root: PathBuf },
    /// `Index::load` failed — usually means a corrupt or truncated
    /// `.libra/index`. Mapped to [`StableErrorCode::RepoCorrupt`].
    #[error("unable to read index '{path}': {source}")]
    IndexLoad { path: PathBuf, source: GitError },
    /// Persisting the updated index back to disk failed (e.g. permission
    /// denied or out of space).
    #[error("unable to write index '{path}': {source}")]
    IndexSave { path: PathBuf, source: GitError },
    /// `Index::refresh` could not stat a tracked file in `--refresh` mode.
    #[error("failed to refresh '{path}': {source}")]
    RefreshFailed { path: PathBuf, source: GitError },
    /// Building an [`IndexEntry`] from a worktree file failed (typically an
    /// `lstat`/`open` error).
    #[error("failed to create index entry for '{path}': {source}")]
    CreateIndexEntry { path: PathBuf, source: io::Error },
    /// Path bytes are not valid UTF-8 — Libra's index does not yet preserve
    /// non-UTF-8 paths verbatim.
    #[error("path '{path}' is not valid UTF-8")]
    InvalidPathEncoding { path: PathBuf },
    /// Failure resolving the working directory (CWD missing, permission
    /// denied, etc.). The `From` impl below distinguishes "missing" (treated
    /// as `RepoNotFound`) from other I/O errors.
    #[error("failed to determine repository working directory: {source}")]
    Workdir { source: io::Error },
    /// The status engine failed before staging could proceed; the underlying
    /// [`status::StatusError`] is preserved as a source.
    #[error("failed to inspect repository status: {source}")]
    Status { source: status::StatusError },
    /// The shared Git-style pathspec parser rejected the user input.
    #[error("{source}")]
    Pathspec { source: PathspecError },
}

impl From<AddError> for CliError {
    fn from(error: AddError) -> Self {
        match &error {
            AddError::LockPolicy(inner) => inner.clone(),
            AddError::LayerPath { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::LayerConflict)
                .with_hint("layer overlays are local-only; 'libra layer unapply' to remove them"),
            AddError::NotInRepo => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoNotFound)
                .with_hint("run 'libra init' to create a repository"),
            AddError::PathspecNotMatched { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("check the path and try again.")
                .with_hint("use 'libra status' to inspect tracked and untracked files."),
            AddError::PathOutsideRepo { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("all paths must be within the repository working tree"),
            AddError::IndexLoad { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint("the index file may be corrupted; try 'libra status' to verify"),
            AddError::IndexSave { .. } => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoWriteFailed)
            }
            AddError::RefreshFailed { .. } => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
            }
            AddError::CreateIndexEntry { .. } => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoWriteFailed)
            }
            AddError::InvalidPathEncoding { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("path contains non-UTF-8 characters"),
            AddError::Workdir { source } => {
                if source.kind() == io::ErrorKind::NotFound {
                    CliError::fatal(error.to_string())
                        .with_stable_code(StableErrorCode::RepoNotFound)
                } else {
                    CliError::fatal(error.to_string())
                        .with_stable_code(StableErrorCode::IoReadFailed)
                }
            }
            AddError::Status { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint("failed to compute working tree status"),
            AddError::Pathspec { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("use supported pathspec magic: top, exclude, icase, literal, glob"),
        }
    }
}

// ---------------------------------------------------------------------------
// Structured output types
// ---------------------------------------------------------------------------

/// One entry in [`AddOutput::failed`]: a path that could not be staged when
/// `--ignore-errors` was set. The `message` is the rendered [`AddError`].
#[derive(Debug, Clone, Serialize)]
pub struct AddFailure {
    pub path: String,
    pub message: String,
}

/// Structured result of a single `libra add` invocation.
///
/// Built by [`run_add`] and consumed by [`render_add_output`] (text mode) or
/// emitted directly through `output::emit_json_data` (JSON mode). The fields
/// always reference paths relative to the working directory.
#[derive(Debug, Clone, Serialize)]
pub struct AddOutput {
    /// New files staged
    pub added: Vec<String>,
    /// Modified files staged
    pub modified: Vec<String>,
    /// Deleted files staged (tracked file no longer in worktree)
    pub removed: Vec<String>,
    /// Files whose metadata was refreshed (--refresh mode)
    pub refreshed: Vec<String>,
    /// Paths ignored by configured ignore sources (only when pathspec matches ignored files)
    pub ignored: Vec<String>,
    /// Paths that failed under --ignore-errors
    pub failed: Vec<AddFailure>,
    /// Pathspecs skipped under `--ignore-missing` (dry-run only). Surfaced as
    /// stderr warnings in text mode and as a machine-readable list in the JSON
    /// payload so agent callers can see what was skipped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    /// Whether this was a dry-run (no actual changes made)
    pub dry_run: bool,
}

impl AddOutput {
    /// Construct an empty result, preserving the user's `--dry-run` choice so
    /// downstream rendering can switch on it.
    fn empty(dry_run: bool) -> Self {
        Self {
            added: Vec::new(),
            modified: Vec::new(),
            removed: Vec::new(),
            refreshed: Vec::new(),
            ignored: Vec::new(),
            failed: Vec::new(),
            missing: Vec::new(),
            dry_run,
        }
    }

    /// Sum of paths that produced an actual index change. Excludes
    /// `refreshed`, since refreshing only updates stat metadata.
    ///
    /// See: tests::add_output_total_and_empty in src/command/add.rs:840.
    fn total_staged(&self) -> usize {
        self.added.len() + self.modified.len() + self.removed.len()
    }

    /// True when no path was staged or refreshed. Used together with
    /// [`Self::ignored`] in [`check_ignored_only_error`] to detect the
    /// "everything was filtered out" failure mode.
    fn is_empty(&self) -> bool {
        self.total_staged() == 0 && self.refreshed.is_empty()
    }

    fn wrote_index(&self) -> bool {
        !self.dry_run && !self.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Action tracking for add_a_file
// ---------------------------------------------------------------------------

/// The outcome of staging a single path. Returned by [`stage_a_file`] so the
/// caller can sort each path into the correct [`AddOutput`] bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StagedAction {
    Added,
    Modified,
    Removed,
    Unchanged,
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Result of [`validate_pathspecs`]: the canonicalised set of pathspecs that
/// should drive staging, plus any pathspecs that only matched
/// ignored entries (reported as warnings).
struct ValidatedPathspecs {
    pathspecs: PathspecSet,
    ignored: Vec<String>,
    /// Pathspecs skipped under `--ignore-missing` because they matched no add
    /// candidate (dry-run only). Reported as stderr warnings and the JSON
    /// payload.
    missing: Vec<String>,
}

#[derive(Clone, Copy)]
struct PathspecMatchContext<'a> {
    workdir: &'a Path,
    current_dir: &'a Path,
    ignore_case: bool,
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Fire-and-forget entry used by the simple CLI dispatcher.
///
/// Functional scope:
/// - Delegates to [`execute_safe`] using the default [`OutputConfig`].
/// - On error, prints the rendered [`CliError`] to stderr and returns; the
///   process exit code is the dispatcher's responsibility.
///
/// Boundary conditions:
/// - Does not propagate errors, so callers that care about the exit status
///   should call [`execute_safe`] directly.
pub async fn execute(args: AddArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

/// Structured entry point used by `cli::parse` and integration tests.
///
/// # Side Effects
/// - Runs the staging pipeline via [`run_add`].
/// - Persists index updates unless `--dry-run` or `--refresh` short-circuits the
///   write path.
/// - Renders success output and records process-level warnings for ignored or
///   partially failed pathspecs.
///
/// # Errors
/// Returns [`CliError`] when repository discovery fails, pathspec validation
/// fails, ignored paths block staging, object/index I/O fails, or output
/// rendering fails.
///
/// Functional scope:
/// - Runs the full staging pipeline via [`run_add`].
/// - Renders the [`AddOutput`] in the format the user requested
///   (`OutputConfig::is_json`, `quiet`, normal, verbose).
/// - Records a process-level warning (via [`output::record_warning`]) when any
///   path was ignored or fell through `--ignore-errors`.
///
/// Boundary conditions:
/// - Returns the same `Err(CliError)` produced by [`run_add`]; rendering only
///   runs after a successful staging pass.
///
/// See: tests::test_add_single_file in tests/command/add_test.rs:12.
pub async fn execute_safe(mut args: AddArgs, output: &OutputConfig) -> CliResult<()> {
    let verbose = args.verbose;
    let dry_run = args.dry_run;

    // If --pathspec-from-file is specified, read and merge pathspecs.
    if let Some(file) = args.pathspec_from_file.take() {
        let data = std::fs::read(&file).map_err(|e| {
            CliError::fatal(format!("cannot read pathspec file '{}': {}", file, e))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        let separator: u8 = if args.pathspec_file_nul { 0 } else { b'\n' };
        let from_file: Vec<String> = data
            .split(|b| *b == separator)
            .filter_map(|s| {
                let s = std::str::from_utf8(s).ok()?.trim();
                if s.is_empty() {
                    None
                } else {
                    Some(s.to_string())
                }
            })
            .collect();
        args.pathspec.extend(from_file);
    }

    let result = run_add(&args).await?;

    // --- Render output ---
    render_add_output(&result, output, verbose, dry_run)?;

    // --- Warning tracking for ignored / partial failures / skipped pathspecs ---
    if !result.ignored.is_empty() || !result.failed.is_empty() || !result.missing.is_empty() {
        output::record_warning();
    }
    if result.wrote_index() {
        dispatch_current_repo_vcs_event_to_history(VCS_EVENT_POST_ADD).await;
    }

    Ok(())
}

/// Pure staging implementation that produces [`AddOutput`] without printing.
///
/// Functional scope:
/// - Resolves repository paths (`workdir`, `.libra/index`, object storage),
///   loads the index, and runs `status::changes_to_be_staged_split_safe`.
/// - Validates pathspecs, optionally folding ignored paths in when `--force`
///   is set, and short-circuits to refresh-mode when `--refresh` is set.
/// - Filters tree changes against the requested pathspec set, then either
///   classifies (dry-run) or stages each file via [`stage_a_file`].
/// - Persists the index back to disk on the non-dry-run path.
///
/// Boundary conditions:
/// - Returns [`AddError::NotInRepo`] when the working dir, index, or storage
///   path lookups raise [`io::ErrorKind::NotFound`]; other I/O errors map to
///   [`AddError::Workdir`].
/// - Returns a `CliError::command_usage` (stable code
///   `CliInvalidArguments`) when no pathspec is given and none of `-A`,
///   `-u`, `--refresh` is set — see
///   `tests::test_add_without_path_should_error` in
///   `tests/command/add_test.rs:518`.
/// - Returns `Err(AddError::PathspecNotMatched)` for unknown pathspecs unless
///   `--ignore-errors` was set during the per-file staging loop.
///
/// See: tests::test_add_all_flag in tests/command/add_test.rs:100;
/// tests::test_add_force_tracks_ignored_file in tests/command/add_test.rs:319.
pub async fn run_add(args: &AddArgs) -> CliResult<AddOutput> {
    // lore.md 2.4: load the layer-overlay exclusion snapshot so the sync
    // ignore resolver skips layer-owned paths (a no-op with no layers).
    crate::internal::layer::refresh_exclusion_snapshot().await;
    let workdir = util::try_working_dir().map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            AddError::NotInRepo
        } else {
            AddError::Workdir { source }
        }
    })?;
    let index_path = path::try_index().map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            AddError::NotInRepo
        } else {
            AddError::Workdir { source }
        }
    })?;
    let storage_path = util::try_get_storage_path(None).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            AddError::NotInRepo
        } else {
            AddError::Workdir { source }
        }
    })?;

    // `--chmod=(+|-)x` -> the index mode to force on the matched regular files.
    // Validated up front so an invalid value fails before any staging work.
    let chmod_mode = match args.chmod.as_deref() {
        Some(value) => Some(parse_chmod(value)?),
        None => None,
    };

    // Resolve pathspecs. `--renormalize` implies `-u` (tracked-only), so it also
    // permits an empty pathspec (operate on the whole tracked set).
    if args.pathspec.is_empty() && !args.all && !args.update && !args.refresh && !args.renormalize {
        return Err(CliError::command_usage("nothing specified, nothing added")
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint("maybe you wanted to say 'libra add .'?"));
    }

    let mut index = Index::load(&index_path).map_err(|source| AddError::IndexLoad {
        path: index_path.clone(),
        source,
    })?;
    let current_dir = env::current_dir().map_err(|source| AddError::Workdir { source })?;
    let ignore_case = crate::utils::path_case::effective_ignore_case()
        .await
        .map_err(|error| {
            CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
        })?;
    let pathspec_ctx = PathspecMatchContext {
        workdir: &workdir,
        current_dir: &current_dir,
        ignore_case,
    };

    let (mut visible_changes, mut ignored_changes) = if args.force {
        status::changes_to_be_staged_split_force_with_ignore_case(ignore_case)
            .map_err(|source| AddError::Status { source })?
    } else {
        status::changes_to_be_staged_split_safe_with_ignore_case(ignore_case)
            .map_err(|source| AddError::Status { source })?
    };
    if args.force {
        visible_changes.extend(ignored_changes.clone());
        ignored_changes = Changes::default();
    }

    let validated = validate_pathspecs(
        &args.pathspec,
        pathspec_ctx,
        &visible_changes,
        &ignored_changes,
        &index,
        args.ignore_missing,
    )?;

    let mut add_output = AddOutput::empty(args.dry_run);

    // Collect ignored paths into output
    if !validated.ignored.is_empty() {
        let mut sorted_ignored = validated.ignored.clone();
        sorted_ignored.sort();
        sorted_ignored.dedup();
        add_output.ignored = sorted_ignored;
    }
    // Pathspecs skipped by `--ignore-missing` are surfaced as stderr warnings.
    add_output.missing = validated.missing.clone();

    // --- Refresh mode ---
    if args.refresh {
        let tracked_modified =
            filter_refresh_candidates(&visible_changes.modified, &validated.pathspecs);
        if args.dry_run {
            add_output.refreshed = tracked_modified
                .iter()
                .map(|f| f.display().to_string())
                .collect();
        } else {
            let refreshed = do_refresh_files(&mut index, &tracked_modified, &workdir)?;
            add_output.refreshed = refreshed.iter().map(|f| f.display().to_string()).collect();
            index
                .save(&index_path)
                .map_err(|source| AddError::IndexSave {
                    path: index_path.clone(),
                    source,
                })?;
        }

        return check_ignored_only_error(add_output);
    }

    // --- Normal add mode ---
    // `--renormalize` operates on the tracked set (implies `-u`) and force-rewrites
    // each matched blob; the regular path collects working-tree changes.
    let mut files = if args.renormalize {
        filter_candidates(&index.tracked_files(), &validated.pathspecs)
    } else {
        let mut f = visible_changes.modified;
        f.extend(visible_changes.deleted);
        if !args.update {
            f.extend(visible_changes.new);
        }
        filter_candidates(&f, &validated.pathspecs)
    };
    filter_out_current_executable(&mut files);
    files.sort();
    files.dedup();

    // Layer never-enters-commit guard (lore.md 2.4): a layer-owned overlay
    // path must NEVER be staged, EVEN under --force (which bypasses ignore
    // filtering — the ignore-exclusion chokepoint alone is not airtight).
    // Under Respect, layer paths are already ignore-excluded so `files` is
    // empty of them (this loop is a no-op — zero overhead with no layers).
    {
        // Fail-CLOSED (Codex P1): a real DB read failure here must NOT allow
        // staging (the invariant is never-enters-commit). `materialized_paths`
        // is absence-tolerant for a missing table (fresh repo) but propagates
        // any other error.
        let owned: std::collections::HashSet<String> =
            crate::internal::layer::LayerStore::materialized_paths()
                .await
                .map_err(|e| {
                    CliError::fatal(format!(
                        "cannot verify layer-owned paths before staging: {e}"
                    ))
                    .with_stable_code(StableErrorCode::IoReadFailed)
                })?
                .into_iter()
                .map(|p| p.path)
                .collect();
        if !owned.is_empty() {
            let blocked: Vec<String> = files
                .iter()
                .filter_map(|file| crate::internal::layer::normalize_key(file))
                .filter(|key| owned.contains(key))
                .collect();
            if let Some(first) = blocked.first() {
                return Err(CliError::from(AddError::LayerPath {
                    path: first.clone(),
                    count: blocked.len(),
                }));
            }
        }
    }

    // `lfs.lockEnforce` gate (lore.md 2.8): before ANY blob/index write, and
    // never on --dry-run (previews must not touch the network). `--refresh`
    // returned above (stat-only rewrite — no content change to gate).
    if !args.dry_run {
        let candidates: Vec<String> = files
            .iter()
            .map(|file| file.display().to_string())
            .collect();
        crate::command::lfs::enforce_lock_policy(&candidates)
            .await
            .map_err(AddError::LockPolicy)?;
    }

    if args.dry_run {
        // Classify files for dry-run preview.
        for file in &files {
            let path_str = file.display().to_string();
            if args.renormalize {
                // Mirror `renormalize_entry` exactly (via `symlink_metadata`, which
                // does not follow links): gone -> staged deletion, directory ->
                // skipped, regular file or symlink -> force-rewritten (modified).
                match std::fs::symlink_metadata(workdir.join(file)) {
                    Err(_) => add_output.removed.push(path_str),
                    Ok(meta) if meta.is_dir() => {}
                    Ok(_) => add_output.modified.push(path_str),
                }
                continue;
            }
            let status = check_file_status(file, &index, &workdir)?;
            match status {
                FileStatus::New => add_output.added.push(path_str),
                FileStatus::Modified => add_output.modified.push(path_str),
                FileStatus::Deleted => add_output.removed.push(path_str),
                FileStatus::Unchanged | FileStatus::NotFound => {}
            }
        }
        if let Some(target_mode) = chmod_mode {
            apply_chmod(
                &mut index,
                target_mode,
                &validated.pathspecs,
                true,
                &mut add_output,
            )?;
        }
        return check_ignored_only_error(add_output);
    }

    // Case-collision guard (lore.md 1.14): on a case-insensitive view, a
    // candidate whose FOLD matches a DIFFERENT-cased tracked entry must never
    // create an index twin (`Foo` + `foo`). Under the conservative default
    // (`core.casehandling=error`) the whole add refuses BEFORE mutating the
    // index; `warn`/`allow` skip the colliding candidates (staging under the
    // existing casing is the engine's job — v1 skips, documented).
    let files = if ignore_case {
        let policy = crate::utils::path_case::case_handling_from_config()
            .await
            .map_err(|error| {
                CliError::fatal(error.to_string())
                    .with_stable_code(StableErrorCode::RepoStateInvalid)
            })?;
        let tracked_fold: std::collections::HashMap<String, String> = index
            .tracked_files()
            .iter()
            .map(|path| {
                let text = crate::utils::util::path_to_string(path);
                (crate::utils::path_case::fold_path_key(&text), text)
            })
            .collect();
        let mut kept = Vec::with_capacity(files.len());
        let mut collisions: Vec<(String, String)> = Vec::new();
        for file in files {
            let text = crate::utils::util::path_to_string(&file);
            match tracked_fold.get(&crate::utils::path_case::fold_path_key(&text)) {
                Some(existing) if existing != &text => {
                    let existing_path = PathBuf::from(existing);
                    if crate::utils::path_case::is_same_file_case_alias(
                        &workdir,
                        &file,
                        &existing_path,
                    ) {
                        continue;
                    }
                    collisions.push((text, existing.clone()));
                }
                _ => kept.push(file),
            }
        }
        if !collisions.is_empty() {
            match policy {
                crate::utils::path_case::CaseHandling::Error => {
                    let listing = collisions
                        .iter()
                        .map(|(candidate, tracked)| {
                            format!("'{candidate}' collides with tracked '{tracked}'")
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    return Err(
                        CliError::failure(format!("case-fold path collision: {listing}"))
                            .with_stable_code(StableErrorCode::ConflictCaseCollision)
                            .with_hint(
                                "rename deliberately with 'libra mv <Tracked> <tracked>', or set \
                         core.casehandling=warn to proceed",
                            ),
                    );
                }
                crate::utils::path_case::CaseHandling::Warn => {
                    for (candidate, tracked) in &collisions {
                        crate::utils::error::emit_warning(format!(
                            "case-fold collision: '{candidate}' matches tracked '{tracked}' \
                             (skipped; use 'libra mv' for a deliberate case rename)"
                        ));
                    }
                }
                crate::utils::path_case::CaseHandling::Allow => {}
            }
        }
        kept
    } else {
        files
    };

    // Stage each file (`--renormalize` force-rewrites instead of diffing).
    for file in &files {
        let staged = if args.renormalize {
            renormalize_entry(file, &mut index, &workdir)
        } else {
            stage_a_file(file, &mut index, &workdir, &storage_path).await
        };
        match staged {
            Ok(action) => {
                let path_str = file.display().to_string();
                match action {
                    StagedAction::Added => add_output.added.push(path_str),
                    StagedAction::Modified => add_output.modified.push(path_str),
                    StagedAction::Removed => add_output.removed.push(path_str),
                    StagedAction::Unchanged => {}
                }
            }
            Err(err) => {
                if !args.ignore_errors {
                    return Err(CliError::from(err));
                }
                add_output.failed.push(AddFailure {
                    path: file.display().to_string(),
                    message: err.to_string(),
                });
            }
        }
    }

    // `--chmod=(+|-)x`: force the executable bit on the matched regular files'
    // index entries, even ones with no content change (Git's `--chmod`).
    if let Some(target_mode) = chmod_mode {
        apply_chmod(
            &mut index,
            target_mode,
            &validated.pathspecs,
            false,
            &mut add_output,
        )?;
    }

    index
        .save(&index_path)
        .map_err(|source| AddError::IndexSave {
            path: index_path.clone(),
            source,
        })?;

    check_ignored_only_error(add_output)
}

/// Parse a `--chmod=(+|-)x` value into the index mode to record: `+x` ->
/// `100755` (executable), `-x` -> `100644`.
fn parse_chmod(value: &str) -> CliResult<u32> {
    match value {
        "+x" => Ok(0o100755),
        "-x" => Ok(0o100644),
        other => Err(CliError::command_usage(format!(
            "invalid --chmod value '{other}' (expected +x or -x)"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint("use --chmod=+x to set the executable bit or --chmod=-x to clear it")),
    }
}

/// Force the executable bit on every matched, tracked **regular** file. Symlinks
/// and gitlinks are skipped (they have no executable bit). A path whose mode
/// already matches is left untouched; a real change is reported as `modified`.
/// In `dry_run` the index is not mutated, only the report.
fn apply_chmod(
    index: &mut Index,
    target_mode: u32,
    pathspecs: &PathspecSet,
    dry_run: bool,
    out: &mut AddOutput,
) -> Result<(), AddError> {
    let workdir = util::working_dir();
    let matched = filter_candidates(&index.tracked_files(), pathspecs);
    for file in &matched {
        let file_str = file
            .to_str()
            .ok_or_else(|| AddError::InvalidPathEncoding { path: file.clone() })?;
        // Read the current mode + blob id without holding the index borrow
        // (`IndexEntry` is not `Clone`).
        let Some((current_mode, hash)) = index.get(file_str, 0).map(|e| (e.mode, e.hash)) else {
            continue;
        };
        // Only regular blobs carry an executable bit; a path already at the
        // target mode needs no change.
        if current_mode & 0o170000 != 0o100000 || current_mode == target_mode {
            continue;
        }
        let file_abs = workdir.join(file);
        let Ok(metadata) = std::fs::symlink_metadata(&file_abs) else {
            // The tracked path is gone: nothing to chmod.
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            // Only real regular files carry an executable bit.
            continue;
        }
        if !dry_run {
            // Rebuild the entry from the working-tree stat, keeping the existing
            // blob (no content change) and forcing the requested mode.
            let mut updated =
                IndexEntry::new_from_file(file, hash, &workdir).map_err(|source| {
                    AddError::CreateIndexEntry {
                        path: file.to_path_buf(),
                        source,
                    }
                })?;
            updated.mode = target_mode;
            index.update(updated);
        }
        let path_str = file.display().to_string();
        if !out.added.contains(&path_str) && !out.modified.contains(&path_str) {
            out.modified.push(path_str);
        }
    }
    Ok(())
}

/// Force-rewrite an already-tracked entry for `--renormalize`.
///
/// Re-reads the working-tree file, writes a fresh blob, and updates the index
/// entry — even when the content is unchanged (the point of `--renormalize`).
/// A tracked file that is gone from the working tree has its deletion staged; a
/// directory is a no-op.
fn renormalize_entry(
    file: &Path,
    index: &mut Index,
    workdir: &Path,
) -> Result<StagedAction, AddError> {
    let file_str = file.to_str().ok_or_else(|| AddError::InvalidPathEncoding {
        path: file.to_path_buf(),
    })?;
    let file_abs = workdir.join(file);
    // `symlink_metadata` does not follow symlinks, so a dangling symlink is still
    // detected as present (and not mistaken for a deleted file).
    let meta = match std::fs::symlink_metadata(&file_abs) {
        Ok(meta) => meta,
        Err(_) => {
            // Tracked but truly gone from the working tree: stage the deletion.
            index.remove(file_str, 0);
            return Ok(StagedAction::Removed);
        }
    };
    if meta.is_dir() {
        return Ok(StagedAction::Unchanged);
    }
    let blob = gen_blob_from_file(&file_abs).map_err(|source| AddError::CreateIndexEntry {
        path: file.to_path_buf(),
        source,
    })?;
    blob.save();
    index.update(
        IndexEntry::new_from_file(file, blob.id, workdir).map_err(|source| {
            AddError::CreateIndexEntry {
                path: file.to_path_buf(),
                source,
            }
        })?,
    );
    Ok(StagedAction::Modified)
}

/// Convert "all paths ignored, nothing staged" into a hard error.
///
/// Functional scope:
/// - When `output.ignored` is non-empty *and* nothing else was staged or
///   refreshed, builds an error message listing each ignored path and
///   attaches a hint to use `-f`.
/// - Otherwise returns the input unchanged.
///
/// Boundary conditions:
/// - Always passes through when [`AddOutput::is_empty`] is false, even if
///   some paths were ignored — those become warnings instead.
/// - Stable code is [`StableErrorCode::AddNothingStaged`].
fn check_ignored_only_error(output: AddOutput) -> CliResult<AddOutput> {
    if !output.ignored.is_empty() && output.is_empty() {
        let mut message =
            String::from("the following paths are ignored by configured ignore rules:");
        for path in &output.ignored {
            message.push('\n');
            message.push_str(path);
        }
        return Err(CliError::fatal(message)
            .with_stable_code(StableErrorCode::AddNothingStaged)
            .with_hint("use -f if you really want to add them."));
    }
    Ok(output)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Top-level dispatcher for the four output modes (JSON, quiet, dry-run,
/// refresh, normal).
///
/// Functional scope:
/// - Picks one body renderer based on flags and writes the result to stdout.
/// - Always emits warnings to stderr last, regardless of mode, so that users
///   who pipe stdout still see ignore/skip notices.
///
/// Boundary conditions:
/// - In quiet mode, stdout is suppressed entirely but stderr warnings still
///   flow.
/// - JSON mode bypasses stdout-locking and short-circuits with the structured
///   payload via [`output::emit_json_data`].
fn render_add_output(
    result: &AddOutput,
    output: &OutputConfig,
    verbose: bool,
    dry_run: bool,
) -> CliResult<()> {
    // JSON / machine mode
    if output.is_json() {
        return output::emit_json_data("add", result, output);
    }

    // Quiet mode: suppress stdout, but still emit warnings to stderr
    if output.quiet {
        render_warnings_stderr(result);
        return Ok(());
    }

    let stdout = io::stdout();
    let mut w = stdout.lock();

    if dry_run {
        render_dry_run(&mut w, result)?;
    } else if !result.refreshed.is_empty() {
        render_refresh(&mut w, result, verbose)?;
    } else {
        render_normal(&mut w, result, verbose)?;
    }

    // Warnings to stderr
    render_warnings_stderr(result);

    Ok(())
}

/// Render the `--dry-run` preview: one line per would-be-changed path,
/// suffixed with the explicit `(dry run, no files were staged)` footer.
fn render_dry_run(w: &mut impl Write, result: &AddOutput) -> CliResult<()> {
    for f in &result.added {
        writeln!(w, "add: {f}").map_err(write_err)?;
    }
    for f in &result.modified {
        writeln!(w, "add: {f}").map_err(write_err)?;
    }
    for f in &result.removed {
        writeln!(w, "remove: {f}").map_err(write_err)?;
    }
    for f in &result.refreshed {
        writeln!(w, "refresh: {f}").map_err(write_err)?;
    }
    writeln!(w, "(dry run, no files were staged)").map_err(write_err)?;
    Ok(())
}

/// Render the output of `--refresh`. In verbose mode each refreshed file is
/// printed; otherwise just a `refreshed N file(s)` summary is emitted.
fn render_refresh(w: &mut impl Write, result: &AddOutput, verbose: bool) -> CliResult<()> {
    if verbose {
        for f in &result.refreshed {
            writeln!(w, "refreshed: {f}").map_err(write_err)?;
        }
    }
    if result.refreshed.is_empty() {
        writeln!(w, "nothing to refresh").map_err(write_err)?;
    } else {
        let n = result.refreshed.len();
        let word = if n == 1 { "file" } else { "files" };
        writeln!(w, "refreshed {n} {word}").map_err(write_err)?;
    }
    Ok(())
}

/// Render the default text output: optional per-file lines (verbose) followed
/// by either a single-file message or a multi-file summary.
///
/// Boundary conditions:
/// - Returns [`CliError::internal`] if `total == 1` but every bucket is empty
///   — this is an internal invariant violation, not a user-visible state.
fn render_normal(w: &mut impl Write, result: &AddOutput, verbose: bool) -> CliResult<()> {
    let total = result.total_staged();

    if total == 0 {
        writeln!(w, "nothing to add").map_err(write_err)?;
        return Ok(());
    }

    // Verbose: per-file listing
    if verbose {
        for f in &result.added {
            writeln!(w, "add(new): {f}").map_err(write_err)?;
        }
        for f in &result.modified {
            writeln!(w, "add(modified): {f}").map_err(write_err)?;
        }
        for f in &result.removed {
            writeln!(w, "removed: {f}").map_err(write_err)?;
        }
    }

    // Summary line
    if total == 1 {
        let (path, kind) = if let Some(f) = result.added.first() {
            (f.as_str(), "new file")
        } else if let Some(f) = result.modified.first() {
            (f.as_str(), "modified")
        } else if let Some(f) = result.removed.first() {
            (f.as_str(), "removed")
        } else {
            return Err(CliError::internal(
                "single-file add summary is missing a staged path",
            ));
        };
        writeln!(w, "add '{path}' ({kind})").map_err(write_err)?;
    } else {
        let mut parts = Vec::new();
        if !result.added.is_empty() {
            parts.push(format!("{} new", result.added.len()));
        }
        if !result.modified.is_empty() {
            parts.push(format!("{} modified", result.modified.len()));
        }
        if !result.removed.is_empty() {
            parts.push(format!("{} removed", result.removed.len()));
        }
        writeln!(w, "add {total} files ({})", parts.join(", ")).map_err(write_err)?;
    }

    Ok(())
}

/// Emit the always-on warning footer: which paths were ignored, which paths
/// were skipped under `--ignore-errors`. Output goes to stderr so it survives
/// stdout redirection.
fn render_warnings_stderr(result: &AddOutput) {
    if !result.ignored.is_empty() {
        eprintln!("warning: the following paths are ignored by configured ignore rules:");
        for path in &result.ignored {
            eprintln!("{path}");
        }
        eprintln!();
        eprintln!("Hint: use -f if you really want to add them.");
        eprintln!("Hint: use 'libra restore --staged <file>' to unstage if needed");
    }
    if !result.failed.is_empty() {
        eprintln!(
            "warning: {} path(s) failed and were skipped (--ignore-errors):",
            result.failed.len()
        );
        for failure in &result.failed {
            eprintln!("  {}: {}", failure.path, failure.message);
        }
    }
    for pathspec in &result.missing {
        eprintln!(
            "warning: pathspec '{pathspec}' did not match any files and was skipped (--ignore-missing)"
        );
    }
}

/// Convert a `writeln!` failure into the standardized I/O [`CliError`] so the
/// caller does not need to repeat the format string at every call site.
fn write_err(e: io::Error) -> CliError {
    CliError::io(format!("failed to write add output: {e}"))
}

// ---------------------------------------------------------------------------
// Core staging logic
// ---------------------------------------------------------------------------

/// Resolve, canonicalise and classify each user-supplied pathspec.
///
/// Functional scope:
/// - When `raw_pathspecs` is empty, returns `requested_paths` unchanged
///   (caller passes the workdir as the implicit pathspec for `-A` / `-u`).
/// - For each pathspec, makes the path absolute, rejects anything outside
///   `workdir`, and probes three candidate sets in order: visible changes,
///   tracked files in the index, and ignored changes.
/// - Pathspecs that match only an ignored entry are returned in
///   [`ValidatedPathspecs::ignored`] so they can be reported as warnings.
///
/// Boundary conditions:
/// - Returns [`AddError::PathOutsideRepo`] for any pathspec resolving outside
///   the working tree (including via `..`).
/// - Returns [`AddError::PathspecNotMatched`] for the first pathspec that
///   matches no candidate at all — `--ignore-errors` does not affect this
///   pre-flight stage.
#[allow(clippy::too_many_arguments)]
fn validate_pathspecs(
    raw_pathspecs: &[String],
    pathspec_ctx: PathspecMatchContext<'_>,
    visible_changes: &Changes,
    ignored_changes: &Changes,
    index: &Index,
    ignore_missing: bool,
) -> Result<ValidatedPathspecs, AddError> {
    let pathspecs = PathspecSet::from_workdir_with_default_icase(
        raw_pathspecs,
        pathspec_ctx.current_dir,
        pathspec_ctx.workdir,
        pathspec_ctx.ignore_case,
    )
    .map_err(|source| AddError::Pathspec { source })?;

    let tracked_files = index.tracked_files();
    let change_candidates = collect_change_candidates(visible_changes);
    let ignored_candidates = collect_change_candidates(ignored_changes);
    let selectable_candidates = pathspec_candidates(&change_candidates, &tracked_files);
    let all_candidates = pathspec_candidates(&selectable_candidates, &ignored_candidates);

    let mut ignored = Vec::new();
    let mut missing = Vec::new();

    let unmatched_selectable = pathspecs.unmatched_positive_specs(&selectable_candidates);
    if !unmatched_selectable.is_empty() {
        let unmatched_all = pathspecs.unmatched_positive_specs(&all_candidates);
        for raw in unmatched_selectable {
            if !unmatched_all.contains(&raw) {
                ignored.push(raw.to_string());
                continue;
            }
            if ignore_missing {
                missing.push(raw.to_string());
                continue;
            }
            return Err(AddError::PathspecNotMatched {
                pathspec: raw.to_string(),
            });
        }
    }

    Ok(ValidatedPathspecs {
        pathspecs,
        ignored,
        missing,
    })
}

fn pathspec_candidates(left: &[PathBuf], right: &[PathBuf]) -> Vec<PathBuf> {
    let mut candidates = Vec::with_capacity(left.len() + right.len());
    candidates.extend(left.iter().cloned());
    candidates.extend(right.iter().cloned());
    candidates
}

/// Flatten the three change buckets (`new`, `modified`, `deleted`) into a
/// single ordered candidate list for pathspec matching.
fn collect_change_candidates(changes: &Changes) -> Vec<PathBuf> {
    let mut files = Vec::new();
    files.extend(changes.new.iter().cloned());
    files.extend(changes.modified.iter().cloned());
    files.extend(changes.deleted.iter().cloned());
    files
}

/// Make a user-supplied pathspec absolute by joining onto `current_dir` when
/// it is relative. Mirrors how Git's pathspec parser anchors specs to the
/// invoking shell's CWD rather than to the worktree root.
/// Restrict `files` (workdir-relative) to entries that fall under at least
/// one of the user's pathspecs. Used to scope `-A`/`-u`-derived candidate
/// sets to the explicit positional arguments.
fn filter_candidates(files: &[PathBuf], pathspecs: &PathspecSet) -> Vec<PathBuf> {
    files
        .iter()
        .filter(|file| pathspecs.matches_path(file.as_path()))
        .cloned()
        .collect()
}

/// Alias of [`filter_candidates`] used in `--refresh` mode. Kept separate so
/// future divergence in semantics (e.g. submodule handling) only needs to
/// touch one branch.
fn filter_refresh_candidates(files: &[PathBuf], pathspecs: &PathspecSet) -> Vec<PathBuf> {
    filter_candidates(files, pathspecs)
}

/// Remove the running `libra` binary from the candidate list.
///
/// Functional scope:
/// - Detects the executable via `current_exe` + `canonicalize`, and drops any
///   candidate whose absolute, canonicalised path matches.
///
/// Boundary conditions:
/// - Silent no-op when `current_exe()` or `canonicalize()` fail; we never
///   skip files based on speculative information.
/// - Important when running `libra add .` from inside a Libra checkout that
///   has compiled the binary into a tracked location (`target/`), which would
///   otherwise stage the freshly produced executable.
fn filter_out_current_executable(files: &mut Vec<PathBuf>) {
    if let Some(exe_path) = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok())
    {
        files.retain(|file| {
            util::try_workdir_to_absolute(file)
                .ok()
                .and_then(|path| path.canonicalize().ok())
                .is_none_or(|abs| abs != exe_path)
        });
    }
}

/// Refresh files and return the list of files actually refreshed.
///
/// Functional scope:
/// - Calls `Index::refresh` for each file. The underlying call returns
///   `true` only when the index entry's stat info actually changed; entries
///   whose mtime/size still match are silently skipped (and not added to the
///   returned vector).
///
/// Boundary conditions:
/// - The first refresh failure short-circuits the loop with
///   [`AddError::RefreshFailed`]; no rollback is performed on the index.
fn do_refresh_files(
    index: &mut Index,
    files: &[PathBuf],
    workdir: &Path,
) -> Result<Vec<PathBuf>, AddError> {
    let mut refreshed = Vec::new();
    for file in files {
        if index
            .refresh(file, workdir)
            .map_err(|source| AddError::RefreshFailed {
                path: file.clone(),
                source,
            })?
        {
            refreshed.push(file.clone());
        }
    }
    Ok(refreshed)
}

/// Stage a single file and return the action taken.
///
/// Functional scope:
/// - Translates the file's [`FileStatus`] into the corresponding index
///   mutation: writes a new blob and inserts an [`IndexEntry`] for `New`,
///   updates the entry only when the on-disk hash differs for `Modified`,
///   and removes the entry for `Deleted`.
/// - Skips files that live inside `storage_path` (the `.libra/` storage
///   directory) by returning `Unchanged` without touching the index.
///
/// Boundary conditions:
/// - `file` must be relative to `workdir`. Absolute paths or paths that
///   resolve outside the worktree return [`AddError::PathOutsideRepo`].
/// - Non-UTF-8 paths return [`AddError::InvalidPathEncoding`].
/// - LFS-tracked files are written as pointer blobs through
///   [`gen_blob_from_file`].
async fn stage_a_file(
    file: &Path,
    index: &mut Index,
    workdir: &Path,
    storage_path: &Path,
) -> Result<StagedAction, AddError> {
    let file_abs = workdir.join(file);
    if !util::is_sub_path(&file_abs, workdir) {
        return Err(AddError::PathOutsideRepo {
            path: file.display().to_string(),
            repo_root: workdir.to_path_buf(),
        });
    }
    if util::is_sub_path(&file_abs, storage_path) {
        return Ok(StagedAction::Unchanged);
    }

    let file_str = file.to_str().ok_or_else(|| AddError::InvalidPathEncoding {
        path: file.to_path_buf(),
    })?;

    // Skip real directories - symlinks to directories are staged as link blobs.
    if file_abs
        .symlink_metadata()
        .map(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Ok(StagedAction::Unchanged);
    }

    let file_status = check_file_status(file, index, workdir)?;
    match file_status {
        FileStatus::New => {
            let blob =
                gen_blob_from_file(&file_abs).map_err(|source| AddError::CreateIndexEntry {
                    path: file.to_path_buf(),
                    source,
                })?;
            blob.save();
            index.add(
                IndexEntry::new_from_file(file, blob.id, workdir).map_err(|source| {
                    AddError::CreateIndexEntry {
                        path: file.to_path_buf(),
                        source,
                    }
                })?,
            );
            Ok(StagedAction::Added)
        }
        FileStatus::Modified => {
            if index.is_modified(file_str, 0, workdir) {
                let blob =
                    gen_blob_from_file(&file_abs).map_err(|source| AddError::CreateIndexEntry {
                        path: file.to_path_buf(),
                        source,
                    })?;
                if !index.verify_hash(file_str, 0, &blob.id) {
                    blob.save();
                    index.update(IndexEntry::new_from_file(file, blob.id, workdir).map_err(
                        |source| AddError::CreateIndexEntry {
                            path: file.to_path_buf(),
                            source,
                        },
                    )?);
                    return Ok(StagedAction::Modified);
                }
            }
            Ok(StagedAction::Unchanged)
        }
        FileStatus::Deleted => {
            index.remove(file_str, 0);
            Ok(StagedAction::Removed)
        }
        FileStatus::Unchanged => Ok(StagedAction::Unchanged),
        FileStatus::NotFound => Err(AddError::PathspecNotMatched {
            pathspec: file.display().to_string(),
        }),
    }
}

/// Internal classification of a path relative to the index. Drives the
/// branching in [`stage_a_file`] and the dry-run preview in [`run_add`].
enum FileStatus {
    /// file is new
    New,
    /// file is modified
    Modified,
    /// file is deleted
    Deleted,
    /// file exists or is tracked but has nothing to stage
    Unchanged,
    /// file is not tracked
    NotFound,
}

/// Compute a [`FileStatus`] for `file` (relative to `workdir`) using the
/// in-memory `index`.
///
/// Functional scope:
/// - Uses `index.tracked` and `index.is_modified` to discriminate the four
///   live states; missing files are reported as `Deleted` when tracked, else
///   `NotFound`.
///
/// Boundary conditions:
/// - Returns [`AddError::InvalidPathEncoding`] when `file` is not UTF-8.
fn check_file_status(file: &Path, index: &Index, workdir: &Path) -> Result<FileStatus, AddError> {
    let file_str = file.to_str().ok_or_else(|| AddError::InvalidPathEncoding {
        path: file.to_path_buf(),
    })?;
    let file_abs = workdir.join(file);
    if file_abs.symlink_metadata().is_err() {
        if index.tracked(file_str, 0) {
            Ok(FileStatus::Deleted)
        } else {
            Ok(FileStatus::NotFound)
        }
    } else if !index.tracked(file_str, 0) {
        Ok(FileStatus::New)
    } else if index.is_modified(file_str, 0, workdir) {
        Ok(FileStatus::Modified)
    } else {
        Ok(FileStatus::Unchanged)
    }
}

/// Generate a `Blob` from a file.
///
/// Functional scope:
/// - Reads the exact bytes Git would store for a worktree blob: regular file
///   content, generated LFS pointer content, or symlink target bytes.
fn gen_blob_from_file(path: impl AsRef<Path>) -> io::Result<Blob> {
    read_worktree_blob_bytes(path).map(Blob::from_content_bytes)
}

#[cfg(test)]
mod test {
    use super::*;

    /// Pin the `Display` format for the static-message and direct-message
    /// variants of [`AddError`]. These strings are used as the `CliError`
    /// message via `From<AddError> for CliError` and surface in both
    /// human and `--json` envelopes.
    ///
    /// Source-chained variants (IndexLoad, IndexSave, RefreshFailed,
    /// CreateIndexEntry, Workdir, Status) wrap upstream error sources
    /// and are intentionally skipped — their `{source}` slot is owned
    /// by the wrapped error type.
    #[test]
    fn add_error_display_pins_static_message_variants() {
        assert_eq!(
            AddError::NotInRepo.to_string(),
            "not a libra repository (or any of the parent directories): .libra",
        );
        assert_eq!(
            AddError::PathspecNotMatched {
                pathspec: "src/missing.rs".to_string(),
            }
            .to_string(),
            "pathspec 'src/missing.rs' did not match any files",
        );
        assert_eq!(
            AddError::PathOutsideRepo {
                path: "/tmp/elsewhere".to_string(),
                repo_root: PathBuf::from("/home/user/repo"),
            }
            .to_string(),
            "'/tmp/elsewhere' is outside repository at '/home/user/repo'",
        );
        assert_eq!(
            AddError::InvalidPathEncoding {
                path: PathBuf::from("src/foo"),
            }
            .to_string(),
            "path 'src/foo' is not valid UTF-8",
        );
    }

    /// Scenario: clap should reject incompatible mode flags up front so the
    /// user gets a parse-time error rather than ambiguous staging behavior.
    /// The `mode` clap group ties `-A`, `-u`, and `--refresh` together.
    #[test]
    fn test_args_conflict_with_refresh() {
        // "--refresh" cannot be combined with "-A", "--refresh" or "-u"
        assert!(AddArgs::try_parse_from(["test", "-A", "--refresh"]).is_err());
        assert!(AddArgs::try_parse_from(["test", "-u", "--refresh"]).is_err());
        assert!(AddArgs::try_parse_from(["test", "-A", "-u", "--refresh"]).is_err());
    }

    /// Scenario: smoke-test `total_staged` and `is_empty` because every
    /// rendering branch keys off these helpers — a regression here would
    /// produce wrong summary lines or wrong "nothing to add" detection.
    #[test]
    fn add_output_total_and_empty() {
        let mut out = AddOutput::empty(false);
        assert!(out.is_empty());
        assert_eq!(out.total_staged(), 0);

        out.added.push("a.rs".to_string());
        assert_eq!(out.total_staged(), 1);
        assert!(!out.is_empty());
    }
}
