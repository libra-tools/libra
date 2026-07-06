//! Reset command covering soft/mixed/hard behaviors to move HEAD and align the index or working tree to a chosen commit.

use std::{
    collections::HashSet,
    fs, io,
    io::{BufRead, Read},
    path::{Path, PathBuf},
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
    command::load_object,
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
        util,
    },
};

const RESET_EXAMPLES: &str = "\
EXAMPLES:
    libra reset HEAD~1                    Move HEAD and reset index to the previous commit
    libra reset --soft HEAD~2             Move HEAD only, keep index and worktree
    libra reset --hard main               Reset HEAD, index, and worktree to branch 'main'
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
}

impl ResetMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Soft => "soft",
            Self::Mixed => "mixed",
            Self::Hard => "hard",
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
    if absolute.exists() {
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
    absolute.exists() && util::is_sub_path(&absolute, util::working_dir())
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
                let entry = IndexEntry::new_from_blob(
                    path_str.to_string(),
                    item.id,
                    blob.data.len() as u32,
                );
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
                        Head::update_with_conn(txn, new_head, None).await;
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
    let target_files = tree.get_plain_items();
    let target_files_set: HashSet<_> = target_files.iter().map(|(path, _)| path.clone()).collect();
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
            _ => {
                // Restore file
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

                let needs_write = match fs::read(&file_path) {
                    Ok(existing) => existing != blob.data,
                    Err(err) if err.kind() == io::ErrorKind::NotFound => true,
                    Err(err) => {
                        return Err(ResetError::WorktreeRead(format!(
                            "failed to read file {}: {}",
                            file_path.display(),
                            err
                        )));
                    }
                };

                if needs_write {
                    fs::write(&file_path, blob.data).map_err(|e| {
                        ResetError::WorktreeRestore(format!(
                            "failed to write file {}: {}",
                            file_path.display(),
                            e
                        ))
                    })?;
                    files_restored += 1;
                }
                apply_worktree_blob_mode(&file_path, item.mode)?;
            }
        }
    }
    Ok(files_restored)
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
    Ok(index.tracked_files().into_iter().collect())
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
        assert!(!args.soft && !args.hard);
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
