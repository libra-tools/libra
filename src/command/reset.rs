//! Reset command covering soft/mixed/hard/merge/keep behaviors to move HEAD and align the index or working tree to a chosen commit.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs, io,
    io::{BufRead, Read},
    path::{Component, Path, PathBuf},
};

use clap::Parser;
use git_internal::{
    hash::ObjectHash,
    internal::{
        index::{Index, IndexEntry},
        object::{
            commit::Commit,
            tree::{Tree, TreeItemMode},
        },
    },
};
use serde::Serialize;

use crate::{
    command::{load_object, symlink_target_blob_bytes},
    common_utils::parse_commit_msg,
    internal::{
        branch::{self, Branch},
        db::get_db_conn_instance,
        head::Head,
        reflog::{ReflogAction, ReflogContext, with_reflog},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode, emit_warning},
        object_ext::{BlobExt, TreeExt},
        output::{OutputConfig, emit_json_data},
        path,
        text::short_display_hash,
        util, worktree,
    },
};

const RESET_EXAMPLES: &str = "\
EXAMPLES:
    libra reset HEAD~1                    Move HEAD and reset index to the previous commit
    libra reset --soft HEAD~2             Move HEAD only, keep index and worktree
    libra reset --hard main               Reset HEAD, index, and worktree to branch 'main'
    libra reset --merge HEAD~1            Preserve safe unstaged worktree changes
    libra reset --keep HEAD~1             Refuse if affected paths have local changes
    libra reset src/lib.rs                 Unstage a path back to HEAD
    libra reset HEAD -- src/lib.rs        Unstage a path back to HEAD
    libra reset --pathspec-from-file=paths.txt   Unstage paths read from a file ('-' for stdin)
    libra reset --json --hard HEAD~1      Structured JSON output for agents";

pub(crate) const RESET_PATHSPEC_SEPARATOR_FLAG: &str = "__libra-reset-pathspec-separator";
pub(crate) const DEFAULT_RESET_TARGET: &str = "HEAD";

#[derive(Parser, Debug)]
#[command(after_help = RESET_EXAMPLES)]
pub struct ResetArgs {
    /// The commit to reset to (default: HEAD)
    pub target: Option<String>,

    /// Soft reset: only move HEAD pointer
    #[clap(long, group = "mode")]
    pub soft: bool,

    /// Mixed reset: move HEAD and reset index (default)
    #[clap(long, group = "mode")]
    pub mixed: bool,

    /// Hard reset: move HEAD, reset index and working directory
    #[clap(long, group = "mode")]
    pub hard: bool,

    /// Reset HEAD/index and update only safely replaceable working-tree paths,
    /// preserving unstaged changes.
    #[clap(long, group = "mode")]
    pub merge: bool,

    /// Reset HEAD/index while preserving local changes on paths changed between
    /// HEAD and the target.
    #[clap(long, group = "mode")]
    pub keep: bool,

    /// Pathspecs to reset specific files
    #[clap(value_name = "PATH")]
    pub pathspecs: Vec<String>,

    /// Internal flag injected by the top-level CLI when the user typed `--`
    /// inside `reset` arguments. Clap does not preserve the separator itself,
    /// but reset needs to distinguish `reset HEAD` from `reset -- HEAD`.
    #[clap(long = RESET_PATHSPEC_SEPARATOR_FLAG, hide = true)]
    pub pathspec_separator: bool,

    /// Read pathspecs from the given file (`-` for stdin), one per line (or
    /// NUL-separated with --pathspec-file-nul). Mutually exclusive with
    /// command-line pathspecs.
    #[clap(long, value_name = "FILE")]
    pub pathspec_from_file: Option<String>,

    /// Treat --pathspec-from-file input as NUL-separated instead of
    /// line-separated. No-op without --pathspec-from-file.
    #[clap(long)]
    pub pathspec_file_nul: bool,

    /// Accepted for Git compatibility. Libra's reset never refreshes the index,
    /// so this flag is a no-op.
    #[clap(long)]
    pub no_refresh: bool,
}

#[derive(Debug, Clone, Copy)]
enum ResetMode {
    Soft,
    Mixed,
    Hard,
    Merge,
    Keep,
}

impl ResetMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Soft => "soft",
            Self::Mixed => "mixed",
            Self::Hard => "hard",
            Self::Merge => "merge",
            Self::Keep => "keep",
        }
    }
}

