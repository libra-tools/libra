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
    collections::BTreeSet,
    env,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
};

use clap::Parser;
use git_internal::{
    errors::GitError,
    hash::ObjectHash,
    internal::{
        index::{Index, IndexEntry},
        object::{ObjectTrait, blob::Blob},
    },
};
use serde::Serialize;

use crate::{
    command::{
        diff::{DiffAlgorithm, compute_unified_hunks},
        read_worktree_blob_bytes,
        status::{self, Changes},
    },
    internal::{
        ai::automation::{VCS_EVENT_POST_ADD, dispatch_current_repo_vcs_event_to_history},
        patch_mode::{
            FileDiff, HunkUse, PatchApplyMode, SessionOptions, apply_selected_hunks_to_blob,
            parse_unified_diff, run_session_with,
        },
    },
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
    libra add --refresh                Refresh index metadata without staging
    libra add --resolved               Stage resolved unmerged paths
    libra add -p                       Interactively stage hunks";

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

    /// Read pathspecs from a file, one per line (or NUL-separated with
    /// `--pathspec-file-nul`). Use `-` to read the list from stdin. Cannot be
    /// combined with `-p`/`--patch`, interactive mode, or pathspec arguments.
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

    /// Under `--dry-run`, classify pathspecs that match no add candidate against
    /// the configured ignore rules: ignored patterns are reported and make the
    /// run exit non-zero; others are skipped with a warning. Mirrors Git's
    /// `add --ignore-missing`, which requires `--dry-run`.
    #[clap(long = "ignore-missing", requires = "dry_run")]
    pub ignore_missing: bool,

    /// Stage only unmerged (conflict) paths after the working-tree copies have
    /// been resolved. Refuses to run together with `-u`/`-A`. Does not require
    /// a pathspec; a pathspec, when given, limits which unmerged paths are
    /// considered. Mirrors Git's `add --resolved`.
    #[clap(long)]
    pub resolved: bool,

    /// Interactively choose hunks to stage (`add -p`).
    #[clap(short = 'p', long = "patch")]
    pub patch: bool,

    /// Auto-advance after each hunk decision (the `add -p` default). Last
    /// one wins against `--no-auto-advance`.
    #[clap(long = "auto-advance", overrides_with = "no_auto_advance")]
    pub auto_advance: bool,

    /// Stay on the current hunk after `y`/`n` and enable `>`/`<` file
    /// navigation. Requires `-p`.
    #[clap(long = "no-auto-advance", overrides_with = "auto_advance")]
    pub no_auto_advance: bool,
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
    /// `add -u` pathspec named an untracked working-tree path that is not in
    /// the index (any stage). Distinct from [`Self::PathspecNotMatched`] so
    /// callers can tell "does not exist" from "exists but is not tracked".
    #[error("pathspec '{pathspec}' did not match any file(s) known to the index")]
    PathspecNotKnownToIndex { pathspec: String },
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
    /// The blob payload was written, but durable local bookkeeping for the
    /// cloud object index could not be registered.
    #[error("failed to store object for '{path}': {source}")]
    ObjectSave { path: PathBuf, source: io::Error },
    /// Batch publication of cloud object-index repair markers failed after
    /// the payloads were stored (ADR-OI-04 / M-BATCH B2).
    #[error("failed to store object: {source}")]
    ObjectIndexBatchFlush {
        stored_objects: usize,
        source: io::Error,
    },
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
            AddError::PathspecNotMatched { .. } | AddError::PathspecNotKnownToIndex { .. } => {
                CliError::fatal(error.to_string())
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_hint("check the path and try again.")
                    .with_hint("use 'libra status' to inspect tracked and untracked files.")
            }
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
            AddError::ObjectSave { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::IoWriteFailed)
                .with_detail("stored_objects", 1usize)
                .with_detail("staged", 0usize),
            AddError::ObjectIndexBatchFlush { stored_objects, .. } => {
                CliError::fatal(error.to_string())
                    .with_stable_code(StableErrorCode::IoWriteFailed)
                    .with_detail("stored_objects", *stored_objects as u64)
                    .with_detail("staged", 0usize)
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

/// One entry in [`AddOutput::chmod_rejected`]: a path whose index entry is not
/// a regular file (symlink `120000` / gitlink `160000`), so `--chmod` cannot
/// set or clear its executable bit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChmodRejection {
    pub path: String,
    /// The requested flip — `"+x"` or `"-x"`.
    pub flip: String,
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
    /// Paths refused under `--chmod` because their index entry is not a
    /// regular file (symlink/gitlink). Reported in text mode as
    /// `error: cannot chmod …` lines and in JSON as `chmod_rejected`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chmod_rejected: Vec<ChmodRejection>,
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
            chmod_rejected: Vec::new(),
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
#[derive(Debug)]
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

    // ADR-PSF-03: `--pathspec-from-file` cannot be combined with an interactive
    // mode or with command-line pathspec arguments — Git refuses both with
    // `cannot be used together` before any write. (`--edit`/`--interactive`
    // keep their own declined-flag refusal, which fires at parse time before
    // this gate.)
    if args.pathspec_from_file.is_some() {
        if args.patch {
            return Err(CliError::command_usage(
                "options '--pathspec-from-file' and '-p/--patch' cannot be used together",
            ));
        }
        if !args.pathspec.is_empty() {
            return Err(CliError::command_usage(
                "'--pathspec-from-file' and pathspec arguments cannot be used together",
            ));
        }
    }

    // If --pathspec-from-file is specified, read and merge pathspecs.
    if let Some(file) = args.pathspec_from_file.take() {
        // ADR-PSF-01: the value `-` reads the list from stdin (never a worktree
        // file literally named `-`); anything else is a file path.
        let data = if file == PATHSPEC_FROM_FILE_STDIN {
            read_pathspec_stdin()?
        } else {
            std::fs::read(&file).map_err(|e| {
                CliError::fatal(format!("cannot read pathspec file '{}': {}", file, e))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })?
        };
        args.pathspec
            .extend(parse_pathspec_file(&data, args.pathspec_file_nul)?);
    }

    if (args.no_auto_advance || args.auto_advance) && !args.patch {
        let option = if args.no_auto_advance {
            "--no-auto-advance"
        } else {
            "--auto-advance"
        };
        return Err(CliError::fatal(format!(
            "the option '{option}' requires '--interactive/--patch'"
        ))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    if args.patch && args.resolved {
        return Err(CliError::command_usage(
            "options '--resolved' and '-p/--patch' cannot be used together",
        ));
    }
    if args.patch && (output.is_json() || args.dry_run) {
        return Err(patch_machine_mode_error(output, args.dry_run));
    }

    // ADR-OI-04: accumulate marker publication for the whole staging pass.
    // `run_add` flushes right before each index write so durable markers
    // never lag behind index content; the end flush publishes any remainder.
    util::objects_storage().begin_object_index_batch();
    let result = match run_add(&args).await {
        Ok(result) => {
            let batch_storage = util::objects_storage();
            let stored_objects = batch_storage.pending_object_index_batch_count();
            batch_storage.end_object_index_batch().map_err(|source| {
                AddError::ObjectIndexBatchFlush {
                    stored_objects,
                    source,
                }
            })?;
            result
        }
        Err(error) => {
            util::objects_storage().abort_object_index_batch();
            return Err(error);
        }
    };

    if args.patch {
        if result.wrote_index() {
            dispatch_current_repo_vcs_event_to_history(VCS_EVENT_POST_ADD).await;
        }
        return Ok(());
    }

    // --- Render output ---
    render_add_output(&result, output, verbose, dry_run)?;

    // --- Warning tracking for ignored / partial failures / skipped pathspecs ---
    if !result.ignored.is_empty() || !result.failed.is_empty() || !result.missing.is_empty() {
        output::record_warning();
    }
    if result.wrote_index() {
        dispatch_current_repo_vcs_event_to_history(VCS_EVENT_POST_ADD).await;
    }

    // ADR-CH-02: a `--chmod` refusal exits 1 after rendering, warning
    // tracking, and event dispatch — the same "render then non-zero" model as
    // the ignored report. Text mode prints one `error: cannot chmod …` line
    // per refused path; JSON carries `chmod_rejected` on the envelope instead.
    if !result.chmod_rejected.is_empty() {
        if !output.is_json() {
            for rejection in &result.chmod_rejected {
                eprintln!(
                    "error: cannot chmod {} '{}'",
                    rejection.flip, rejection.path
                );
            }
        }
        return Err(CliError::silent_exit(1));
    }

    // ADR-IA-02 item 1 / M-EXIT E1-E2: a mixed ignored report exits 1 after
    // full rendering, warning tracking, and event dispatch (Git parity).
    // "Only ignored" forms never reach this point: `check_ignored_only_error`
    // returns `LBR-ADD-001` / 128 from `run_add` instead.
    if !result.ignored.is_empty() {
        return Err(CliError::silent_exit(1));
    }

    Ok(())
}

/// Git's `--pathspec-from-file` stdin sentinel and the bounded-stdin cap
/// (ADR-PSF-01).
const PATHSPEC_FROM_FILE_STDIN: &str = "-";
const PATHSPEC_FROM_FILE_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Read a `--pathspec-from-file=-` list from stdin, bounded. Any failure is a
/// hard `LBR-IO-001` error with zero writes (ADR-PSF-01 item 4).
fn read_pathspec_stdin() -> CliResult<Vec<u8>> {
    let mut data = Vec::new();
    io::stdin()
        .lock()
        .take(PATHSPEC_FROM_FILE_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut data)
        .map_err(|error| {
            CliError::fatal(format!("cannot read pathspec list from stdin: {error}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
    if data.len() as u64 > PATHSPEC_FROM_FILE_MAX_BYTES {
        return Err(CliError::fatal(format!(
            "pathspec list from stdin exceeds {PATHSPEC_FROM_FILE_MAX_BYTES} bytes"
        ))
        .with_stable_code(StableErrorCode::IoReadFailed));
    }
    Ok(data)
}

/// Decode a `--pathspec-from-file` payload (ADR-PSF-01/02): NUL mode splits on
/// `0` and keeps every byte; otherwise lines split on `\n` with one trailing
/// `\r` stripped and one C-style quoted line decoded (Git's `unquote_c_style`).
/// Empty entries are dropped; a non-UTF-8 entry or malformed quoting is a hard
/// `LBR-IO-001` failure rather than a silent skip. The quote decoding is the
/// shared [`crate::utils::text::decode_c_quoted`] helper — no second state
/// machine lives here.
fn parse_pathspec_file(data: &[u8], nul: bool) -> CliResult<Vec<String>> {
    let mut pathspecs = Vec::new();
    if nul {
        // NUL mode: split on `0` and keep every byte (CR included).
        for entry in data.split(|byte| *byte == 0) {
            if let Some(text) = parse_pathspec_entry(entry, false)? {
                pathspecs.push(text);
            }
        }
    } else {
        // LF mode: one trailing CR is stripped only when it precedes the
        // terminating LF — an unterminated final segment keeps its bytes
        // verbatim (Git parity, PSF-01 review P1-1).
        for raw in data.split_inclusive(|byte| *byte == b'\n') {
            let entry = match raw.strip_suffix(b"\n") {
                Some(line) => line.strip_suffix(b"\r").unwrap_or(line),
                None => raw,
            };
            if let Some(text) = parse_pathspec_entry(entry, true)? {
                pathspecs.push(text);
            }
        }
    }
    Ok(pathspecs)
}

/// Decode one `--pathspec-from-file` entry: drop empty entries, reject
/// non-UTF-8, and (newline mode only) decode one C-style quoted line
/// (ADR-PSF-02). An empty C-quoted string is dropped rather than becoming the
/// whole-tree pathspec (PSF-02 review P1-1: fail closed, never stage
/// everything).
fn parse_pathspec_entry(entry: &[u8], decode_quotes: bool) -> CliResult<Option<String>> {
    if entry.is_empty() {
        return Ok(None);
    }
    let text = std::str::from_utf8(entry).map_err(|_| {
        CliError::fatal("pathspec list contains a non-UTF-8 entry".to_string())
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    if !decode_quotes {
        return Ok(Some(text.to_string()));
    }
    match crate::utils::text::decode_c_quoted(text) {
        Ok(Some(decoded)) => {
            if decoded.is_empty() {
                Ok(None)
            } else {
                Ok(Some(decoded))
            }
        }
        Ok(None) => Ok(Some(text.to_string())),
        Err(reason) => Err(CliError::fatal(format!(
            "line is badly quoted in --pathspec-from-file: {reason}: {text}"
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)),
    }
}

const CONFLICT_MARKER_SIZE: usize = 7;

/// Git `die_for_incompatible_opt3` for `-u` / `-A` / `--resolved`.
/// Kept out of the clap `mode` group so the diagnostic uses Git's
/// `cannot be used together` wording instead of clap's `cannot be used with`.
fn resolved_option_conflict(args: &AddArgs) -> Option<CliError> {
    if !args.resolved {
        return None;
    }
    if args.update && args.all {
        return Some(CliError::command_usage(
            "options '-u/--update', '-A/--all' and '--resolved' cannot be used together",
        ));
    }
    if args.update {
        return Some(CliError::command_usage(
            "options '-u/--update' and '--resolved' cannot be used together",
        ));
    }
    if args.all {
        return Some(CliError::command_usage(
            "options '-A/--all' and '--resolved' cannot be used together",
        ));
    }
    None
}

fn collect_unmerged_paths(index: &Index) -> Vec<String> {
    let mut paths = BTreeSet::new();
    for stage in 1..=3 {
        for entry in index.tracked_entries(stage) {
            paths.insert(entry.name.clone());
        }
    }
    paths.into_iter().collect()
}

fn path_has_conflict_stages(index: &Index, name: &str) -> bool {
    (1..=3).any(|stage| index.tracked(name, stage))
}

fn clear_conflict_stages(index: &mut Index, name: &str) {
    for stage in 1..=3 {
        index.remove(name, stage);
    }
}

fn pathspec_looks_like_glob(raw: &str) -> bool {
    raw.contains('*') || raw.contains('?') || raw.contains('[')
}

/// Whether default (non-verbose, non-dry-run) add summaries should go to
/// stdout. Non-terminal stdout is silent, matching Git; tests default to
/// silent unless `LIBRA_ADD_TTY` is set (same idea as the pager's
/// `LIBRA_TEST` gate).
fn stdout_is_tty_for_add() -> bool {
    if env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_some() {
        return env::var_os("LIBRA_ADD_TTY").is_some();
    }
    io::stdout().is_terminal()
}

fn index_paths_any_stage(index: &Index) -> Vec<PathBuf> {
    let mut paths = BTreeSet::new();
    for stage in 0..=3 {
        for entry in index.tracked_entries(stage) {
            paths.insert(PathBuf::from(&entry.name));
        }
    }
    paths.into_iter().collect()
}

fn remove_all_stages(index: &mut Index, name: &str) {
    for stage in 0..=3 {
        index.remove(name, stage);
    }
}

/// Git `merge-ll.c:is_conflict_marker_line` with a fixed marker size of 7.
fn is_conflict_marker_line(line: &[u8]) -> bool {
    if line.len() < CONFLICT_MARKER_SIZE + 1 {
        return false;
    }
    let first = line[0];
    if !matches!(first, b'=' | b'>' | b'<' | b'|') {
        return false;
    }
    if line[1..CONFLICT_MARKER_SIZE]
        .iter()
        .any(|&byte| byte != first)
    {
        return false;
    }
    let after = line[CONFLICT_MARKER_SIZE];
    if matches!(first, b'<' | b'>') && after != b' ' {
        return false;
    }
    after.is_ascii_whitespace()
}

fn line_is_binary(line: &[u8]) -> bool {
    line.contains(&0)
}

/// Git `merge-ll.c:has_conflict_markers` without `conflict-marker-size`.
fn file_has_conflict_markers(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let mut start = 0usize;
    for i in 0..=bytes.len() {
        if i != bytes.len() && bytes[i] != b'\n' {
            continue;
        }
        let end = if i < bytes.len() { i + 1 } else { i };
        let line = &bytes[start..end];
        if is_conflict_marker_line(line) {
            return true;
        }
        if line_is_binary(line) {
            return false;
        }
        start = i.saturating_add(1);
    }
    false
}

fn worktree_regular_file_has_markers(workdir: &Path, rel: &str) -> bool {
    let abs = workdir.join(rel);
    let Ok(meta) = abs.symlink_metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    file_has_conflict_markers(&abs)
}

fn conflict_markers_error(paths: &[String]) -> CliError {
    let listing = paths
        .iter()
        .map(|path| format!("\t{path}"))
        .collect::<Vec<_>>()
        .join("\n");
    CliError::fatal(format!(
        "the following paths still have conflict markers:\n{listing}"
    ))
    .with_stable_code(StableErrorCode::ConflictUnresolved)
}

fn stage_resolved_path(
    file: &str,
    index: &mut Index,
    workdir: &Path,
    storage_path: &Path,
) -> Result<StagedAction, AddError> {
    let rel = Path::new(file);
    let file_abs = workdir.join(rel);
    if !util::is_sub_path(&file_abs, workdir) {
        return Err(AddError::PathOutsideRepo {
            path: file.to_string(),
            repo_root: workdir.to_path_buf(),
        });
    }
    if util::is_sub_path(&file_abs, storage_path) {
        return Ok(StagedAction::Unchanged);
    }

    match file_abs.symlink_metadata() {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            remove_all_stages(index, file);
            Ok(StagedAction::Removed)
        }
        Err(source) => Err(AddError::CreateIndexEntry {
            path: rel.to_path_buf(),
            source,
        }),
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => Ok(StagedAction::Unchanged),
        Ok(_) => {
            let pre_read = file_abs.symlink_metadata().ok();
            let blob =
                gen_blob_from_file(&file_abs).map_err(|source| AddError::CreateIndexEntry {
                    path: rel.to_path_buf(),
                    source,
                })?;
            blob.try_save().map_err(|source| AddError::ObjectSave {
                path: rel.to_path_buf(),
                source,
            })?;
            let entry =
                crate::command::verified_index_entry(rel, blob.id, workdir, pre_read.as_ref())
                    .map_err(|source| AddError::CreateIndexEntry {
                        path: rel.to_path_buf(),
                        source,
                    })?;
            for stage in 1..=3 {
                index.remove(file, stage);
            }
            index.add(entry);
            Ok(StagedAction::Modified)
        }
    }
}

fn patch_machine_mode_error(output: &OutputConfig, dry_run: bool) -> CliError {
    if dry_run {
        CliError::command_usage("options '--dry-run' and '-p/--patch' cannot be used together")
    } else if output.is_json() {
        CliError::command_usage(
            "options '--json'/'--machine' and '-p/--patch' cannot be used together",
        )
    } else {
        CliError::command_usage("patch mode cannot be used with machine-readable output")
    }
}

struct PatchCandidate {
    file: FileDiff,
    old_bytes: Vec<u8>,
}

fn patch_bytes_are_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0)
}

fn abbrev7(hash: &ObjectHash) -> String {
    let hex = hash.to_string();
    hex.chars().take(7).collect()
}

fn worktree_index_mode(path: &Path) -> Option<u32> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() {
        return Some(0o120000);
    }
    if !meta.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 != 0 {
            Some(0o100755)
        } else {
            Some(0o100644)
        }
    }
    #[cfg(not(unix))]
    {
        Some(0o100644)
    }
}

fn load_index_blob_bytes(hash: &ObjectHash, path: &str) -> Result<Vec<u8>, AddError> {
    let storage = util::objects_storage();
    let data = storage.get(hash).map_err(|source| AddError::ObjectSave {
        path: PathBuf::from(path),
        source: io::Error::other(source.to_string()),
    })?;
    let blob = Blob::from_bytes(&data, *hash).map_err(|source| AddError::ObjectSave {
        path: PathBuf::from(path),
        source: io::Error::other(format!("{source:?}")),
    })?;
    Ok(blob.data)
}

fn build_patch_header(
    path: &str,
    old_hash: &ObjectHash,
    new_hash: Option<&ObjectHash>,
    old_mode: u32,
    new_mode: Option<u32>,
    deleted: bool,
    binary: bool,
) -> String {
    let mut header = format!("diff --git a/{path} b/{path}\n");
    if deleted {
        header.push_str(&format!("deleted file mode {old_mode:06o}\n"));
        header.push_str(&format!("index {}..0000000\n", abbrev7(old_hash)));
        header.push_str(&format!("--- a/{path}\n+++ /dev/null\n"));
        return header;
    }
    let new_mode = new_mode.unwrap_or(old_mode);
    if old_mode != new_mode {
        header.push_str(&format!("old mode {old_mode:06o}\n"));
        header.push_str(&format!("new mode {new_mode:06o}\n"));
        header.push_str(&format!(
            "index {}..{}\n",
            abbrev7(old_hash),
            new_hash.map(abbrev7).unwrap_or_else(|| "0000000".into())
        ));
    } else {
        header.push_str(&format!(
            "index {}..{} {old_mode:06o}\n",
            abbrev7(old_hash),
            new_hash.map(abbrev7).unwrap_or_else(|| "0000000".into())
        ));
    }
    if binary {
        header.push_str(&format!("Binary files a/{path} and b/{path} differ\n"));
    } else {
        header.push_str(&format!("--- a/{path}\n+++ b/{path}\n"));
    }
    header
}

fn collect_patch_candidate(
    rel: &Path,
    index: &Index,
    workdir: &Path,
) -> Result<Option<PatchCandidate>, AddError> {
    let path = rel.to_str().ok_or_else(|| AddError::InvalidPathEncoding {
        path: rel.to_path_buf(),
    })?;
    let Some(entry) = index.get(path, 0) else {
        return Ok(None);
    };
    let old_bytes = load_index_blob_bytes(&entry.hash, path)?;
    let abs = workdir.join(rel);
    let deleted = !abs.exists();
    let new_bytes = if deleted {
        Vec::new()
    } else {
        read_worktree_blob_bytes(&abs).map_err(|source| AddError::CreateIndexEntry {
            path: rel.to_path_buf(),
            source,
        })?
    };
    let new_mode = if deleted {
        None
    } else {
        worktree_index_mode(&abs)
    };
    let binary = patch_bytes_are_binary(&old_bytes) || patch_bytes_are_binary(&new_bytes);
    let new_blob = if binary {
        None
    } else {
        Some(Blob::from_content_bytes(new_bytes.clone()))
    };
    let header = build_patch_header(
        path,
        &entry.hash,
        new_blob.as_ref().map(|blob| &blob.id),
        entry.mode,
        new_mode,
        deleted,
        binary,
    );
    if binary {
        return Ok(Some(PatchCandidate {
            file: FileDiff {
                path: path.to_string(),
                header,
                old_mode: Some(entry.mode),
                new_mode,
                added: false,
                deleted,
                mode_change: new_mode.is_some_and(|mode| mode != entry.mode),
                binary: true,
                hunks: Vec::new(),
            },
            old_bytes,
        }));
    }
    let old_text = String::from_utf8(old_bytes.clone()).ok();
    let new_text = String::from_utf8(new_bytes).ok();
    let (Some(old_text), Some(new_text)) = (old_text, new_text) else {
        return Ok(Some(PatchCandidate {
            file: FileDiff {
                path: path.to_string(),
                header,
                old_mode: Some(entry.mode),
                new_mode,
                added: false,
                deleted,
                mode_change: new_mode.is_some_and(|mode| mode != entry.mode),
                binary: true,
                hunks: Vec::new(),
            },
            old_bytes,
        }));
    };
    let hunk_body = if old_text == new_text {
        String::new()
    } else {
        compute_unified_hunks(&old_text, &new_text, 3, &DiffAlgorithm::Myers)
    };
    if hunk_body.is_empty() && !deleted && new_mode.is_none_or(|mode| mode == entry.mode) {
        return Ok(None);
    }
    let mut patch = header;
    patch.push_str(&hunk_body);
    let mut files = parse_unified_diff(&patch).map_err(|source| AddError::ObjectSave {
        path: rel.to_path_buf(),
        source: io::Error::other(source.to_string()),
    })?;
    let Some(file) = files.pop() else {
        return Ok(None);
    };
    Ok(Some(PatchCandidate { file, old_bytes }))
}

async fn run_add_patch(
    args: &AddArgs,
    workdir: &Path,
    index_path: &Path,
    storage_path: &Path,
    layer_scope: &crate::internal::worktree_scope::WorktreeScope,
    pathspec_ctx: PathspecMatchContext<'_>,
    mut index: Index,
) -> CliResult<AddOutput> {
    let (visible_changes, _ignored_changes) =
        status::changes_to_be_staged_split_safe_with_ignore_case(pathspec_ctx.ignore_case)
            .map_err(|source| AddError::Status { source })?;
    let validated = validate_pathspecs(
        &args.pathspec,
        pathspec_ctx,
        &visible_changes,
        &Changes::default(),
        &index,
        false,
        false,
        true,
        false,
    )?;
    let mut files = visible_changes.modified;
    files.extend(visible_changes.deleted);
    let mut files = filter_candidates(&files, &validated.pathspecs);
    for tracked in index.tracked_files() {
        if !validated.pathspecs.matches_path(&tracked) || files.contains(&tracked) {
            continue;
        }
        let Some(path) = tracked.to_str() else {
            continue;
        };
        let Some(entry) = index.get(path, 0) else {
            continue;
        };
        let Some(mode) = worktree_index_mode(&workdir.join(&tracked)) else {
            continue;
        };
        if mode != entry.mode {
            files.push(tracked);
        }
    }
    filter_out_current_executable(&mut files);
    files.sort();
    files.dedup();

    crate::internal::layer::verify_staging_context(workdir, layer_scope)?;
    let owned: std::collections::HashSet<String> =
        crate::internal::layer::LayerStore::owned_path_set_strict(layer_scope)
            .await
            .map_err(|e| {
                CliError::fatal(format!(
                    "cannot verify layer-owned paths before staging: {e}"
                ))
                .with_stable_code(StableErrorCode::IoReadFailed)
            })?
            .into_iter()
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

    let mut candidates = Vec::new();
    for file in &files {
        if util::is_sub_path(workdir.join(file), storage_path) {
            continue;
        }
        if let Some(candidate) = collect_patch_candidate(file, &index, workdir)? {
            candidates.push(candidate);
        }
    }
    candidates.sort_by(|a, b| a.file.path.as_bytes().cmp(b.file.path.as_bytes()));

    let mut session_files: Vec<FileDiff> = candidates.iter().map(|c| c.file.clone()).collect();
    {
        let stdin = io::stdin();
        let mut input = stdin.lock();
        let mut stdout = io::stdout();
        let editor = crate::command::editor::resolve_editor().await;
        let edit_path = storage_path.join("ADD_EDIT.patch");
        let index_blobs = candidates.iter().map(|c| c.old_bytes.clone()).collect();
        run_session_with(
            &mut session_files,
            &mut input,
            &mut stdout,
            SessionOptions {
                auto_advance: !args.no_auto_advance,
                editor,
                edit_path: Some(edit_path.clone()),
                index_blobs,
                kind: crate::internal::patch_mode::PatchSessionKind::Stage,
            },
        )
        .map_err(|source| {
            CliError::fatal(format!("failed to read patch-mode input: {source}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        let _ = std::fs::remove_file(&edit_path);
    }

    let mut add_output = AddOutput::empty(false);
    let mut pending: Vec<(usize, crate::internal::patch_mode::AppliedIndexBlob)> = Vec::new();
    for (i, file) in session_files.iter().enumerate() {
        let decided = file
            .hunks
            .iter()
            .any(|hunk| hunk.use_decision == HunkUse::Use)
            || (file.mode_change
                && file
                    .hunks
                    .iter()
                    .any(|hunk| hunk.use_decision == HunkUse::Use));
        if !decided {
            continue;
        }
        let applied =
            apply_selected_hunks_to_blob(&candidates[i].old_bytes, file, PatchApplyMode::Stage)
                .map_err(|source| {
                    CliError::fatal(source.to_string())
                        .with_stable_code(StableErrorCode::RepoStateInvalid)
                })?;
        pending.push((i, applied));
    }
    if pending.is_empty() {
        return Ok(add_output);
    }

    let lock_paths: Vec<String> = pending
        .iter()
        .map(|(i, _)| session_files[*i].path.clone())
        .collect();
    crate::command::lfs::enforce_lock_policy(&lock_paths)
        .await
        .map_err(AddError::LockPolicy)?;

    for (i, applied) in pending {
        let path = &session_files[i].path;
        match applied.bytes {
            None => {
                index.remove(path, 0);
                add_output.removed.push(path.clone());
            }
            Some(bytes) => {
                let blob = Blob::from_content_bytes(bytes);
                blob.try_save().map_err(|source| AddError::ObjectSave {
                    path: PathBuf::from(path),
                    source,
                })?;
                let mut entry =
                    IndexEntry::new_from_blob(path.clone(), blob.id, blob.data.len() as u32);
                if let Some(mode) = applied.mode {
                    entry.mode = mode;
                }
                index.update(entry);
                add_output.modified.push(path.clone());
            }
        }
    }
    let batch_storage = util::objects_storage();
    let stored_objects = batch_storage.pending_object_index_batch_count();
    batch_storage
        .flush_object_index_batch()
        .map_err(|source| AddError::ObjectIndexBatchFlush {
            stored_objects,
            source,
        })?;
    index
        .save(index_path)
        .map_err(|source| AddError::IndexSave {
            path: index_path.to_path_buf(),
            source,
        })?;
    Ok(add_output)
}

async fn run_add_resolved(
    args: &AddArgs,
    workdir: &Path,
    index_path: &Path,
    storage_path: &Path,
    layer_scope: &crate::internal::worktree_scope::WorktreeScope,
    pathspec_ctx: PathspecMatchContext<'_>,
    mut index: Index,
) -> CliResult<AddOutput> {
    let pathspecs = PathspecSet::from_workdir_with_default_icase(
        &args.pathspec,
        pathspec_ctx.current_dir,
        pathspec_ctx.workdir,
        pathspec_ctx.ignore_case,
    )
    .map_err(|source| AddError::Pathspec { source })?;

    if !args.pathspec.is_empty() {
        let index_paths = index_paths_any_stage(&index);
        if let Some(raw) = pathspecs.unmatched_positive(&index_paths)
            && !args.ignore_missing
        {
            return Err(CliError::from(AddError::PathspecNotMatched {
                pathspec: raw.to_string(),
            }));
        }
    }

    let unmerged = collect_unmerged_paths(&index);
    let files: Vec<String> = unmerged
        .into_iter()
        .filter(|path| pathspecs.matches_path(Path::new(path)))
        .collect();

    let mut add_output = AddOutput::empty(args.dry_run);
    if files.is_empty() {
        return Ok(add_output);
    }

    let leftover: Vec<String> = files
        .iter()
        .filter(|path| worktree_regular_file_has_markers(workdir, path))
        .cloned()
        .collect();
    if !leftover.is_empty() {
        return Err(conflict_markers_error(&leftover));
    }

    crate::internal::layer::verify_staging_context(workdir, layer_scope)?;
    let owned: std::collections::HashSet<String> =
        crate::internal::layer::LayerStore::owned_path_set_strict(layer_scope)
            .await
            .map_err(|e| {
                CliError::fatal(format!(
                    "cannot verify layer-owned paths before staging: {e}"
                ))
                .with_stable_code(StableErrorCode::IoReadFailed)
            })?
            .into_iter()
            .collect();
    if !owned.is_empty() {
        let blocked: Vec<String> = files
            .iter()
            .filter(|path| owned.contains(path.as_str()))
            .cloned()
            .collect();
        if let Some(first) = blocked.first() {
            return Err(CliError::from(AddError::LayerPath {
                path: first.clone(),
                count: blocked.len(),
            }));
        }
    }

    if !args.dry_run {
        crate::command::lfs::enforce_lock_policy(&files)
            .await
            .map_err(AddError::LockPolicy)?;
    }

    if args.dry_run {
        for file in &files {
            match workdir.join(file).symlink_metadata() {
                Err(_) => add_output.removed.push(file.clone()),
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
                Ok(_) => add_output.modified.push(file.clone()),
            }
        }
        return Ok(add_output);
    }

    for file in &files {
        match stage_resolved_path(file, &mut index, workdir, storage_path) {
            Ok(action) => match action {
                StagedAction::Modified => add_output.modified.push(file.clone()),
                StagedAction::Removed => add_output.removed.push(file.clone()),
                StagedAction::Added | StagedAction::Unchanged => {}
            },
            Err(err) => {
                if !args.ignore_errors {
                    return Err(CliError::from(err));
                }
                add_output.failed.push(AddFailure {
                    path: file.clone(),
                    message: err.to_string(),
                });
            }
        }
    }
    let batch_storage = util::objects_storage();
    let stored_objects = batch_storage.pending_object_index_batch_count();
    batch_storage
        .flush_object_index_batch()
        .map_err(|source| AddError::ObjectIndexBatchFlush {
            stored_objects,
            source,
        })?;

    index
        .save(index_path)
        .map_err(|source| AddError::IndexSave {
            path: index_path.to_path_buf(),
            source,
        })?;

    Ok(add_output)
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
    if let Some(err) = resolved_option_conflict(args) {
        return Err(err);
    }

    let workdir = util::try_working_dir().map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            AddError::NotInRepo
        } else {
            AddError::Workdir { source }
        }
    })?;
    // lore.md 2.4: load the layer-overlay exclusion snapshot so the sync
    // ignore resolver skips layer-owned paths (a no-op with no layers).
    // W1 §C.4.1.1: the scope is derived from the CAPTURED workdir (not the
    // ambient cwd) so it stays bound to the tree this request stages into —
    // resolved ONCE and reused by the staging guard below.
    let layer_scope = crate::internal::worktree_scope::WorktreeScope::for_workdir(&workdir);
    crate::internal::layer::refresh_exclusion_snapshot(&layer_scope).await;
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

    // ADR-CH-05: `--chmod` with no pathspec (and no whole-tree selector) is a
    // successful no-op — there is nothing to apply the mode to, and Git exits 0
    // without writing. Short-circuit before the empty-pathspec usage gate below,
    // which exists precisely because an empty spec would otherwise match the
    // whole tree.
    if args.pathspec.is_empty()
        && args.chmod.is_some()
        && !args.all
        && !args.update
        && !args.refresh
        && !args.renormalize
        && !args.resolved
        && !args.patch
    {
        return Ok(AddOutput::empty(args.dry_run));
    }

    // Resolve pathspecs. `--renormalize` implies `-u` (tracked-only), so it also
    // permits an empty pathspec (operate on the whole tracked set). `--resolved`
    // likewise does not require a pathspec: it operates on unmerged index paths.
    if args.pathspec.is_empty()
        && !args.all
        && !args.update
        && !args.refresh
        && !args.renormalize
        && !args.resolved
        && !args.patch
    {
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

    if args.patch {
        return run_add_patch(
            args,
            &workdir,
            &index_path,
            &storage_path,
            &layer_scope,
            pathspec_ctx,
            index,
        )
        .await;
    }

    if args.resolved {
        return run_add_resolved(
            args,
            &workdir,
            &index_path,
            &storage_path,
            &layer_scope,
            pathspec_ctx,
            index,
        )
        .await;
    }

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
        args.force,
        args.update,
        args.update && args.ignore_errors,
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
            let batch_storage = util::objects_storage();
            let stored_objects = batch_storage.pending_object_index_batch_count();
            batch_storage.flush_object_index_batch().map_err(|source| {
                AddError::ObjectIndexBatchFlush {
                    stored_objects,
                    source,
                }
            })?;
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
    // Unmerged-only paths have no stage 0, so the status change calc skips
    // them. Ordinary add (not --renormalize) must still stage them.
    if !args.renormalize {
        for path in collect_unmerged_paths(&index) {
            let candidate = PathBuf::from(&path);
            if validated.pathspecs.matches_path(&candidate) && !files.contains(&candidate) {
                files.push(candidate);
            }
        }
    }
    filter_out_current_executable(&mut files);
    files.sort();
    files.dedup();

    // Layer never-enters-commit guard (lore.md 2.4): a layer-owned overlay
    // path must NEVER be staged, EVEN under --force (which bypasses ignore
    // filtering — the ignore-exclusion chokepoint alone is not airtight).
    // Under Respect, layer paths are already ignore-excluded so `files` is
    // empty of them (this loop is a no-op — zero overhead with no layers).
    {
        // W1 §C.4.1.1 scope↔workdir binding, last-moment re-verification:
        // awaits ran since `layer_scope` was derived from the entry workdir.
        // If the process cwd moved to ANOTHER worktree meanwhile, the index
        // this request stages into would no longer belong to `layer_scope` —
        // refuse rather than guard the wrong tree (fail closed).
        crate::internal::layer::verify_staging_context(&workdir, &layer_scope)?;
        // Fail-CLOSED (§C.4.1.1): a read failure here must NOT allow staging —
        // the invariant is that a materialized overlay never enters a commit.
        // The STRICT reader, because `materialized_paths` is absence-tolerant:
        // it answers "no overlays" for a missing `layer_path` table so a fresh
        // or pre-migration repository still works, and on a corrupt or partially
        // migrated database that same answer would let `add --force` stage an
        // overlay that is still on disk.
        let owned: std::collections::HashSet<String> =
            crate::internal::layer::LayerStore::owned_path_set_strict(&layer_scope)
                .await
                .map_err(|e| {
                    CliError::fatal(format!(
                        "cannot verify layer-owned paths before staging: {e}"
                    ))
                    .with_stable_code(StableErrorCode::IoReadFailed)
                })?
                .into_iter()
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

    let batch_storage = util::objects_storage();
    let stored_objects = batch_storage.pending_object_index_batch_count();
    batch_storage
        .flush_object_index_batch()
        .map_err(|source| AddError::ObjectIndexBatchFlush {
            stored_objects,
            source,
        })?;
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

/// The `"(+|-)x"` spelling a `--chmod` target mode requests: `+x` sets the
/// executable bit (`100755`), `-x` clears it (`100644`).
fn chmod_flip(target_mode: u32) -> &'static str {
    if target_mode & 0o111 != 0 { "+x" } else { "-x" }
}

/// Force the executable bit on every matched, tracked **regular** file.
/// Symlinks and gitlinks carry no executable bit and are refused (recorded in
/// [`AddOutput::chmod_rejected`]). A path whose mode already matches is left
/// untouched; a real change is reported as `modified`. In `dry_run` the index
/// is not mutated, only the report.
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
        // Read the current mode + blob id + size without holding the index
        // borrow (`IndexEntry` is not `Clone`).
        let Some((current_mode, hash, entry_size)) =
            index.get(file_str, 0).map(|e| (e.mode, e.hash, e.size))
        else {
            continue;
        };
        // Non-regular index entries (symlinks `120000`, gitlinks `160000`)
        // carry no executable bit: record the refusal, leave the entry
        // unchanged, and keep processing the remaining paths (ADR-CH-01).
        if current_mode & 0o170000 != 0o100000 {
            out.chmod_rejected.push(ChmodRejection {
                path: file_str.to_string(),
                flip: chmod_flip(target_mode).to_string(),
            });
            continue;
        }
        // A regular blob already at the target mode needs no change.
        if current_mode == target_mode {
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
            // Keep the existing blob and force the requested mode, but do
            // NOT record the CURRENT worktree stat: `new_from_file` here
            // paired a possibly-modified file's fresh stat with the stale
            // staged hash, making the modification invisible to status's
            // stat shortcut over an unbounded window (2026-08-06 R0-8
            // review). Zeroed stat fields (the `new_from_blob` shape)
            // force the next status to content-compare this entry.
            let mut updated = IndexEntry::new_from_blob(file_str.to_string(), hash, entry_size);
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
    // Stat BEFORE reading — the entry's stat must describe the hashed
    // content, or it is smudged (2026-08-06 R0-8 review).
    let pre_read = file_abs.symlink_metadata().ok();
    let blob = gen_blob_from_file(&file_abs).map_err(|source| AddError::CreateIndexEntry {
        path: file.to_path_buf(),
        source,
    })?;
    blob.try_save().map_err(|source| AddError::ObjectSave {
        path: file.to_path_buf(),
        source,
    })?;
    index.update(
        crate::command::verified_index_entry(file, blob.id, workdir, pre_read.as_ref()).map_err(
            |source| AddError::CreateIndexEntry {
                path: file.to_path_buf(),
                source,
            },
        )?,
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

    let emit_body = dry_run || verbose || !result.refreshed.is_empty() || stdout_is_tty_for_add();
    if emit_body {
        if dry_run {
            render_dry_run(&mut w, result)?;
        } else if !result.refreshed.is_empty() {
            render_refresh(&mut w, result, verbose)?;
        } else {
            render_normal(&mut w, result, verbose)?;
        }
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
/// - Under `ignore_missing` (and not `force`), a spec that matches nothing is
///   classified against the configured ignore rules: ignored → `ignored`
///   (reported, non-zero via the caller), otherwise → `missing` (skip warning).
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
    force: bool,
    update_known_only: bool,
    ignore_unknown_pathspecs: bool,
) -> Result<ValidatedPathspecs, AddError> {
    let pathspecs = PathspecSet::from_workdir_with_default_icase(
        raw_pathspecs,
        pathspec_ctx.current_dir,
        pathspec_ctx.workdir,
        pathspec_ctx.ignore_case,
    )
    .map_err(|source| AddError::Pathspec { source })?;

    let index_known = index_paths_any_stage(index);
    let change_candidates = collect_change_candidates(visible_changes);
    let ignored_candidates = collect_change_candidates(ignored_changes);
    let selectable_candidates = if update_known_only {
        index_known
    } else {
        pathspec_candidates(&change_candidates, &index_known)
    };
    let all_candidates = pathspec_candidates(&selectable_candidates, &ignored_candidates);
    let untracked_candidates = visible_changes.new.clone();

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
                // ADR-IA-03: a spec that matched nothing is classified against
                // the configured ignore rules (unless `--force`, which skips the
                // ignore check, mirroring Git). Ignored → reported and, via the
                // caller's exit-1 decision, non-zero; otherwise it stays a
                // skipped-warning `missing` spec.
                if !force
                    && pathspecs.positive_spec_match_path(raw).is_some_and(
                        |(match_path, _icase)| {
                            crate::utils::ignore::should_ignore(
                                Path::new(match_path),
                                crate::utils::ignore::IgnorePolicy::Respect,
                                index,
                            )
                        },
                    )
                {
                    ignored.push(raw.to_string());
                    continue;
                }
                missing.push(raw.to_string());
                continue;
            }
            if ignore_unknown_pathspecs {
                continue;
            }
            if update_known_only
                && !pathspec_looks_like_glob(raw)
                && pathspecs
                    .unmatched_positive_specs(&untracked_candidates)
                    .iter()
                    .all(|spec| *spec != raw)
            {
                return Err(AddError::PathspecNotKnownToIndex {
                    pathspec: raw.to_string(),
                });
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
            // Stat BEFORE reading: the entry's stat must describe the
            // hashed content, or it is smudged (2026-08-06 R0-8 review).
            let pre_read = file_abs.symlink_metadata().ok();
            let blob =
                gen_blob_from_file(&file_abs).map_err(|source| AddError::CreateIndexEntry {
                    path: file.to_path_buf(),
                    source,
                })?;
            blob.try_save().map_err(|source| AddError::ObjectSave {
                path: file.to_path_buf(),
                source,
            })?;
            index.add(
                crate::command::verified_index_entry(file, blob.id, workdir, pre_read.as_ref())
                    .map_err(|source| AddError::CreateIndexEntry {
                        path: file.to_path_buf(),
                        source,
                    })?,
            );
            clear_conflict_stages(index, file_str);
            Ok(StagedAction::Added)
        }
        FileStatus::Modified => {
            let unmerged = path_has_conflict_stages(index, file_str);
            let missing_stage0 = !index.tracked(file_str, 0);
            let content_dirty = !missing_stage0 && index.is_modified(file_str, 0, workdir);
            if unmerged || missing_stage0 || content_dirty {
                let pre_read = file_abs.symlink_metadata().ok();
                let blob =
                    gen_blob_from_file(&file_abs).map_err(|source| AddError::CreateIndexEntry {
                        path: file.to_path_buf(),
                        source,
                    })?;
                if missing_stage0 || !index.verify_hash(file_str, 0, &blob.id) {
                    blob.try_save().map_err(|source| AddError::ObjectSave {
                        path: file.to_path_buf(),
                        source,
                    })?;
                    index.update(
                        crate::command::verified_index_entry(
                            file,
                            blob.id,
                            workdir,
                            pre_read.as_ref(),
                        )
                        .map_err(|source| AddError::CreateIndexEntry {
                            path: file.to_path_buf(),
                            source,
                        })?,
                    );
                }
                clear_conflict_stages(index, file_str);
                return Ok(StagedAction::Modified);
            }
            Ok(StagedAction::Unchanged)
        }
        FileStatus::Deleted => {
            remove_all_stages(index, file_str);
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
    let unmerged = path_has_conflict_stages(index, file_str);
    if file_abs.symlink_metadata().is_err() {
        if index.tracked(file_str, 0) || unmerged {
            Ok(FileStatus::Deleted)
        } else {
            Ok(FileStatus::NotFound)
        }
    } else if !index.tracked(file_str, 0) {
        if unmerged {
            Ok(FileStatus::Modified)
        } else {
            Ok(FileStatus::New)
        }
    } else if unmerged || index.is_modified(file_str, 0, workdir) {
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
            AddError::PathspecNotKnownToIndex {
                pathspec: "baz".to_string(),
            }
            .to_string(),
            "pathspec 'baz' did not match any file(s) known to the index",
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

    #[test]
    #[serial_test::serial(env)]
    fn test_pathspec_looks_like_glob() {
        assert!(pathspec_looks_like_glob("b*"));
        assert!(pathspec_looks_like_glob("file?.txt"));
        assert!(pathspec_looks_like_glob("file[ab].txt"));
        assert!(!pathspec_looks_like_glob("baz"));
        assert!(!pathspec_looks_like_glob("top"));
    }

    #[test]
    #[serial_test::serial(env)]
    fn test_stdout_is_tty_for_add_respects_test_gate() {
        // The unit test process sets LIBRA_TEST in some suites and not in
        // others; the helper must not panic either way.
        let _ = stdout_is_tty_for_add();
    }

    #[test]
    fn test_args_accepts_resolved() {
        let args = AddArgs::try_parse_from(["test", "--resolved"]).expect("parse --resolved");
        assert!(args.resolved);
        assert!(args.pathspec.is_empty());
        // Combinations with -u/-A must parse so run_add can emit Git's wording.
        let with_u = AddArgs::try_parse_from(["test", "--resolved", "-u"]).expect("parse");
        assert!(with_u.resolved && with_u.update);
        let with_a = AddArgs::try_parse_from(["test", "--resolved", "-A"]).expect("parse");
        assert!(with_a.resolved && with_a.all);
    }

    #[test]
    fn test_args_accepts_hidden_patch() {
        let short = AddArgs::try_parse_from(["test", "-p"]).expect("parse -p");
        assert!(short.patch);
        let long = AddArgs::try_parse_from(["test", "--patch", "tracked.txt"]).expect("parse");
        assert!(long.patch);
        assert_eq!(long.pathspec, vec!["tracked.txt".to_string()]);
        assert!(!long.resolved);
    }

    #[test]
    fn test_conflict_marker_line_matches_git() {
        assert!(is_conflict_marker_line(b"<<<<<<< HEAD\n"));
        assert!(is_conflict_marker_line(b"=======\n"));
        assert!(is_conflict_marker_line(b">>>>>>> theirs\n"));
        assert!(is_conflict_marker_line(
            b"||||||| merged common ancestors\n"
        ));
        assert!(!is_conflict_marker_line(b"<<<<<< x\n"));
        assert!(!is_conflict_marker_line(b"<<<<<<<HEAD\n"));
        assert!(!is_conflict_marker_line(b"not a marker\n"));
        assert!(!is_conflict_marker_line(b"=======")); // no trailing whitespace
    }

    #[test]
    fn test_conflict_markers_stop_on_binary() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("text.txt");
        std::fs::write(
            &text,
            "<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> other\n",
        )
        .unwrap();
        assert!(file_has_conflict_markers(&text));

        let marker_then_nul = dir.path().join("bin");
        std::fs::write(&marker_then_nul, b"<<<<<<< HEAD\n\0ours").unwrap();
        assert!(file_has_conflict_markers(&marker_then_nul));

        let binary_first = dir.path().join("bin_first");
        std::fs::write(&binary_first, b"\0<<<<<<< HEAD\n").unwrap();
        assert!(!file_has_conflict_markers(&binary_first));
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

    /// CH-01 (plan-20260918): `apply_chmod` refuses non-regular index entries
    /// (symlink `120000`, gitlink `160000`) into `chmod_rejected` with the
    /// requested flip, while a regular `100644` entry is still updated in the
    /// report; a dry run leaves the index untouched.
    #[tokio::test]
    #[serial_test::serial(cwd)]
    async fn apply_chmod_refuses_nonregular_entries() {
        use crate::utils::test::{ChangeDirGuard, setup_with_new_libra_in};

        let repo = tempfile::tempdir().unwrap();
        setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());
        std::fs::write(repo.path().join("reg"), "reg\n").unwrap();

        let mut index = Index::new();
        for (name, mode) in [("link", 0o120000u32), ("gl", 0o160000), ("reg", 0o100644)] {
            let mut entry = IndexEntry::new_from_blob(name.to_string(), ObjectHash::default(), 0);
            entry.mode = mode;
            index.add(entry);
        }
        let specs = PathspecSet::from_workdir(
            &["link".to_string(), "gl".to_string(), "reg".to_string()],
            repo.path(),
            repo.path(),
        )
        .expect("pathspec compiles");
        let mut out = AddOutput::empty(false);
        apply_chmod(&mut index, 0o100755, &specs, true, &mut out).expect("apply_chmod");

        let rejected: Vec<&str> = out.chmod_rejected.iter().map(|r| r.path.as_str()).collect();
        assert!(
            rejected.contains(&"link"),
            "symlink must be refused: {rejected:?}"
        );
        assert!(
            rejected.contains(&"gl"),
            "gitlink must be refused: {rejected:?}"
        );
        assert!(
            !rejected.contains(&"reg"),
            "regular entry must not be refused: {rejected:?}"
        );
        assert!(
            out.chmod_rejected.iter().all(|r| r.flip == "+x"),
            "flip must reflect the request: {:?}",
            out.chmod_rejected
        );
        assert!(
            out.modified.contains(&"reg".to_string()),
            "regular entry is updated in the report: {:?}",
            out.modified
        );
        // Dry run: the refused entries keep their original modes in the index.
        assert_eq!(index.get("link", 0).unwrap().mode, 0o120000);
        assert_eq!(index.get("gl", 0).unwrap().mode, 0o160000);
        assert_eq!(index.get("reg", 0).unwrap().mode, 0o100644);
    }

    /// PSF-01 (plan-20260918): `parse_pathspec_file` implements the delimiter
    /// contract — LF mode strips one trailing CR, NUL mode keeps every byte,
    /// blanks are dropped, and non-UTF-8 is a hard error.
    #[test]
    fn parse_pathspec_file_splits_and_strips_cr() {
        let lf = parse_pathspec_file(b"a.txt\r\nb.txt\r\n", false).unwrap();
        assert_eq!(lf, vec!["a.txt".to_string(), "b.txt".to_string()]);

        // A CR that is not a line terminator is kept.
        let inner_cr = parse_pathspec_file(b"a\rb\n", false).unwrap();
        assert_eq!(inner_cr, vec!["a\rb".to_string()]);

        // Blank lines are dropped.
        let blanks = parse_pathspec_file(b"\n\na.txt\n\n", false).unwrap();
        assert_eq!(blanks, vec!["a.txt".to_string()]);

        // NUL mode splits on 0 and keeps CR bytes verbatim.
        let nul = parse_pathspec_file(b"a.txt\r\n\0b.txt", true).unwrap();
        assert_eq!(nul, vec!["a.txt\r\n".to_string(), "b.txt".to_string()]);

        // Non-UTF-8 is a hard failure, never a silent skip.
        assert!(
            parse_pathspec_file(b"\xff\xfe", false).is_err(),
            "non-UTF-8 must fail"
        );

        // An empty payload yields no pathspecs (falls through to the gate).
        assert!(parse_pathspec_file(b"", false).unwrap().is_empty());

        // PSF-02: newline mode decodes one C-style quoted line.
        let quoted = parse_pathspec_file(b"\"qu\\\"ote.txt\"\n", false).unwrap();
        assert_eq!(quoted, vec!["qu\"ote.txt".to_string()]);
        let spaced = parse_pathspec_file(b"\"we ird.txt\"\n", false).unwrap();
        assert_eq!(spaced, vec!["we ird.txt".to_string()]);

        // NUL mode keeps quoting verbatim (no C-quote decoding).
        let nul_quoted = parse_pathspec_file(b"\"a.txt\"\0", true).unwrap();
        assert_eq!(nul_quoted, vec!["\"a.txt\"".to_string()]);

        // Unterminated quoting is a hard failure.
        assert!(parse_pathspec_file(b"\"we ird.txt\n", false).is_err());

        // PSF-01 review P1-1: a CR is stripped only when it precedes the
        // terminating LF; an unterminated final segment keeps it.
        assert_eq!(
            parse_pathspec_file(b"a.txt\r", false).unwrap(),
            vec!["a.txt\r".to_string()]
        );
        assert_eq!(
            parse_pathspec_file(b"a.txt\n\r", false).unwrap(),
            vec!["a.txt".to_string(), "\r".to_string()]
        );

        // PSF-02 review P1-1: an empty C-quoted string is dropped, never the
        // whole-tree pathspec (fail closed).
        assert!(parse_pathspec_file(b"\"\"\n", false).unwrap().is_empty());
    }

    /// IA-02 (ADR-IA-03): `validate_pathspecs` classifies an unmatched spec
    /// against the ignore rules when `ignore_missing` is set, and `force`
    /// skips that classification — unit-tested directly over the
    /// `(ignore_missing, force)` combinations.
    #[tokio::test]
    #[serial_test::serial(cwd)]
    async fn validate_pathspecs_classifies_ignored_missing() {
        use crate::utils::test::{ChangeDirGuard, setup_with_new_libra_in};

        let repo = tempfile::tempdir().unwrap();
        setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());
        std::fs::write(repo.path().join(".libraignore"), "*.log\n").unwrap();

        let index = Index::new();
        let changes = Changes::default();
        let ctx = || PathspecMatchContext {
            workdir: repo.path(),
            current_dir: repo.path(),
            ignore_case: false,
        };
        let classify = |spec: &str, ignore_missing: bool, force: bool| {
            validate_pathspecs(
                &[spec.to_string()],
                ctx(),
                &changes,
                &changes,
                &index,
                ignore_missing,
                force,
                false,
                false,
            )
            .expect("validate_pathspecs")
        };

        // An ignored non-existent path lands in `ignored`.
        let validated = classify("x.log", true, false);
        assert_eq!(validated.ignored, vec!["x.log".to_string()]);
        assert!(validated.missing.is_empty());

        // An un-ignored non-existent path stays a `missing` skip.
        let validated = classify("note.txt", true, false);
        assert!(validated.ignored.is_empty());
        assert_eq!(validated.missing, vec!["note.txt".to_string()]);

        // `force` skips the ignore classification (Git parity).
        let validated = classify("x.log", true, true);
        assert!(validated.ignored.is_empty());
        assert_eq!(validated.missing, vec!["x.log".to_string()]);

        // Without `ignore_missing` an unmatched spec is a hard error.
        let err = validate_pathspecs(
            &["nope.txt".to_string()],
            ctx(),
            &changes,
            &changes,
            &index,
            false,
            false,
            false,
            false,
        )
        .expect_err("unmatched spec without ignore_missing must fail");
        assert!(
            matches!(err, AddError::PathspecNotMatched { .. }),
            "{err:?}"
        );
    }
}