#[derive(Debug, Default, Clone)]
struct ResetStats {
    files_restored: usize,
    warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct ResetExecution {
    output: ResetOutput,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResetOutput {
    pub mode: String,
    pub commit: String,
    pub short_commit: String,
    pub subject: String,
    pub previous_commit: Option<String>,
    pub files_unstaged: usize,
    pub files_restored: usize,
    pub pathspecs: Vec<String>,
}

/// Execute the reset command with the given arguments.
/// Resets the current HEAD to the specified state, with different modes:
/// - Soft: Only moves HEAD pointer
/// - Mixed: Moves HEAD and resets index (default)
/// - Hard: Moves HEAD, resets index and working directory
pub async fn execute(args: ResetArgs) {
    if let Err(e) = execute_safe(args, &OutputConfig::default()).await {
        e.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting.
///
/// # Side Effects
/// - Moves HEAD/current branch to the resolved target commit.
/// - In mixed mode, rewrites the index from the target tree or pathspecs.
/// - In hard mode, rewrites both the index and working tree.
/// - Emits warnings for recoverable filesystem cleanup issues.
///
/// # Errors
/// Returns [`CliError`] when the repository is missing, the revision or
/// pathspecs cannot be resolved, object reads fail, or HEAD/index/worktree
/// updates fail.
pub async fn execute_safe(args: ResetArgs, output: &OutputConfig) -> CliResult<()> {
    let result = run_reset(args).await.map_err(CliError::from)?;
    render_reset_output(&result.output, output)?;
    for warning in result.warnings {
        emit_warning(warning);
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum ResetError {
    #[error("not a libra repository")]
    NotInRepo,

    #[error("{0}")]
    InvalidRevision(String),

    #[error("ambiguous argument '{0}': both revision and filename")]
    AmbiguousRevisionPath(String),

    #[error("Cannot reset: HEAD is unborn and points to no commit.")]
    HeadUnborn,

    #[error("failed to resolve HEAD commit: {0}")]
    HeadRead(String),

    #[error("stored HEAD reference is corrupt: {0}")]
    HeadCorrupt(String),

    #[error("failed to load {kind} '{object_id}': {detail}")]
    ObjectLoad {
        kind: &'static str,
        object_id: String,
        detail: String,
    },

    #[error("failed to load index: {0}")]
    IndexLoad(String),

    #[error("failed to save index: {0}")]
    IndexSave(String),

    #[error("failed to update HEAD: {0}")]
    HeadUpdate(String),

    #[error("failed to read working tree: {0}")]
    WorktreeRead(String),

    #[error("failed to restore working tree: {0}")]
    WorktreeRestore(String),

    #[error("{0}")]
    RevisionRead(String),

    #[error("{0}")]
    RevisionCorrupt(String),

    #[error("path contains invalid UTF-8: {0}")]
    InvalidPathspecEncoding(String),

    #[error("pathspec '{0}' is not compatible with --soft reset")]
    PathspecWithSoft(String),

    #[error("Cannot do hard reset with paths.")]
    PathspecWithHard,

    #[error("Cannot do {0} reset with paths.")]
    PathspecWithPreservingMode(&'static str),

    #[error("local changes to '{path}' would be overwritten by reset --{mode}")]
    LocalChangesWouldBeOverwritten { mode: &'static str, path: String },

    #[error("pathspec '{0}' did not match any file(s) known to libra")]
    PathspecNotMatched(String),

    #[error("--pathspec-from-file cannot be combined with command-line pathspecs")]
    PathspecSourceConflict,

    #[error("pathspec '{0}' is outside the repository working directory")]
    PathspecOutsideWorkdir(String),

    #[error("failed to read pathspecs from {path}: {source}")]
    PathspecFileRead {
        path: String,
        #[source]
        source: io::Error,
    },

    /// Refused to reset onto a Libra-managed locked branch (`intent`,
    /// `traces`, …). These refs hold AI-agent state that the user
    /// should not be able to overwrite by `reset`.
    #[error("refusing to reset to locked branch '{0}'")]
    LockedTarget(String),

    /// Refused to move HEAD/index/worktree while HEAD is attached to a
    /// Libra-managed AI branch.
    #[error("refusing to reset locked current branch '{0}'")]
    LockedCurrentBranch(String),

    #[error("{primary}; rollback failed: {rollback}")]
    Rollback {
        primary: Box<ResetError>,
        rollback: Box<ResetError>,
    },
}

impl ResetError {
    fn stable_code(&self) -> StableErrorCode {
        match self {
            Self::NotInRepo => StableErrorCode::RepoNotFound,
            Self::InvalidRevision(_) => StableErrorCode::CliInvalidTarget,
            Self::AmbiguousRevisionPath(_) => StableErrorCode::CliInvalidArguments,
            Self::HeadUnborn => StableErrorCode::RepoStateInvalid,
            Self::HeadRead(_) => StableErrorCode::IoReadFailed,
            Self::HeadCorrupt(_) => StableErrorCode::RepoCorrupt,
            Self::ObjectLoad { .. } => StableErrorCode::RepoCorrupt,
            Self::IndexLoad(_) => StableErrorCode::RepoCorrupt,
            Self::IndexSave(_) => StableErrorCode::IoWriteFailed,
            Self::HeadUpdate(_) => StableErrorCode::IoWriteFailed,
            Self::WorktreeRead(_) => StableErrorCode::IoReadFailed,
            Self::WorktreeRestore(_) => StableErrorCode::IoWriteFailed,
            Self::RevisionRead(_) => StableErrorCode::IoReadFailed,
            Self::RevisionCorrupt(_) => StableErrorCode::RepoCorrupt,
            Self::InvalidPathspecEncoding(_) => StableErrorCode::CliInvalidArguments,
            Self::PathspecWithSoft(_) => StableErrorCode::CliInvalidArguments,
            Self::PathspecWithHard => StableErrorCode::CliInvalidArguments,
            Self::PathspecWithPreservingMode(_) => StableErrorCode::CliInvalidArguments,
            Self::LocalChangesWouldBeOverwritten { .. } => {
                StableErrorCode::ConflictOperationBlocked
            }
            Self::PathspecNotMatched(_) => StableErrorCode::CliInvalidTarget,
            Self::PathspecSourceConflict => StableErrorCode::CliInvalidArguments,
            Self::PathspecOutsideWorkdir(_) => StableErrorCode::CliInvalidArguments,
            Self::PathspecFileRead { .. } => StableErrorCode::IoReadFailed,
            Self::LockedTarget(_) => StableErrorCode::CliInvalidTarget,
            Self::LockedCurrentBranch(_) => StableErrorCode::ConflictOperationBlocked,
            Self::Rollback { primary, .. } => primary.stable_code(),
        }
    }

    fn hint(&self) -> Option<&'static str> {
        match self {
            Self::NotInRepo => {
                Some("run 'libra init' to create a repository in the current directory.")
            }
            Self::InvalidRevision(_) => Some("check the revision name and try again."),
            Self::AmbiguousRevisionPath(_) => Some(
                "use '--' to separate paths from revisions, like 'libra reset <revision> -- <file>' or 'libra reset -- <file>'.",
            ),
            Self::HeadUnborn => Some("create a commit first before resetting HEAD."),
            Self::HeadRead(_) => Some("check whether the repository database is readable."),
            Self::HeadCorrupt(_) => Some("the HEAD reference or branch metadata may be corrupted."),
            Self::ObjectLoad { .. } => Some("the object store may be corrupted."),
            Self::IndexLoad(_) => Some("the index file may be corrupted."),
            Self::InvalidPathspecEncoding(_) => {
                Some("rename the path or invoke libra from a path representable as UTF-8.")
            }
            Self::PathspecWithSoft(_) => {
                Some("--soft only moves HEAD; use --mixed to reset index for specific paths.")
            }
            Self::PathspecWithHard => Some(
                "--hard updates the working tree; omit pathspecs or use --mixed for specific paths.",
            ),
            Self::PathspecWithPreservingMode(_) => Some(
                "--merge/--keep operate on the whole tree; omit pathspecs or use --mixed for specific paths.",
            ),
            Self::LocalChangesWouldBeOverwritten { .. } => {
                Some("commit or stash the local changes, then retry the reset.")
            }
            Self::PathspecNotMatched(_) => Some("check the path and try again."),
            Self::PathspecSourceConflict => Some(
                "provide pathspecs either on the command line or via --pathspec-from-file, not both.",
            ),
            Self::PathspecOutsideWorkdir(_) => {
                Some("pathspecs must stay within the repository working directory.")
            }
            Self::PathspecFileRead { .. } => {
                Some("check that the pathspec file exists and is readable.")
            }
            Self::LockedTarget(_) => Some(
                "Libra-managed branches like 'intent' and 'traces' cannot be used as reset targets",
            ),
            Self::LockedCurrentBranch(_) => Some("switch to a user branch before running reset"),
            Self::RevisionRead(_) => {
                Some("check whether the repository references and object storage are readable.")
            }
            Self::RevisionCorrupt(_) => {
                Some("the referenced branch, tag, or object metadata may be corrupted.")
            }
            Self::IndexSave(_)
            | Self::HeadUpdate(_)
            | Self::WorktreeRead(_)
            | Self::WorktreeRestore(_) => None,
            Self::Rollback { primary, .. } => primary.hint(),
        }
    }

    fn is_command_usage(&self) -> bool {
        match self {
            Self::AmbiguousRevisionPath(_)
            | Self::PathspecWithSoft(_)
            | Self::PathspecWithHard
            | Self::PathspecWithPreservingMode(_)
            | Self::PathspecSourceConflict
            | Self::PathspecOutsideWorkdir(_) => true,
            Self::Rollback { primary, .. } => primary.is_command_usage(),
            _ => false,
        }
    }
}

impl From<ResetError> for CliError {
    fn from(error: ResetError) -> Self {
        match error {
            ResetError::NotInRepo => CliError::repo_not_found(),
            other => {
                let message = other.to_string();
                let stable_code = other.stable_code();
                let mut cli = if other.is_command_usage() {
                    CliError::command_usage(message)
                } else {
                    CliError::fatal(message)
                }
                .with_stable_code(stable_code);

                if let Some(hint) = other.hint() {
                    cli = cli.with_hint(hint);
                }

                cli
            }
        }
    }
}

fn object_load_error(
    kind: &'static str,
    object_id: impl Into<String>,
    detail: impl Into<String>,
) -> ResetError {
    ResetError::ObjectLoad {
        kind,
        object_id: object_id.into(),
        detail: detail.into(),
    }
}

fn map_reset_head_commit_error(error: branch::BranchStoreError) -> ResetError {
    match error {
        branch::BranchStoreError::Query(detail) => ResetError::HeadRead(detail),
        other => ResetError::HeadCorrupt(other.to_string()),
    }
}

async fn reject_reset_on_ai_managed_current_branch() -> Result<(), ResetError> {
    match Head::current_result()
        .await
        .map_err(map_reset_head_commit_error)?
    {
        Head::Branch(name) if branch::is_ai_managed_branch(&name) => {
            Err(ResetError::LockedCurrentBranch(name))
        }
        _ => Ok(()),
    }
}

struct ResetRequest {
    target: String,
    pathspecs: Vec<String>,
}

async fn run_reset(args: ResetArgs) -> Result<ResetExecution, ResetError> {
    util::require_repo().map_err(|_| ResetError::NotInRepo)?;
    let request = normalize_reset_request(&args).await?;

    // Refuse to reset onto a Libra-managed locked branch. `is_locked_revision`
    // strips `~` / `^` / `@` suffixes so attempts like `traces~1` or
    // `intent^` are still rejected.
    if branch::is_locked_revision(&request.target) {
        return Err(ResetError::LockedTarget(request.target.clone()));
    }

    let mode = if args.soft {
        ResetMode::Soft
    } else if args.hard {
        ResetMode::Hard
    } else if args.merge {
        ResetMode::Merge
    } else if args.keep {
        ResetMode::Keep
    } else {
        ResetMode::Mixed
    };
    let previous_commit = Head::current_commit().await.map(|hash| hash.to_string());

    if !request.pathspecs.is_empty() {
        if matches!(mode, ResetMode::Soft) {
            return Err(ResetError::PathspecWithSoft(request.pathspecs.join(" ")));
        }
        if matches!(mode, ResetMode::Hard) {
            return Err(ResetError::PathspecWithHard);
        }
        if matches!(mode, ResetMode::Merge | ResetMode::Keep) {
            return Err(ResetError::PathspecWithPreservingMode(mode.as_str()));
        }

        let target_commit_id = resolve_commit(&request.target).await?;
        let changed_paths = reset_pathspecs(&request.pathspecs, &target_commit_id).await?;
        let subject = load_commit_summary_or_warn(&target_commit_id);
        let commit = target_commit_id.to_string();

        // Pathspec resets do not move HEAD, so the user-contract JSON schema
        // (docs/commands/reset.md) promises `previous_commit: null` to signal
        // "HEAD is unchanged". Drop the captured HEAD here so machine
        // consumers can tell pathspec resets apart from full resets without
        // having to compare `commit` against `previous_commit`.
        return Ok(ResetExecution {
            output: ResetOutput {
                mode: mode.as_str().to_string(),
                short_commit: short_display_hash(&commit).to_string(),
                commit,
                subject,
                previous_commit: None,
                files_unstaged: changed_paths.len(),
                files_restored: 0,
                pathspecs: changed_paths,
            },
            warnings: Vec::new(),
        });
    }

    reject_reset_on_ai_managed_current_branch().await?;

    let target_commit_id = resolve_commit(&request.target).await?;
    let reset_stats = perform_reset(target_commit_id, mode, &request.target).await?;

    let subject = load_commit_summary_or_warn(&target_commit_id);
    let commit = target_commit_id.to_string();
    Ok(ResetExecution {
        output: ResetOutput {
            mode: mode.as_str().to_string(),
            short_commit: short_display_hash(&commit).to_string(),
            commit,
            subject,
            previous_commit,
            files_unstaged: 0,
            files_restored: reset_stats.files_restored,
            pathspecs: Vec::new(),
        },
        warnings: reset_stats.warnings,
    })
}

async fn normalize_reset_request(args: &ResetArgs) -> Result<ResetRequest, ResetError> {
    // Effective pathspecs may come from the command line or from
    // `--pathspec-from-file` (mutually exclusive; `-` reads stdin). Both
    // sources flow through the same `reset_pathspecs` execution and
    // containment checks below.
    let mut pathspecs = resolve_effective_pathspecs(args)?;
    let target = args
        .target
        .as_deref()
        .unwrap_or(DEFAULT_RESET_TARGET)
        .to_string();

    if args.pathspec_separator || args.target.is_none() || args.pathspec_from_file.is_some() {
        return Ok(ResetRequest { target, pathspecs });
    }

    let Some(target_arg) = args.target.as_deref() else {
        return Ok(ResetRequest { target, pathspecs });
    };
    let resolves_as_revision = target_resolves_as_revision(target_arg).await?;
    if resolves_as_revision {
        if pathspec_exists_in_worktree(target_arg) {
            return Err(ResetError::AmbiguousRevisionPath(target_arg.to_string()));
        }
        return Ok(ResetRequest { target, pathspecs });
    }

    if pathspec_matches_known_path(target_arg).await? {
        pathspecs.insert(0, target_arg.to_string());
        return Ok(ResetRequest {
            target: DEFAULT_RESET_TARGET.to_string(),
            pathspecs,
        });
    }

    Ok(ResetRequest { target, pathspecs })
}

async fn target_resolves_as_revision(target: &str) -> Result<bool, ResetError> {
    match resolve_commit(target).await {
        Ok(_) => Ok(true),
        Err(ResetError::InvalidRevision(_)) | Err(ResetError::HeadUnborn) => Ok(false),
        Err(error) => Err(error),
    }
}

async fn pathspec_matches_known_path(pathspec: &str) -> Result<bool, ResetError> {
    let absolute = util::workdir_to_absolute(PathBuf::from(pathspec));
    if absolute.symlink_metadata().is_ok() {
        return Ok(true);
    }
    if !util::is_sub_path(&absolute, util::working_dir()) {
        return Ok(false);
    }

    let relative_path = util::workdir_to_current(PathBuf::from(pathspec));
    let path_str = relative_path
        .to_str()
        .ok_or_else(|| ResetError::InvalidPathspecEncoding(relative_path.display().to_string()))?;

    let index = Index::load(path::index()).map_err(|e| ResetError::IndexLoad(e.to_string()))?;
    if index.get(path_str, 0).is_some() {
        return Ok(true);
    }

    let Some(head_commit_id) = Head::current_commit_result()
        .await
        .map_err(map_reset_head_commit_error)?
    else {
        return Ok(false);
    };
    let commit: Commit = load_object(&head_commit_id)
        .map_err(|e| object_load_error("commit", head_commit_id.to_string(), e.to_string()))?;
    let tree: Tree = load_object(&commit.tree_id)
        .map_err(|e| object_load_error("tree", commit.tree_id.to_string(), e.to_string()))?;
    find_tree_item(&tree, path_str).map(|item| item.is_some())
}

fn pathspec_exists_in_worktree(pathspec: &str) -> bool {
    let absolute = util::workdir_to_absolute(PathBuf::from(pathspec));
    absolute.symlink_metadata().is_ok() && util::is_sub_path(&absolute, util::working_dir())
}

/// Reset specific files in the index to their state in the target commit.
/// This function only affects the index, not the working directory.
async fn reset_pathspecs(
    pathspecs: &[String],
    target_commit_id: &ObjectHash,
) -> Result<Vec<String>, ResetError> {
    let commit: Commit = load_object(target_commit_id)
        .map_err(|e| object_load_error("commit", target_commit_id.to_string(), e.to_string()))?;

    let tree: Tree = load_object(&commit.tree_id)
        .map_err(|e| object_load_error("tree", commit.tree_id.to_string(), e.to_string()))?;

    let index_file = path::index();
    let mut index = Index::load(&index_file).map_err(|e| ResetError::IndexLoad(e.to_string()))?;
    let mut changed = false;
    let mut changed_paths = Vec::new();

    for pathspec in pathspecs {
        // Containment: a pathspec is workdir-relative, so resolve it against the
        // working directory and reject anything that escapes the repository (a
        // `../` traversal). This applies uniformly to command-line and
        // `--pathspec-from-file` sources. `is_sub_path` normalises `..`
        // components without touching the filesystem.
        let absolute = util::workdir_to_absolute(PathBuf::from(pathspec));
        if !util::is_sub_path(&absolute, util::working_dir()) {
            return Err(ResetError::PathspecOutsideWorkdir(pathspec.clone()));
        }

        let relative_path = util::workdir_to_current(PathBuf::from(pathspec));
        let path_str = relative_path.to_str().ok_or_else(|| {
            ResetError::InvalidPathspecEncoding(relative_path.display().to_string())
        })?;

        match find_tree_item(&tree, path_str)? {
            Some(item) => {
                let blob: git_internal::internal::object::blob::Blob = load_object(&item.id)
                    .map_err(|e| object_load_error("blob", item.id.to_string(), e.to_string()))?;
                let mut entry = IndexEntry::new_from_blob(
                    path_str.to_string(),
                    item.id,
                    blob.data.len() as u32,
                );
                entry.mode = tree_item_mode_to_index_mode(item.mode)?;
                index.add(entry);
                changed = true;
                changed_paths.push(pathspec.clone());
            }
            None => {
                if index.get(path_str, 0).is_some() {
                    index.remove(path_str, 0);
                    changed = true;
                    changed_paths.push(pathspec.clone());
                } else {
                    return Err(ResetError::PathspecNotMatched(pathspec.clone()));
                }
            }
        }
    }

    if changed {
        index
            .save(&index_file)
            .map_err(|e| ResetError::IndexSave(e.to_string()))?;
    }
    Ok(changed_paths)
}

/// Upper bound on `--pathspec-from-file` input (file or stdin) to guard against
/// OOM / DoS from a pathological input. Matches `libra add`'s limit so both
/// commands share one ceiling.
const MAX_PATHSPEC_FILE_BYTES: u64 = 128 * 1024 * 1024;

/// Resolve the pathspecs the reset should operate on.
///
/// Pathspecs come from exactly one source: the command line, or
/// `--pathspec-from-file` (`-` reads stdin). Supplying both is a usage error
/// ([`ResetError::PathspecSourceConflict`], exit 129) so a script cannot
/// silently merge two lists. `--pathspec-file-nul` only switches the separator
/// and is an inert no-op when `--pathspec-from-file` is absent (matching Git).
fn resolve_effective_pathspecs(args: &ResetArgs) -> Result<Vec<String>, ResetError> {
    match args.pathspec_from_file.as_deref() {
        Some(file) => {
            if !args.pathspecs.is_empty() {
                return Err(ResetError::PathspecSourceConflict);
            }
            read_pathspec_from_file(file, args.pathspec_file_nul)
        }
        None => Ok(args.pathspecs.clone()),
    }
}

/// Read pathspecs from `path` (a file, or `-` for stdin), streaming.
///
/// Items are separated by NUL when `nul` is set (`--pathspec-file-nul`),
/// otherwise by newline (a trailing `\r` is stripped so CRLF files work). Empty
/// items are dropped. Input is read incrementally via [`BufRead::read_until`]
/// and bounded at [`MAX_PATHSPEC_FILE_BYTES`] as it is consumed, so even an
/// unbounded stdin pipe cannot exhaust memory; exceeding the cap (or any read
/// failure) returns [`ResetError::PathspecFileRead`] and non-UTF-8 input
/// returns [`ResetError::InvalidPathspecEncoding`].
///
/// Each item is taken verbatim — Git's default-mode C-style quoted-path
/// decoding is intentionally not performed (use `--pathspec-file-nul` for paths
/// with special characters); the returned raw pathspecs are still normalised
/// and containment-checked by [`reset_pathspecs`].
fn read_pathspec_from_file(path: &str, nul: bool) -> Result<Vec<String>, ResetError> {
    let separator = if nul { b'\0' } else { b'\n' };
    let (label, reader): (String, Box<dyn Read>) = if path == "-" {
        ("<stdin>".to_string(), Box::new(io::stdin().lock()))
    } else {
        // Fail fast on an oversized file without opening/reading it.
        let meta = fs::metadata(path).map_err(|source| ResetError::PathspecFileRead {
            path: path.to_string(),
            source,
        })?;
        if meta.len() > MAX_PATHSPEC_FILE_BYTES {
            return Err(ResetError::PathspecFileRead {
                path: path.to_string(),
                source: io::Error::new(
                    io::ErrorKind::InvalidData,
                    "pathspec file exceeds the 128 MiB limit",
                ),
            });
        }
        let file = fs::File::open(path).map_err(|source| ResetError::PathspecFileRead {
            path: path.to_string(),
            source,
        })?;
        (path.to_string(), Box::new(file))
    };

    // `take` bounds the total read so an unbounded stdin pipe cannot exhaust
    // memory; `total` enforces the cap precisely as bytes are consumed.
    let mut reader = io::BufReader::new(reader.take(MAX_PATHSPEC_FILE_BYTES + 1));
    let mut items = Vec::new();
    let mut chunk = Vec::new();
    let mut total: u64 = 0;
    loop {
        chunk.clear();
        let read = reader.read_until(separator, &mut chunk).map_err(|source| {
            ResetError::PathspecFileRead {
                path: label.clone(),
                source,
            }
        })?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > MAX_PATHSPEC_FILE_BYTES {
            return Err(ResetError::PathspecFileRead {
                path: label.clone(),
                source: io::Error::new(
                    io::ErrorKind::InvalidData,
                    "pathspec input exceeds the 128 MiB limit",
                ),
            });
        }
        if chunk.last() == Some(&separator) {
            chunk.pop();
        }
        if !nul && chunk.last() == Some(&b'\r') {
            chunk.pop();
        }
        if chunk.is_empty() {
            continue;
        }
        let item = std::str::from_utf8(&chunk)
            .map_err(|_| ResetError::InvalidPathspecEncoding(label.clone()))?;
        items.push(item.to_string());
    }
    Ok(items)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResetIndexValue {
    hash: ObjectHash,
    mode: u32,
}

struct GuardedResetPlan {
    target_index: Index,
    worktree_updates: Vec<(PathBuf, Option<ResetIndexValue>)>,
}

#[derive(Debug)]
struct IndexSnapshot {
    existed: bool,
    bytes: Vec<u8>,
}

#[derive(Debug)]
enum WorktreeSnapshotKind {
    Missing,
    Directory,
    /// Guarded modes only replace paths whose worktree entry matches stage 0,
    /// so the existing object is an exact, bounded rollback source. Keeping its
    /// object reference avoids buffering arbitrarily large tracked files in RAM.
    Tracked(ResetIndexValue),
}

#[derive(Debug)]
struct WorktreePathSnapshot {
    path: PathBuf,
    kind: WorktreeSnapshotKind,
}

fn validate_guarded_relative_path(path: &Path) -> Result<(), ResetError> {
    let mut components = path.components();
    let first = components.next();
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || !matches!(first, Some(Component::Normal(_)))
        || components.any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ResetError::RevisionCorrupt(format!(
            "unsafe path '{}' in reset index/tree",
            path.display()
        )));
    }
    if matches!(first, Some(Component::Normal(name)) if name == ".libra") {
        return Err(ResetError::RevisionCorrupt(format!(
            "reset index/tree path '{}' targets repository metadata",
            path.display()
        )));
    }
    Ok(())
}

/// Return the first existing non-directory or symlink ancestor without ever
/// following it. A missing ancestor is safe: guarded writes may create it.
fn blocking_worktree_ancestor(relative_path: &Path) -> Result<Option<PathBuf>, ResetError> {
    validate_guarded_relative_path(relative_path)?;
    let mut cursor = util::working_dir();
    let component_count = relative_path.components().count();
    for component in relative_path
        .components()
        .take(component_count.saturating_sub(1))
    {
        cursor.push(component.as_os_str());
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Ok(Some(cursor));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(ResetError::WorktreeRead(format!(
                    "failed to inspect ancestor {}: {error}",
                    cursor.display()
                )));
            }
        }
    }
    Ok(None)
}

fn index_for_commit(commit_id: &ObjectHash) -> Result<Index, ResetError> {
    let commit: Commit = load_object(commit_id)
        .map_err(|error| object_load_error("commit", commit_id.to_string(), error.to_string()))?;
    let tree: Tree = load_object(&commit.tree_id).map_err(|error| {
        object_load_error("tree", commit.tree_id.to_string(), error.to_string())
    })?;
    let mut index = Index::new();
    rebuild_index_from_tree_typed(&tree, &mut index, "")?;
    Ok(index)
}

fn stage_zero_values(index: &Index) -> Result<HashMap<PathBuf, ResetIndexValue>, ResetError> {
    let mut values = HashMap::new();
    for path in index.tracked_files() {
        validate_guarded_relative_path(&path)?;
        let path_str = path
            .to_str()
            .ok_or_else(|| ResetError::InvalidPathspecEncoding(path.display().to_string()))?;
        let entry = index.get(path_str, 0).ok_or_else(|| {
            ResetError::IndexLoad(format!(
                "stage-0 entry disappeared for '{}'",
                path.display()
            ))
        })?;
        values.insert(
            path,
            ResetIndexValue {
                hash: entry.hash,
                mode: entry.mode,
            },
        );
    }
    Ok(values)
}

fn index_mode_to_tree_mode(mode: u32) -> Result<TreeItemMode, ResetError> {
    match mode & 0o170000 {
        0o100000 if mode & 0o111 != 0 => Ok(TreeItemMode::BlobExecutable),
        0o100000 => Ok(TreeItemMode::Blob),
        0o120000 => Ok(TreeItemMode::Link),
        0o160000 => Ok(TreeItemMode::Commit),
        _ => Err(ResetError::RevisionCorrupt(format!(
            "unsupported index mode {mode:o}"
        ))),
    }
}

fn worktree_matches_value(
    relative_path: &Path,
    value: Option<ResetIndexValue>,
) -> Result<bool, ResetError> {
    if blocking_worktree_ancestor(relative_path)?.is_some() {
        return Ok(value.is_none());
    }
    let full_path = util::working_dir().join(relative_path);
    let Some(value) = value else {
        return match fs::symlink_metadata(&full_path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
            Ok(_) => Ok(false),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(true)
            }
            Err(error) => Err(ResetError::WorktreeRead(format!(
                "failed to inspect {}: {error}",
                full_path.display()
            ))),
        };
    };
    if (value.mode & 0o170000) == 0o160000 {
        return Err(ResetError::WorktreeRead(format!(
            "submodule worktree entry '{}' is not supported by reset --merge/--keep",
            relative_path.display()
        )));
    }
    let blob: git_internal::internal::object::blob::Blob = load_object(&value.hash)
        .map_err(|error| object_load_error("blob", value.hash.to_string(), error.to_string()))?;
    worktree_entry_matches(&full_path, index_mode_to_tree_mode(value.mode)?, &blob.data)
}

fn unmerged_paths(index: &Index) -> HashSet<PathBuf> {
    (1..=3)
        .flat_map(|stage| index.tracked_entries(stage))
        .map(|entry| PathBuf::from(&entry.name))
        .collect()
}

fn carry_unmerged_entries(source: &Index, target: &mut Index) -> Result<(), ResetError> {
    let paths = unmerged_paths(source);
    for path in &paths {
        let path_str = path
            .to_str()
            .ok_or_else(|| ResetError::InvalidPathspecEncoding(path.display().to_string()))?;
        target.remove(path_str, 0);
    }
    for stage in 1..=3 {
        for entry in source.tracked_entries(stage) {
            let mut carried = IndexEntry::new_from_blob(entry.name.clone(), entry.hash, entry.size);
            carried.mode = entry.mode;
            carried.flags.stage = stage;
            target.add(carried);
        }
    }
    Ok(())
}

fn build_guarded_reset_plan(
    mode: ResetMode,
    current_index: &Index,
    head_index: &Index,
    mut target_index: Index,
) -> Result<GuardedResetPlan, ResetError> {
    let current = stage_zero_values(current_index)?;
    let head = stage_zero_values(head_index)?;
    let target = stage_zero_values(&target_index)?;
    let unmerged = unmerged_paths(current_index);
    for path in &unmerged {
        validate_guarded_relative_path(path)?;
    }
    let mut paths = BTreeSet::new();
    paths.extend(current.keys().cloned());
    paths.extend(head.keys().cloned());
    paths.extend(target.keys().cloned());

    let untracked =
        worktree::untracked_workdir_paths(current_index).map_err(ResetError::WorktreeRead)?;
    let mut worktree_updates = Vec::new();
    for path in paths {
        if matches!(mode, ResetMode::Merge) && unmerged.contains(&path) {
            continue;
        }
        if let Some(blocker) = blocking_worktree_ancestor(&path)? {
            let blocker_path = blocker.strip_prefix(util::working_dir()).map_err(|error| {
                ResetError::WorktreeRead(format!(
                    "failed to normalize blocking ancestor {}: {error}",
                    blocker.display()
                ))
            })?;
            let blocker_current = current.get(blocker_path).copied();
            let tracked_blocker_is_safely_removed = blocker_current.is_some()
                && !target.contains_key(blocker_path)
                && worktree_matches_value(blocker_path, blocker_current)?;
            if !tracked_blocker_is_safely_removed {
                return Err(ResetError::LocalChangesWouldBeOverwritten {
                    mode: mode.as_str(),
                    path: blocker_path.display().to_string(),
                });
            }
        }
        let current_value = current.get(&path).copied();
        let head_value = head.get(&path).copied();
        let target_value = target.get(&path).copied();
        let worktree_matches_index = worktree_matches_value(&path, current_value)?;
        let target_changes_head = target_value != head_value;
        let should_update = match mode {
            ResetMode::Merge => {
                if target_value != current_value && !worktree_matches_index {
                    return Err(ResetError::LocalChangesWouldBeOverwritten {
                        mode: mode.as_str(),
                        path: path.display().to_string(),
                    });
                }
                target_value != current_value && worktree_matches_index
            }
            ResetMode::Keep => {
                let has_local_changes = current_value != head_value || !worktree_matches_index;
                if target_changes_head && has_local_changes {
                    return Err(ResetError::LocalChangesWouldBeOverwritten {
                        mode: mode.as_str(),
                        path: path.display().to_string(),
                    });
                }
                target_changes_head
            }
            _ => false,
        };
        if should_update {
            if target_value.is_some()
                && let Some(conflict) = untracked
                    .iter()
                    .find(|untracked_path| worktree::paths_conflict(untracked_path, &path))
            {
                return Err(ResetError::LocalChangesWouldBeOverwritten {
                    mode: mode.as_str(),
                    path: conflict.display().to_string(),
                });
            }
            if let Some(value) = target_value {
                let _: git_internal::internal::object::blob::Blob = load_object(&value.hash)
                    .map_err(|error| {
                        object_load_error("blob", value.hash.to_string(), error.to_string())
                    })?;
            }
            worktree_updates.push((path, target_value));
        }
    }

    if matches!(mode, ResetMode::Merge) {
        carry_unmerged_entries(current_index, &mut target_index)?;
    }
    Ok(GuardedResetPlan {
        target_index,
        worktree_updates,
    })
}

fn capture_index_snapshot() -> Result<IndexSnapshot, ResetError> {
    let index_path = path::index();
    match fs::read(&index_path) {
        Ok(bytes) => Ok(IndexSnapshot {
            existed: true,
            bytes,
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(IndexSnapshot {
            existed: false,
            bytes: Vec::new(),
        }),
        Err(error) => Err(ResetError::IndexLoad(error.to_string())),
    }
}

fn capture_worktree_snapshot(
    relative_path: &Path,
    original_value: Option<ResetIndexValue>,
) -> Result<WorktreePathSnapshot, ResetError> {
    if let Some(blocker) = blocking_worktree_ancestor(relative_path)? {
        if original_value.is_none() {
            return Ok(WorktreePathSnapshot {
                path: relative_path.to_path_buf(),
                kind: WorktreeSnapshotKind::Missing,
            });
        }
        return Err(ResetError::WorktreeRead(format!(
            "tracked path '{}' is blocked by ancestor '{}'",
            relative_path.display(),
            blocker.display()
        )));
    }
    let full_path = util::working_dir().join(relative_path);
    let kind = match fs::symlink_metadata(&full_path) {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            WorktreeSnapshotKind::Missing
        }
        Err(error) => {
            return Err(ResetError::WorktreeRead(format!(
                "failed to inspect {}: {error}",
                full_path.display()
            )));
        }
        Ok(metadata) if metadata.is_dir() => WorktreeSnapshotKind::Directory,
        Ok(_) => WorktreeSnapshotKind::Tracked(original_value.ok_or_else(|| {
            ResetError::WorktreeRead(format!(
                "cannot snapshot untracked path '{}' for guarded reset",
                relative_path.display()
            ))
        })?),
    };
    Ok(WorktreePathSnapshot {
        path: relative_path.to_path_buf(),
        kind,
    })
}

fn remove_worktree_path(relative_path: &Path) -> Result<(), ResetError> {
    let full_path = util::working_dir().join(relative_path);
    match fs::symlink_metadata(&full_path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => fs::remove_file(&full_path).map_err(|error| {
            ResetError::WorktreeRestore(format!(
                "failed to remove {}: {error}",
                full_path.display()
            ))
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ResetError::WorktreeRead(format!(
            "failed to inspect {}: {error}",
            full_path.display()
        ))),
    }
}

fn write_guarded_worktree_value(
    relative_path: &Path,
    value: ResetIndexValue,
) -> Result<(), ResetError> {
    if let Some(blocker) = blocking_worktree_ancestor(relative_path)? {
        return Err(ResetError::WorktreeRestore(format!(
            "refusing to write '{}' through non-directory or symlink ancestor '{}'",
            relative_path.display(),
            blocker.display()
        )));
    }
    let full_path = util::working_dir().join(relative_path);
    let blob: git_internal::internal::object::blob::Blob = load_object(&value.hash)
        .map_err(|error| object_load_error("blob", value.hash.to_string(), error.to_string()))?;
    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            ResetError::WorktreeRestore(format!(
                "failed to create directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    if full_path.is_dir() {
        fs::remove_dir(&full_path).map_err(|error| {
            ResetError::WorktreeRestore(format!(
                "failed to replace directory {}: {error}",
                full_path.display()
            ))
        })?;
    }
    let mode = index_mode_to_tree_mode(value.mode)?;
    write_worktree_entry(&full_path, mode, &blob.data)?;
    apply_worktree_blob_mode(&full_path, mode)
}

fn apply_guarded_worktree_updates(
    updates: &[(PathBuf, Option<ResetIndexValue>)],
) -> Result<ResetStats, ResetError> {
    let mut deletions: Vec<_> = updates
        .iter()
        .filter(|(_, value)| value.is_none())
        .map(|(path, _)| path)
        .collect();
    deletions.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for path in &deletions {
        remove_worktree_path(path)?;
    }
    let warnings = remove_empty_parents_after_guarded_delete(&deletions);

    let mut writes: Vec<_> = updates
        .iter()
        .filter_map(|(path, value)| value.map(|value| (path, value)))
        .collect();
    writes.sort_by_key(|(path, _)| path.components().count());
    for (path, value) in writes {
        write_guarded_worktree_value(path, value)?;
    }
    Ok(ResetStats {
        files_restored: updates.len(),
        warnings,
    })
}

fn remove_empty_parents_after_guarded_delete(paths: &[&PathBuf]) -> Vec<String> {
    let workdir = util::working_dir();
    let mut parents: Vec<PathBuf> = paths
        .iter()
        .filter_map(|path| workdir.join(path).parent().map(Path::to_path_buf))
        .collect();
    parents.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    parents.dedup();

    let mut warnings = Vec::new();
    for mut parent in parents {
        while parent != workdir && parent.starts_with(&workdir) {
            if parent.file_name().and_then(|name| name.to_str()) == Some(".libra")
                || util::check_gitignore(&workdir, &parent)
            {
                break;
            }
            match fs::remove_dir(&parent) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
                    ) =>
                {
                    break;
                }
                Err(error) => {
                    warnings.push(format!(
                        "failed to remove empty directory {}: {error}",
                        parent.display()
                    ));
                    break;
                }
            }
            let Some(next) = parent.parent() else {
                break;
            };
            parent = next.to_path_buf();
        }
    }
    warnings
}

fn restore_index_snapshot(snapshot: &IndexSnapshot) -> Result<(), ResetError> {
    let index_path = path::index();
    if snapshot.existed {
        crate::utils::atomic_write::write_atomic(&index_path, &snapshot.bytes, true)
            .map_err(|error| ResetError::IndexSave(error.to_string()))
    } else if index_path.exists() {
        fs::remove_file(&index_path).map_err(|error| ResetError::IndexSave(error.to_string()))
    } else {
        Ok(())
    }
}

fn restore_worktree_snapshots(snapshots: &[WorktreePathSnapshot]) -> Result<(), ResetError> {
    let mut ordered: Vec<_> = snapshots.iter().collect();
    ordered.sort_by_key(|snapshot| std::cmp::Reverse(snapshot.path.components().count()));
    for snapshot in &ordered {
        if blocking_worktree_ancestor(&snapshot.path)?.is_some() {
            // The path cannot exist without following the blocker. A shallower
            // snapshot will clear/restore that ancestor before the write pass.
            continue;
        }
        let full_path = util::working_dir().join(&snapshot.path);
        match fs::symlink_metadata(&full_path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                if !matches!(snapshot.kind, WorktreeSnapshotKind::Directory) {
                    fs::remove_dir(&full_path).map_err(|error| {
                        ResetError::WorktreeRestore(format!(
                            "failed to clear directory {} during rollback: {error}",
                            full_path.display()
                        ))
                    })?;
                }
            }
            Ok(_) => fs::remove_file(&full_path).map_err(|error| {
                ResetError::WorktreeRestore(format!(
                    "failed to clear {} during rollback: {error}",
                    full_path.display()
                ))
            })?,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) => {}
            Err(error) => {
                return Err(ResetError::WorktreeRead(format!(
                    "failed to inspect {} during rollback: {error}",
                    full_path.display()
                )));
            }
        }
    }
    ordered.sort_by_key(|snapshot| snapshot.path.components().count());
    for snapshot in ordered {
        if let Some(blocker) = blocking_worktree_ancestor(&snapshot.path)? {
            if matches!(snapshot.kind, WorktreeSnapshotKind::Missing) {
                continue;
            }
            return Err(ResetError::WorktreeRestore(format!(
                "refusing to restore '{}' through non-directory or symlink ancestor '{}'",
                snapshot.path.display(),
                blocker.display()
            )));
        }
        let full_path = util::working_dir().join(&snapshot.path);
        match &snapshot.kind {
            WorktreeSnapshotKind::Missing => {}
            WorktreeSnapshotKind::Directory => fs::create_dir_all(&full_path).map_err(|error| {
                ResetError::WorktreeRestore(format!(
                    "failed to restore directory {}: {error}",
                    full_path.display()
                ))
            })?,
            WorktreeSnapshotKind::Tracked(value) => {
                if let Some(parent) = full_path.parent() {
                    fs::create_dir_all(parent).map_err(|error| {
                        ResetError::WorktreeRestore(format!(
                            "failed to restore directory {}: {error}",
                            parent.display()
                        ))
                    })?;
                }
                let blob: git_internal::internal::object::blob::Blob = load_object(&value.hash)
                    .map_err(|error| {
                        object_load_error("blob", value.hash.to_string(), error.to_string())
                    })?;
                let mode = index_mode_to_tree_mode(value.mode)?;
                write_worktree_entry(&full_path, mode, &blob.data)?;
                apply_worktree_blob_mode(&full_path, mode)?;
            }
        }
    }
    Ok(())
}

async fn perform_guarded_reset(
    target_commit_id: ObjectHash,
    old_oid: ObjectHash,
    current_head_state: Option<Head>,
    mode: ResetMode,
    target_ref_str: &str,
) -> Result<ResetStats, ResetError> {
    let current_index =
        Index::load(path::index()).map_err(|error| ResetError::IndexLoad(error.to_string()))?;
    let head_index = index_for_commit(&old_oid)?;
    let target_index = index_for_commit(&target_commit_id)?;
    let plan = build_guarded_reset_plan(mode, &current_index, &head_index, target_index)?;
    let index_snapshot = capture_index_snapshot()?;
    let current_values = stage_zero_values(&current_index)?;
    let worktree_snapshots = plan
        .worktree_updates
        .iter()
        .map(|(path, _)| capture_worktree_snapshot(path, current_values.get(path).copied()))
        .collect::<Result<Vec<_>, _>>()?;

    let apply_result = (|| {
        plan.target_index
            .save(path::index())
            .map_err(|error| ResetError::IndexSave(error.to_string()))?;
        apply_guarded_worktree_updates(&plan.worktree_updates)
    })();
    let stats = match apply_result {
        Ok(stats) => stats,
        Err(error) => {
            let worktree_rollback = restore_worktree_snapshots(&worktree_snapshots);
            let index_rollback = restore_index_snapshot(&index_snapshot);
            let rollback = worktree_rollback.and(index_rollback);
            return Err(merge_reset_failure(error, rollback));
        }
    };

    if let Some(current_head_state) = current_head_state
        && let Err(error) = update_reset_reference(
            current_head_state,
            old_oid,
            target_commit_id,
            target_ref_str,
        )
        .await
    {
        let worktree_rollback = restore_worktree_snapshots(&worktree_snapshots);
        let index_rollback = restore_index_snapshot(&index_snapshot);
        let rollback = worktree_rollback.and(index_rollback);
        return Err(merge_reset_failure(error, rollback));
    }
    Ok(stats)
}

/// Perform the actual reset operation based on the specified mode.
/// Updates HEAD pointer and optionally resets index and working directory.
async fn perform_reset(
    target_commit_id: ObjectHash,
    mode: ResetMode,
    target_ref_str: &str, // e.g, "HEAD~2"
) -> Result<ResetStats, ResetError> {
    // avoids holding the transaction open while doing read-only preparations.
    let db = get_db_conn_instance().await;
    let old_oid = Head::current_commit_result_with_conn(&db)
        .await
        .map_err(map_reset_head_commit_error)?
        .ok_or(ResetError::HeadUnborn)?;
    let current_head_state = if old_oid != target_commit_id {
        Some(Head::current_with_conn(&db).await)
    } else {
        None
    };
    if matches!(mode, ResetMode::Merge | ResetMode::Keep) {
        return perform_guarded_reset(
            target_commit_id,
            old_oid,
            current_head_state,
            mode,
            target_ref_str,
        )
        .await;
    }
    let previously_tracked_paths = if matches!(mode, ResetMode::Hard) {
        tracked_paths_for_hard_reset(&old_oid)?
    } else {
        HashSet::new()
    };
    // INVARIANT: apply index/worktree changes before moving HEAD. If a
    // filesystem write fails, rollback can still restore the old index/worktree
    // while refs continue to point at the previous commit.
    let stats =
        match apply_reset_side_effects(mode, &target_commit_id, &previously_tracked_paths).await {
            Ok(stats) => stats,
            Err(error) => {
                let rollback = rollback_reset_side_effects(mode, &old_oid, &target_commit_id).await;
                return Err(merge_reset_failure(error, rollback));
            }
        };

    if let Some(current_head_state) = current_head_state
        && let Err(error) = update_reset_reference(
            current_head_state,
            old_oid,
            target_commit_id,
            target_ref_str,
        )
        .await
    {
        // INVARIANT: if the final ref move fails after side effects, restore the
        // index/worktree to match the old commit so the visible checkout does
        // not diverge from HEAD.
        let rollback = rollback_reset_side_effects(mode, &old_oid, &target_commit_id).await;
        return Err(merge_reset_failure(error, rollback));
    }

    Ok(stats)
}

async fn apply_reset_side_effects(
    mode: ResetMode,
    target_commit_id: &ObjectHash,
    previously_tracked_paths: &HashSet<PathBuf>,
) -> Result<ResetStats, ResetError> {
    let mut stats = ResetStats::default();
    match mode {
        ResetMode::Soft => {}
        ResetMode::Mixed => {
            reset_index_to_commit_typed(target_commit_id)?;
        }
        ResetMode::Hard => {
            reset_index_to_commit_typed(target_commit_id)?;
            let worktree_stats =
                reset_working_directory_to_commit(target_commit_id, previously_tracked_paths)
                    .await?;
            stats.files_restored = worktree_stats.files_restored;
            stats.warnings = worktree_stats.warnings;
        }
        ResetMode::Merge | ResetMode::Keep => {
            return Err(ResetError::WorktreeRestore(
                "internal reset mode dispatch failure".to_string(),
            ));
        }
    }
    Ok(stats)
}

async fn rollback_reset_side_effects(
    mode: ResetMode,
    old_oid: &ObjectHash,
    target_commit_id: &ObjectHash,
) -> Result<(), ResetError> {
    match mode {
        ResetMode::Soft => Ok(()),
        ResetMode::Mixed => reset_index_to_commit_typed(old_oid),
        ResetMode::Hard => {
            reset_index_to_commit_typed(old_oid)?;
            let rollback_paths = tracked_paths_for_hard_reset(target_commit_id)?;
            let rollback_stats =
                reset_working_directory_to_commit(old_oid, &rollback_paths).await?;
            if !rollback_stats.warnings.is_empty() {
                tracing::warn!(
                    warnings = ?rollback_stats.warnings,
                    "rollback after reset completed with worktree warnings"
                );
            }
            Ok(())
        }
        ResetMode::Merge | ResetMode::Keep => Err(ResetError::WorktreeRestore(
            "internal reset rollback mode dispatch failure".to_string(),
        )),
    }
}

fn load_commit_summary_or_warn(commit_id: &ObjectHash) -> String {
    get_commit_summary(commit_id).unwrap_or_else(|error| {
        tracing::warn!("failed to load commit summary for {commit_id}: {error}");
        String::new()
    })
}

async fn update_reset_reference(
    current_head_state: Head,
    old_oid: ObjectHash,
    target_commit_id: ObjectHash,
    target_ref_str: &str,
) -> Result<(), ResetError> {
    let action = ReflogAction::Reset {
        target: target_ref_str.to_string(),
    };
    let context = ReflogContext {
        old_oid: old_oid.to_string(),
        new_oid: target_commit_id.to_string(),
        action,
    };

    with_reflog(
        context,
        move |txn| {
            Box::pin(async move {
                match &current_head_state {
                    Head::Branch(branch_name) => {
                        Branch::update_branch_with_conn(
                            txn,
                            branch_name,
                            &target_commit_id.to_string(),
                            None,
                        )
                        .await?;
                    }
                    Head::Detached(_) => {
                        let new_head = Head::Detached(target_commit_id);
                        // Propagated, not logged-and-dropped: a HEAD write
                        // that fails while the surrounding work commits
                        // leaves the repository pointing at the wrong commit
                        // and reports success.
                        Head::update_result_with_conn(txn, new_head, None)
                            .await
                            .map_err(|error| sea_orm::DbErr::Custom(error.to_string()))?;
                    }
                }
                Ok(())
            })
        },
        true,
    )
    .await
    .map_err(|e| ResetError::HeadUpdate(e.to_string()))
}

fn merge_reset_failure(error: ResetError, rollback: Result<(), ResetError>) -> ResetError {
    match rollback {
        Ok(()) => error,
        Err(rollback_error) => ResetError::Rollback {
            primary: Box::new(error),
            rollback: Box::new(rollback_error),
        },
    }
}

/// Reset the index to match the specified commit's tree.
/// Clears the current index and rebuilds it from the commit's tree structure.
pub(crate) fn reset_index_to_commit(commit_id: &ObjectHash) -> Result<(), String> {
    reset_index_to_commit_typed(commit_id).map_err(|e| e.to_string())
}

/// Reset the working directory to match the specified commit.
/// Removes files that exist in the original commit but not in the target commit,
/// and restores files from the target commit's tree.
async fn reset_working_directory_to_commit(
    commit_id: &ObjectHash,
    previously_tracked_paths: &HashSet<PathBuf>,
) -> Result<ResetStats, ResetError> {
    let commit: Commit = load_object(commit_id)
        .map_err(|e| object_load_error("commit", commit_id.to_string(), e.to_string()))?;

    let tree: Tree = load_object(&commit.tree_id)
        .map_err(|e| object_load_error("tree", commit.tree_id.to_string(), e.to_string()))?;

    let workdir = util::working_dir();
    // `get_plain_items` omits `160000` gitlinks, so the target set must be
    // taken with modes: otherwise a submodule the target tree still declares
    // would look "removed" and the loop below would try to `remove_file` the
    // submodule DIRECTORY (ADR-MG-01: submodule working trees are not Libra's
    // to delete).
    let target_files = tree.get_plain_items_with_mode();
    let target_files_set: HashSet<_> = target_files
        .iter()
        .map(|(path, _, _)| path.clone())
        .collect();
    let mut files_restored = 0;

    // Remove tracked files that should not exist in the target tree.
    for file_path in previously_tracked_paths {
        if !target_files_set.contains(file_path) {
            let full_path = workdir.join(file_path);
            if full_path.exists() {
                fs::remove_file(&full_path).map_err(|e| {
                    ResetError::WorktreeRestore(format!(
                        "failed to remove file {}: {}",
                        full_path.display(),
                        e
                    ))
                })?;
                files_restored += 1;
            }
        }
    }

    // Remove empty directories
    let warnings = remove_empty_directories_with_warnings(&workdir)?;

    // Restore files from target tree
    files_restored += restore_working_directory_from_tree_counted_typed(&tree, &workdir, "")?;

    Ok(ResetStats {
        files_restored,
        warnings,
    })
}

/// Recursively rebuild the index from a tree structure.
/// Traverses the tree and adds all files to the index with their blob hashes.
pub(crate) fn rebuild_index_from_tree(
    tree: &Tree,
    index: &mut Index,
    prefix: &str,
) -> Result<(), String> {
    rebuild_index_from_tree_typed(tree, index, prefix).map_err(|e| e.to_string())
}

fn reset_index_to_commit_typed(commit_id: &ObjectHash) -> Result<(), ResetError> {
    let commit: Commit = load_object(commit_id)
        .map_err(|e| object_load_error("commit", commit_id.to_string(), e.to_string()))?;

    let tree: Tree = load_object(&commit.tree_id)
        .map_err(|e| object_load_error("tree", commit.tree_id.to_string(), e.to_string()))?;

    let index_file = path::index();
    let mut index = Index::new();

    rebuild_index_from_tree_typed(&tree, &mut index, "")?;

    index
        .save(&index_file)
        .map_err(|e| ResetError::IndexSave(e.to_string()))?;

    Ok(())
}

fn rebuild_index_from_tree_typed(
    tree: &Tree,
    index: &mut Index,
    prefix: &str,
) -> Result<(), ResetError> {
    for item in &tree.tree_items {
        let full_path = if prefix.is_empty() {
            item.name.clone()
        } else {
            format!("{}/{}", prefix, item.name)
        };

        match item.mode {
            TreeItemMode::Tree => {
                let subtree: Tree = load_object(&item.id)
                    .map_err(|e| object_load_error("tree", item.id.to_string(), e.to_string()))?;
                rebuild_index_from_tree_typed(&subtree, index, &full_path)?;
            }
            // A `160000` gitlink records a SUBMODULE's commit id, which is not
            // an object of THIS repository — `Blob::load` would have panicked
            // trying to size it. Record the pointer verbatim (size 0) so the
            // rebuilt index keeps the submodule the tree declares, matching what
            // `ls-files`/`status` already expect of a gitlink index entry.
            TreeItemMode::Commit => {
                let mut entry = IndexEntry::new_from_blob(full_path, item.id, 0);
                entry.mode = tree_item_mode_to_index_mode(item.mode)?;
                index.add(entry);
            }
            _ => {
                // Add file to index - but don't modify working directory files
                // Use the blob hash from the tree, not from working directory
                // Get blob size for IndexEntry
                let blob = git_internal::internal::object::blob::Blob::load(&item.id);

                // Create IndexEntry with the tree's blob hash
                let mut entry =
                    IndexEntry::new_from_blob(full_path, item.id, blob.data.len() as u32);
                entry.mode = tree_item_mode_to_index_mode(item.mode)?;
                index.add(entry);
            }
        }
    }
    Ok(())
}

fn tree_item_mode_to_index_mode(mode: TreeItemMode) -> Result<u32, ResetError> {
    match mode {
        TreeItemMode::Blob => Ok(0o100644),
        TreeItemMode::BlobExecutable => Ok(0o100755),
        TreeItemMode::Link => Ok(0o120000),
        TreeItemMode::Commit => Ok(0o160000),
        TreeItemMode::Tree => Err(ResetError::RevisionCorrupt(
            "tree entry cannot be stored directly in index".to_string(),
        )),
    }
}

/// Restore the working directory from a tree structure.
/// Recursively creates directories and writes files from the tree's blob objects.
pub(crate) fn restore_working_directory_from_tree(
    tree: &Tree,
    workdir: &Path,
    prefix: &str,
) -> Result<(), String> {
    restore_working_directory_from_tree_counted_typed(tree, workdir, prefix)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn restore_working_directory_from_tree_counted_typed(
    tree: &Tree,
    workdir: &Path,
    prefix: &str,
) -> Result<usize, ResetError> {
    let mut files_restored = 0;
    for item in &tree.tree_items {
        let full_path = if prefix.is_empty() {
            item.name.clone()
        } else {
            format!("{}/{}", prefix, item.name)
        };

        let file_path = workdir.join(&full_path);

        match item.mode {
            TreeItemMode::Tree => {
                // Create directory
                fs::create_dir_all(&file_path).map_err(|e| {
                    ResetError::WorktreeRestore(format!(
                        "failed to create directory {}: {}",
                        file_path.display(),
                        e
                    ))
                })?;

                let subtree: Tree = load_object(&item.id)
                    .map_err(|e| object_load_error("tree", item.id.to_string(), e.to_string()))?;
                files_restored += restore_working_directory_from_tree_counted_typed(
                    &subtree, workdir, &full_path,
                )?;
            }
            // A `160000` gitlink points at a SUBMODULE's commit, which is not
            // an object of this repository and has no working-tree file of its
            // own: Libra never materializes submodule working trees (see
            // `TreeExt::get_plain_items`). Restoring one is a no-op rather than
            // a failed blob load.
            TreeItemMode::Commit => {}
            _ => {
                let blob = load_object::<git_internal::internal::object::blob::Blob>(&item.id)
                    .map_err(|e| object_load_error("blob", item.id.to_string(), e.to_string()))?;

                // Create parent directory if needed
                if let Some(parent) = file_path.parent() {
                    fs::create_dir_all(parent).map_err(|e| {
                        ResetError::WorktreeRestore(format!(
                            "failed to create directory {}: {}",
                            parent.display(),
                            e
                        ))
                    })?;
                }

                let needs_write = !worktree_entry_matches(&file_path, item.mode, &blob.data)?;

                if needs_write {
                    write_worktree_entry(&file_path, item.mode, &blob.data)?;
                    files_restored += 1;
                }
                apply_worktree_blob_mode(&file_path, item.mode)?;
            }
        }
    }
    Ok(files_restored)
}

fn worktree_entry_matches(
    path: &Path,
    mode: TreeItemMode,
    expected: &[u8],
) -> Result<bool, ResetError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(ResetError::WorktreeRead(format!(
                "failed to inspect file {}: {}",
                path.display(),
                error
            )));
        }
    };

    if metadata.file_type().is_symlink() {
        if mode != TreeItemMode::Link {
            return Ok(false);
        }
        let target = fs::read_link(path).map_err(|error| {
            ResetError::WorktreeRead(format!(
                "failed to read symlink {}: {}",
                path.display(),
                error
            ))
        })?;
        return Ok(symlink_target_blob_bytes(&target) == expected);
    }

    if mode == TreeItemMode::Link {
        return Ok(false);
    }

    #[cfg(unix)]
    if matches!(mode, TreeItemMode::Blob | TreeItemMode::BlobExecutable) {
        use std::os::unix::fs::PermissionsExt;
        let actual_executable = metadata.permissions().mode() & 0o111 != 0;
        let expected_executable = mode == TreeItemMode::BlobExecutable;
        if actual_executable != expected_executable {
            return Ok(false);
        }
    }

    match fs::read(path) {
        Ok(existing) => Ok(existing == expected),
        Err(_) if metadata.is_dir() => Ok(false),
        Err(error) => Err(ResetError::WorktreeRead(format!(
            "failed to read file {}: {}",
            path.display(),
            error
        ))),
    }
}

fn write_worktree_entry(path: &Path, mode: TreeItemMode, content: &[u8]) -> Result<(), ResetError> {
    if mode == TreeItemMode::Link {
        return write_worktree_symlink(path, content);
    }

    remove_existing_symlink(path)?;
    fs::write(path, content).map_err(|error| {
        ResetError::WorktreeRestore(format!(
            "failed to write file {}: {}",
            path.display(),
            error
        ))
    })
}

fn remove_existing_symlink(path: &Path) -> Result<(), ResetError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::remove_file(path).map_err(|error| {
                ResetError::WorktreeRestore(format!(
                    "failed to replace symlink {}: {}",
                    path.display(),
                    error
                ))
            })
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ResetError::WorktreeRead(format!(
            "failed to inspect file {}: {}",
            path.display(),
            error
        ))),
    }
}

#[cfg(unix)]
fn write_worktree_symlink(path: &Path, target: &[u8]) -> Result<(), ResetError> {
    use std::{
        ffi::OsStr,
        os::unix::{ffi::OsStrExt, fs::symlink},
    };

    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            return Err(ResetError::WorktreeRestore(format!(
                "cannot replace directory {} with symlink",
                path.display()
            )));
        }
        Ok(_) => fs::remove_file(path).map_err(|error| {
            ResetError::WorktreeRestore(format!(
                "failed to replace path {} with symlink: {}",
                path.display(),
                error
            ))
        })?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(ResetError::WorktreeRead(format!(
                "failed to inspect file {}: {}",
                path.display(),
                error
            )));
        }
    }

    let target = Path::new(OsStr::from_bytes(target));
    symlink(target, path).map_err(|error| {
        ResetError::WorktreeRestore(format!(
            "failed to create symlink {}: {}",
            path.display(),
            error
        ))
    })
}

#[cfg(not(unix))]
fn write_worktree_symlink(path: &Path, _target: &[u8]) -> Result<(), ResetError> {
    Err(ResetError::WorktreeRestore(format!(
        "symlink checkout is not supported on this platform: {}",
        path.display()
    )))
}

#[cfg(unix)]
fn apply_worktree_blob_mode(path: &Path, mode: TreeItemMode) -> Result<(), ResetError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = match mode {
        TreeItemMode::Blob => Some(0o644),
        TreeItemMode::BlobExecutable => Some(0o755),
        _ => None,
    };
    if let Some(mode) = mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| {
            ResetError::WorktreeRestore(format!(
                "failed to set mode on {}: {}",
                path.display(),
                error
            ))
        })?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_worktree_blob_mode(_path: &Path, _mode: TreeItemMode) -> Result<(), ResetError> {
    Ok(())
}

/// Remove empty directories from the working directory.
/// Recursively traverses the directory tree and removes any empty directories,
/// except for the .libra directory and the working directory root.
///
/// This is a backward-compatible shim for callers (e.g. `stash.rs`) that do
/// not have a warning pipeline.  Non-fatal directory-removal warnings are
/// intentionally dropped here; the typed reset path collects them via
/// [`remove_empty_directories_with_warnings`] and routes them through
/// `emit_warning()`.
pub(crate) fn remove_empty_directories(workdir: &Path) -> Result<(), String> {
    remove_empty_directories_with_warnings(workdir)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn remove_empty_directories_with_warnings(workdir: &Path) -> Result<Vec<String>, ResetError> {
    let workdir_buf = workdir.to_path_buf();
    fn remove_empty_dirs_recursive(
        dir: &Path,
        workdir: &Path,
        workdir_buf: &PathBuf,
        warnings: &mut Vec<String>,
    ) -> Result<bool, ResetError> {
        if !dir.is_dir() || dir == workdir {
            return Ok(true);
        }

        let entries = fs::read_dir(dir).map_err(|e| {
            ResetError::WorktreeRead(format!("failed to read directory {}: {}", dir.display(), e))
        })?;

        let mut has_files = false;

        for entry in entries {
            let entry = entry.map_err(|e| {
                ResetError::WorktreeRead(format!("failed to read directory entry: {e}"))
            })?;
            let path = entry.path();

            if path.is_dir() {
                // Don't remove .libra directory or ignored directories
                if path.file_name().and_then(|n| n.to_str()) == Some(".libra")
                    || util::check_gitignore(workdir_buf, &path)
                {
                    has_files = true;
                } else {
                    has_files |=
                        remove_empty_dirs_recursive(&path, workdir, workdir_buf, warnings)?;
                }
            } else {
                has_files = true;
            }
        }

        // Remove this directory if it's empty and not the working directory
        if !has_files && dir != workdir {
            if let Err(e) = fs::remove_dir(dir) {
                warnings.push(format!(
                    "failed to remove empty directory {}: {}",
                    dir.display(),
                    e
                ));
                return Ok(true);
            }
            return Ok(false);
        }

        Ok(has_files)
    }

    // Start from working directory and process all subdirectories
    let entries = fs::read_dir(workdir)
        .map_err(|e| ResetError::WorktreeRead(format!("failed to read working directory: {e}")))?;
    let mut warnings = Vec::new();

    for entry in entries {
        let entry = entry.map_err(|e| {
            ResetError::WorktreeRead(format!("failed to read directory entry: {e}"))
        })?;
        let path = entry.path();

        if path.is_dir()
            && path.file_name().and_then(|n| n.to_str()) != Some(".libra")
            && !util::check_gitignore(&workdir_buf, &path)
        {
            let _ = remove_empty_dirs_recursive(&path, workdir, &workdir_buf, &mut warnings)?;
        }
    }

    Ok(warnings)
}

/// Resolve a reference string to a commit ObjectHash.
/// Accepts commit hashes, branch names, or HEAD references.
async fn resolve_commit(reference: &str) -> Result<ObjectHash, ResetError> {
    util::get_commit_base_typed(reference)
        .await
        .map_err(map_commit_base_error)
}

fn map_commit_base_error(error: util::CommitBaseError) -> ResetError {
    match error {
        util::CommitBaseError::HeadUnborn => ResetError::HeadUnborn,
        util::CommitBaseError::InvalidReference(message) => ResetError::InvalidRevision(message),
        util::CommitBaseError::ReadFailure(message) => ResetError::RevisionRead(message),
        util::CommitBaseError::CorruptReference(message) => ResetError::RevisionCorrupt(message),
    }
}

/// Get the first line of a commit's message for display purposes.
fn get_commit_summary(commit_id: &ObjectHash) -> Result<String, ResetError> {
    let commit: Commit = load_object(commit_id)
        .map_err(|e| object_load_error("commit", commit_id.to_string(), e.to_string()))?;

    let first_line = parse_commit_msg(&commit.message)
        .0
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    Ok(first_line)
}

fn tracked_paths_from_index() -> Result<HashSet<PathBuf>, ResetError> {
    let index = Index::load(path::index()).map_err(|e| ResetError::IndexLoad(e.to_string()))?;
    // `160000` gitlinks are excluded on purpose. Libra never materializes a
    // submodule working tree (ADR-MG-01), so whatever sits at that path was not
    // written by Libra and is not Libra's to remove — deleting it would destroy
    // either the user's submodule checkout or an unrelated file they left
    // there. (`tracked_paths_from_commit` gets this for free: `get_plain_items`
    // already skips gitlinks.)
    Ok(index
        .tracked_entries(0)
        .into_iter()
        .filter(|entry| entry.mode & 0o170000 != 0o160000)
        .map(|entry| PathBuf::from(&entry.name))
        .collect())
}

fn tracked_paths_from_commit(commit_id: &ObjectHash) -> Result<HashSet<PathBuf>, ResetError> {
    let commit: Commit = load_object(commit_id)
        .map_err(|e| object_load_error("commit", commit_id.to_string(), e.to_string()))?;
    let tree: Tree = load_object(&commit.tree_id)
        .map_err(|e| object_load_error("tree", commit.tree_id.to_string(), e.to_string()))?;
    Ok(tree
        .get_plain_items()
        .into_iter()
        .map(|(path, _)| path)
        .collect())
}

fn tracked_paths_for_hard_reset(
    current_commit_id: &ObjectHash,
) -> Result<HashSet<PathBuf>, ResetError> {
    // `reset --hard` must remove paths that are tracked either by the current HEAD
    // tree or by the staged index, otherwise cached removals can leave stale files
    // behind when the target commit does not contain them.
    let mut tracked_paths = tracked_paths_from_commit(current_commit_id)?;
    tracked_paths.extend(tracked_paths_from_index()?);
    Ok(tracked_paths)
}

fn render_reset_output(result: &ResetOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("reset", result, output);
    }

    if output.quiet {
        return Ok(());
    }

    if result.pathspecs.is_empty() {
        if result.subject.is_empty() {
            println!("HEAD is now at {}", result.short_commit);
        } else {
            println!("HEAD is now at {} {}", result.short_commit, result.subject);
        }
    } else {
        println!("Unstaged changes after reset:");
        for path in &result.pathspecs {
            println!("M\t{path}");
        }
    }

    Ok(())
}

/// Find a specific file or directory in a tree by path.
/// Returns the tree item if found, None otherwise.
fn find_tree_item(
    tree: &Tree,
    path: &str,
) -> Result<Option<git_internal::internal::object::tree::TreeItem>, ResetError> {
    let parts: Vec<&str> = path.split('/').collect();
    find_tree_item_recursive(tree, &parts, 0)
}

/// Recursively search for a tree item by path components.
/// Helper function for find_tree_item that handles nested directory structures.
fn find_tree_item_recursive(
    tree: &Tree,
    parts: &[&str],
    index: usize,
) -> Result<Option<git_internal::internal::object::tree::TreeItem>, ResetError> {
    if index >= parts.len() {
        return Ok(None);
    }

    for item in &tree.tree_items {
        if item.name == parts[index] {
            if index == parts.len() - 1 {
                // Found the target
                return Ok(Some(item.clone()));
            } else if item.mode == TreeItemMode::Tree {
                // Continue searching in subtree
                let subtree = load_object::<Tree>(&item.id)
                    .map_err(|e| object_load_error("tree", item.id.to_string(), e.to_string()))?;
                if let Some(result) = find_tree_item_recursive(&subtree, parts, index + 1)? {
                    return Ok(Some(result));
                }
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[serial_test::serial]
    async fn guarded_worktree_snapshots_restore_file_directory_transitions() {
        let temp = tempfile::tempdir().expect("create reset snapshot test directory");
        let _guard = crate::utils::test::ChangeDirGuard::new(temp.path());
        crate::utils::test::setup_with_new_libra_in(temp.path()).await;

        let file_data = b"original file\n".to_vec();
        let file_blob =
            git_internal::internal::object::blob::Blob::from_content_bytes(file_data.clone());
        crate::command::save_object(&file_blob, &file_blob.id).expect("save rollback source blob");
        let file_value = ResetIndexValue {
            hash: file_blob.id,
            mode: 0o100644,
        };

        fs::write("node", &file_data).expect("write file-form snapshot source");
        let file_snapshots = vec![
            capture_worktree_snapshot(Path::new("node"), Some(file_value))
                .expect("capture tracked file"),
            capture_worktree_snapshot(Path::new("node/child"), None)
                .expect("a child below a file is missing, not an I/O failure"),
        ];
        fs::remove_file("node").expect("remove file-form source");
        fs::create_dir("node").expect("create replacement directory");
        fs::write("node/child", "replacement\n").expect("write replacement child");
        restore_worktree_snapshots(&file_snapshots).expect("roll back directory to file");
        assert_eq!(fs::read("node").expect("read restored file"), file_data);

        let child_data = b"original child\n".to_vec();
        let child_blob =
            git_internal::internal::object::blob::Blob::from_content_bytes(child_data.clone());
        crate::command::save_object(&child_blob, &child_blob.id)
            .expect("save child rollback source blob");
        let child_value = ResetIndexValue {
            hash: child_blob.id,
            mode: 0o100644,
        };

        fs::remove_file("node").expect("remove restored file");
        fs::create_dir("node").expect("create directory-form source");
        fs::write("node/child", &child_data).expect("write child-form snapshot source");
        let directory_snapshots = vec![
            capture_worktree_snapshot(Path::new("node"), None).expect("capture tracked directory"),
            capture_worktree_snapshot(Path::new("node/child"), Some(child_value))
                .expect("capture tracked child"),
        ];
        fs::remove_file("node/child").expect("remove child-form source");
        fs::remove_dir("node").expect("remove source directory");
        fs::write("node", "replacement file\n").expect("write replacement file");
        restore_worktree_snapshots(&directory_snapshots).expect("roll back file to directory");
        assert!(Path::new("node").is_dir());
        assert_eq!(
            fs::read("node/child").expect("read restored child"),
            child_data
        );
    }

    #[test]
    fn guarded_reset_rejects_escaping_and_metadata_paths() {
        assert!(validate_guarded_relative_path(Path::new("src/lib.rs")).is_ok());
        assert!(validate_guarded_relative_path(Path::new("../outside")).is_err());
        assert!(validate_guarded_relative_path(Path::new("./alias")).is_err());
        assert!(validate_guarded_relative_path(Path::new(".libra/index")).is_err());
        assert!(validate_guarded_relative_path(Path::new("/absolute")).is_err());
    }

    #[test]
    fn test_reset_args_parse() {
        let args = ResetArgs::try_parse_from(["reset", "--hard", "HEAD~1"]).unwrap();
        assert!(args.hard);
        assert_eq!(args.target.as_deref(), Some("HEAD~1"));
    }

    /// Pin the `Display` format contract for static-message and
    /// `{0}`-prefixed variants of [`ResetError`]. These strings are
    /// used directly as the CliError message in the `From<ResetError>
    /// for CliError` mapping, so they form part of the human +
    /// --json error envelope contract.
    ///
    /// Source-chained / wrapper variants whose Display body forwards
    /// to upstream error strings (HeadRead, HeadCorrupt, ObjectLoad,
    /// IndexLoad, IndexSave, HeadUpdate, WorktreeRead, WorktreeRestore)
    /// are intentionally skipped — their `{0}` slot is owned by the
    /// wrapped error type.
    #[test]
    fn reset_error_display_pins_static_message_variants() {
        assert_eq!(ResetError::NotInRepo.to_string(), "not a libra repository");
        assert_eq!(
            ResetError::HeadUnborn.to_string(),
            "Cannot reset: HEAD is unborn and points to no commit.",
        );
        assert_eq!(
            ResetError::PathspecWithHard.to_string(),
            "Cannot do hard reset with paths.",
        );
        assert_eq!(
            ResetError::PathspecWithPreservingMode("merge").to_string(),
            "Cannot do merge reset with paths.",
        );
        assert_eq!(
            ResetError::LocalChangesWouldBeOverwritten {
                mode: "keep",
                path: "tracked.txt".to_string(),
            }
            .to_string(),
            "local changes to 'tracked.txt' would be overwritten by reset --keep",
        );
        // {0}-prefixed variants where the inner string IS the message.
        assert_eq!(
            ResetError::InvalidRevision("ambiguous revision 'a'".to_string()).to_string(),
            "ambiguous revision 'a'",
        );
        assert_eq!(
            ResetError::AmbiguousRevisionPath("HEAD".to_string()).to_string(),
            "ambiguous argument 'HEAD': both revision and filename",
        );
        assert_eq!(
            ResetError::RevisionRead("io error".to_string()).to_string(),
            "io error",
        );
        // {0}-suffixed variants where the prefix is the user message.
        assert_eq!(
            ResetError::InvalidPathspecEncoding("src/\\xff".to_string()).to_string(),
            "path contains invalid UTF-8: src/\\xff",
        );
        assert_eq!(
            ResetError::PathspecWithSoft("src/foo.rs".to_string()).to_string(),
            "pathspec 'src/foo.rs' is not compatible with --soft reset",
        );
        assert_eq!(
            ResetError::PathspecNotMatched("src/missing.rs".to_string()).to_string(),
            "pathspec 'src/missing.rs' did not match any file(s) known to libra",
        );
        assert_eq!(
            ResetError::PathspecSourceConflict.to_string(),
            "--pathspec-from-file cannot be combined with command-line pathspecs",
        );
        assert_eq!(
            ResetError::PathspecOutsideWorkdir("../escape.txt".to_string()).to_string(),
            "pathspec '../escape.txt' is outside the repository working directory",
        );
        assert_eq!(
            ResetError::LockedCurrentBranch("traces".to_string()).to_string(),
            "refusing to reset locked current branch 'traces'",
        );
        // ObjectLoad — three structured fields.
        assert_eq!(
            ResetError::ObjectLoad {
                kind: "tree",
                object_id: "deadbeef".to_string(),
                detail: "object not found".to_string(),
            }
            .to_string(),
            "failed to load tree 'deadbeef': object not found",
        );
    }

    /// Pin the `stable_code()` mapping for every variant of
    /// [`ResetError`]. The [`StableErrorCode`] is what `--json`
    /// consumers branch on; ResetError has 21 variants spread across
    /// repo-state (RepoNotFound / RepoStateInvalid / RepoCorrupt),
    /// I/O (IoReadFailed / IoWriteFailed), and CLI input
    /// (CliInvalidArguments / CliInvalidTarget) buckets. A future
    /// refactor that flips even a single mapping silently changes
    /// client retry classification.
    ///
    /// The existing scattered per-variant tests (HeadUnborn,
    /// HeadRead, WorktreeRead, RevisionRead, RevisionCorrupt) keep
    /// their narrative role of documenting one mapping at a time;
    /// this single test owns the exhaustive surface contract so
    /// adding a new variant trips both this list and the
    /// `stable_code()` impl's exhaustive match.
    #[test]
    fn reset_error_stable_code_pins_each_variant() {
        assert_eq!(
            ResetError::NotInRepo.stable_code(),
            StableErrorCode::RepoNotFound,
        );
        assert_eq!(
            ResetError::InvalidRevision("ignored".to_string()).stable_code(),
            StableErrorCode::CliInvalidTarget,
        );
        assert_eq!(
            ResetError::AmbiguousRevisionPath("ignored".to_string()).stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            ResetError::HeadUnborn.stable_code(),
            StableErrorCode::RepoStateInvalid,
        );
        assert_eq!(
            ResetError::HeadRead("ignored".to_string()).stable_code(),
            StableErrorCode::IoReadFailed,
        );
        assert_eq!(
            ResetError::HeadCorrupt("ignored".to_string()).stable_code(),
            StableErrorCode::RepoCorrupt,
        );
        assert_eq!(
            ResetError::ObjectLoad {
                kind: "tree",
                object_id: "ignored".to_string(),
                detail: "ignored".to_string(),
            }
            .stable_code(),
            StableErrorCode::RepoCorrupt,
        );
        assert_eq!(
            ResetError::IndexLoad("ignored".to_string()).stable_code(),
            StableErrorCode::RepoCorrupt,
        );
        assert_eq!(
            ResetError::IndexSave("ignored".to_string()).stable_code(),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            ResetError::HeadUpdate("ignored".to_string()).stable_code(),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            ResetError::WorktreeRead("ignored".to_string()).stable_code(),
            StableErrorCode::IoReadFailed,
        );
        assert_eq!(
            ResetError::WorktreeRestore("ignored".to_string()).stable_code(),
            StableErrorCode::IoWriteFailed,
        );
        assert_eq!(
            ResetError::RevisionRead("ignored".to_string()).stable_code(),
            StableErrorCode::IoReadFailed,
        );
        assert_eq!(
            ResetError::RevisionCorrupt("ignored".to_string()).stable_code(),
            StableErrorCode::RepoCorrupt,
        );
        assert_eq!(
            ResetError::InvalidPathspecEncoding("ignored".to_string()).stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            ResetError::PathspecWithSoft("ignored".to_string()).stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            ResetError::PathspecWithHard.stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            ResetError::PathspecWithPreservingMode("merge").stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            ResetError::LocalChangesWouldBeOverwritten {
                mode: "keep",
                path: "ignored".to_string(),
            }
            .stable_code(),
            StableErrorCode::ConflictOperationBlocked,
        );
        assert_eq!(
            ResetError::PathspecNotMatched("ignored".to_string()).stable_code(),
            StableErrorCode::CliInvalidTarget,
        );
        assert_eq!(
            ResetError::PathspecSourceConflict.stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            ResetError::PathspecOutsideWorkdir("ignored".to_string()).stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            ResetError::PathspecFileRead {
                path: "ignored".to_string(),
                source: io::Error::new(io::ErrorKind::NotFound, "ignored"),
            }
            .stable_code(),
            StableErrorCode::IoReadFailed,
        );
        assert_eq!(
            ResetError::LockedTarget("ignored".to_string()).stable_code(),
            StableErrorCode::CliInvalidTarget,
        );
        assert_eq!(
            ResetError::LockedCurrentBranch("ignored".to_string()).stable_code(),
            StableErrorCode::ConflictOperationBlocked,
        );
        // Rollback delegates to its primary error's stable_code via
        // recursion; pinning the delegation surfaces a future change
        // that would (e.g.) shadow the primary code with the rollback
        // code instead.
        let rollback = ResetError::Rollback {
            primary: Box::new(ResetError::HeadUnborn),
            rollback: Box::new(ResetError::IndexSave("ignored".to_string())),
        };
        assert_eq!(rollback.stable_code(), StableErrorCode::RepoStateInvalid);
    }

    #[test]
    fn test_reset_mode_detection() {
        let args = ResetArgs::try_parse_from(["reset", "--soft"]).unwrap();
        assert!(args.soft);

        let args = ResetArgs::try_parse_from(["reset"]).unwrap();
        assert!(!args.soft && !args.hard && !args.merge && !args.keep);

        let args = ResetArgs::try_parse_from(["reset", "--merge"]).unwrap();
        assert!(args.merge);

        let args = ResetArgs::try_parse_from(["reset", "--keep"]).unwrap();
        assert!(args.keep);
    }

    #[test]
    fn test_reset_error_maps_unborn_head_as_repo_state() {
        let error = CliError::from(ResetError::HeadUnborn);
        assert_eq!(error.stable_code(), StableErrorCode::RepoStateInvalid);
    }

    #[test]
    fn test_reset_error_maps_head_read_failures_as_io_read() {
        let error = CliError::from(ResetError::HeadRead("database is locked".into()));
        assert_eq!(error.stable_code(), StableErrorCode::IoReadFailed);
    }

    #[test]
    fn test_reset_error_maps_file_read_failures_as_io_read() {
        let error = CliError::from(ResetError::WorktreeRead(
            "failed to read file /tmp/repo/tracked.txt: Permission denied".into(),
        ));
        assert_eq!(error.stable_code(), StableErrorCode::IoReadFailed);
    }

    #[test]
    fn test_reset_error_maps_revision_read_failures_as_io_read() {
        let error = CliError::from(ResetError::RevisionRead(
            "failed to resolve branch 'main': failed to query branch storage: database is locked"
                .into(),
        ));
        assert_eq!(error.stable_code(), StableErrorCode::IoReadFailed);
    }

    #[test]
    fn test_reset_error_maps_revision_corruption_as_repo_corrupt() {
        let error = CliError::from(ResetError::RevisionCorrupt(
            "failed to resolve branch 'main': stored branch reference 'main' is corrupt: invalid hash"
                .into(),
        ));
        assert_eq!(error.stable_code(), StableErrorCode::RepoCorrupt);
    }

    #[test]
    fn test_merge_reset_failure_preserves_primary_error_category() {
        let merged = merge_reset_failure(
            ResetError::ObjectLoad {
                kind: "tree",
                object_id: "deadbeef".into(),
                detail: "corrupt object".into(),
            },
            Err(ResetError::WorktreeRestore(
                "failed to restore working tree".into(),
            )),
        );

        assert!(matches!(merged, ResetError::Rollback { .. }));
        let cli_error = CliError::from(merged);
        assert_eq!(cli_error.stable_code(), StableErrorCode::RepoCorrupt);
        assert!(cli_error.message().contains("rollback failed"));
        assert!(
            cli_error
                .message()
                .contains("failed to restore working tree")
        );
    }
}
