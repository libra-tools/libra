//! Implements status reporting with ignore policy support, computing staged/unstaged/untracked sets and printing concise summaries.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    io,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};

use clap::{Parser, ValueEnum};
use colored::Colorize;
use git_internal::{
    errors::GitError,
    hash::{ObjectHash, get_hash_kind},
    internal::{
        index::Index,
        object::{
            commit::Commit,
            tree::{Tree, TreeItemMode},
        },
    },
};
use serde::Serialize;

use super::{
    merge, rename_detect, stash, status_untracked,
    unmerged::{self, UnmergedEntry},
};
use crate::{
    command::calc_file_blob_hash,
    internal::{
        branch::{Branch, BranchStoreError},
        config::ConfigKv,
        head::Head,
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
// Args & enums
// ---------------------------------------------------------------------------

const STATUS_EXAMPLES: &str = "\
EXAMPLES:
    libra status                       Show working tree status
    libra status -s                    Short format output
    libra status --porcelain           Machine-readable output (v1)
    libra status --porcelain v2        Extended machine-readable output
    libra status -sb                   Include branch info in short output (-b = --branch)
    libra status --show-stash          Show stash count
    libra status --ignored             Include ignored files
    libra status -uno                  Hide untracked files (-u = --untracked-files; bare -u = all)
    libra status --renames             Detect renames (--no-renames disables)
    libra status --json                Structured JSON output for agents
    libra status --exit-code           Exit 1 if working tree is dirty
    libra status --quiet --exit-code   Silent dirty check for scripts";

/// Show the working tree status.
// EXAMPLES are wired via `#[command(after_help = STATUS_EXAMPLES)]` and render
// at the bottom of `libra status --help`. The meta-commentary that used to
// live here as a `///` line leaked into clap's `--help` body (see
// `tests/command/status_test.rs::test_status_help_does_not_leak_impl_meta`).
#[derive(Parser, Debug, Default)]
#[command(after_help = STATUS_EXAMPLES)]
pub struct StatusArgs {
    /// Output in a machine-readable format (default v1). Use v2 for extended format.
    #[clap(
        long = "porcelain",
        value_name = "VERSION",
        num_args = 0..=1,
        default_missing_value = "v1",
        conflicts_with = "short"
    )]
    pub porcelain: Option<PorcelainVersion>,

    /// Give the output in the short-format
    #[clap(short = 's', long = "short", conflicts_with = "porcelain")]
    pub short: bool,

    /// Give the output in the long-format. This is Libra's default, so the flag
    /// is accepted for Git parity and simply selects the default rendering;
    /// it conflicts with `--short`/`--porcelain`.
    #[clap(long = "long", conflicts_with_all = ["short", "porcelain"])]
    pub long_format: bool,

    /// Output with branch info (short or porcelain mode)
    #[clap(short = 'b', long = "branch")]
    pub branch: bool,

    /// Do not show branch info in the short format, overriding
    /// `status.branch=true` (and an earlier `--branch`; the last one wins).
    #[clap(long = "no-branch", overrides_with = "branch")]
    pub no_branch: bool,

    /// Show ahead/behind counts in branch info (default: true).
    /// Use --no-ahead-behind to suppress the counts.
    #[clap(long = "ahead-behind")]
    pub ahead_behind: bool,

    /// Suppress ahead/behind counts in branch info.
    #[clap(long = "no-ahead-behind", overrides_with = "ahead_behind")]
    pub no_ahead_behind: bool,

    /// Output with stash info (only in standard mode)
    #[clap(long = "show-stash")]
    pub show_stash: bool,

    /// Do not show the stash hint, overriding `status.showStash=true` (and an
    /// earlier `--show-stash`; the last one wins).
    #[clap(long = "no-show-stash", overrides_with = "show_stash")]
    pub no_show_stash: bool,

    /// Show ignored files
    #[clap(long = "ignored")]
    pub ignored: bool,

    /// Control untracked files display: `no`, `normal` (the default when both
    /// the flag and `status.showUntrackedFiles` are absent), or `all`. As in
    /// Git, the short `-u`/long `--untracked-files` with no value means `all`
    /// (e.g. `-u`, `-uno`, `--untracked-files=all`); when the flag is absent
    /// the `status.showUntrackedFiles` config default applies.
    #[clap(
        short = 'u',
        long = "untracked-files",
        value_name = "MODE",
        num_args = 0..=1,
        default_missing_value = "all"
    )]
    pub untracked_files: Option<UntrackedFiles>,

    /// Libra extension (lore.md 1.1): consume the dirty-set cache instead of
    /// walking the working tree. Requires a fresh cache (`status --scan`);
    /// a missing/stale cache degrades to the full reconcile with a hint.
    /// NOTE: unrelated to Git's `--cached` (= the index) — this reads Libra's
    /// `working_dirty` SQLite cache.
    #[clap(long = "cached", conflicts_with_all = ["check_dirty", "scan", "porcelain", "short", "ignored"])]
    pub cached: bool,

    /// Libra extension (lore.md 1.1): re-verify ONLY the cached dirty set
    /// (O(dirty paths)) — rows re-verified clean are pruned; nothing new is
    /// discovered. Degrades to the full reconcile when the cache is stale.
    #[clap(long = "check-dirty", conflicts_with_all = ["cached", "scan", "porcelain", "short", "ignored"])]
    pub check_dirty: bool,

    /// Libra extension (lore.md 1.1): run the normal full status AND rebuild
    /// the dirty-set cache atomically from it (the only authoritative writer).
    #[clap(long = "scan", conflicts_with_all = ["cached", "check_dirty", "porcelain", "short", "ignored"])]
    pub scan: bool,

    /// Print status entries with columns aligned (human output only).
    #[clap(long = "column", overrides_with = "no_column")]
    pub column: bool,

    /// Do not print status entries in columns (equivalent to `--column=never`),
    /// countermanding an earlier `--column` (last one on the command line wins),
    /// matching `git status --no-column`. Status is not columnar by default, so
    /// on its own this is a no-op.
    #[clap(long = "no-column", overrides_with = "column")]
    pub no_column: bool,

    /// Terminate each status entry with a NUL byte instead of a newline.
    /// This is intended for machine-readable short/porcelain output.
    #[clap(short = 'z')]
    pub null_terminated: bool,

    /// Detect renames in staged/unstaged changes.
    /// The optional value is the similarity threshold percentage (default 50).
    #[clap(
        long = "find-renames",
        value_name = "PERCENT",
        num_args = 0..=1,
        default_missing_value = "50"
    )]
    pub find_renames: Option<u8>,

    /// Enable rename detection at the default (or `--find-renames`) threshold
    /// (Git's `--renames`).
    #[clap(long = "renames", overrides_with = "no_renames")]
    pub renames: bool,

    /// Disable rename detection, overriding `--renames`/`--find-renames`
    /// (Git's `--no-renames`).
    #[clap(long = "no-renames", overrides_with = "renames")]
    pub no_renames: bool,

    /// Exit with code 1 if the working tree has changes.
    /// Can be combined with --quiet for silent dirty checking.
    #[clap(long = "exit-code")]
    pub exit_code: bool,

    /// Limit status output to files matching the given pathspec(s).
    #[clap(value_name = "pathspec")]
    pub pathspec: Vec<String>,

    /// Resolved `status.relativePaths` (config-only, like Git): `true` (the
    /// default) renders human long/short paths relative to the current
    /// directory; `false` keeps repository-root-relative paths. Populated by
    /// [`apply_status_config_defaults`], never by the CLI.
    #[clap(skip = true)]
    pub relative_paths: bool,

    /// Resolved rename-detection default from `status.renames` (falling back
    /// to `diff.renames`), config-only. `None` = unset (feature default 50%
    /// applies); `Some(false)` = disabled; `Some(true)` = enabled at 50%.
    /// CLI flags (`--no-renames`/`--find-renames`/`--renames`) always win.
    /// Populated by [`apply_status_config_defaults`], never by the CLI.
    #[clap(skip)]
    pub renames_config: Option<bool>,
}

impl StatusArgs {
    /// Whether ahead/behind counts should be shown in branch info.
    fn show_ahead_behind(&self) -> bool {
        !self.no_ahead_behind
    }
}

/// Module-private resolved status config that deliberately does NOT live on
/// the public `StatusArgs` (adding fields there breaks exhaustive struct
/// literals in downstream code, even with the `Default` derive).
#[derive(Clone, Copy, Default)]
struct StatusConfigExtras {
    /// `status.renameUntracked` (§B.3.1): untracked worktree paths may be
    /// unstaged rename destinations only when true (default false = Git
    /// parity: a tracked→untracked move renders as `D` + `??`).
    rename_untracked: bool,
}

/// Resolve the Git-compatible `status.*` config defaults (plan-20260708
/// P1-05d): `status.showUntrackedFiles`, `status.short`, `status.branch`,
/// `status.showStash`, `status.relativePaths`, plus the rename keys
/// (`status.renames`→`diff.renames` and the `status.renameUntracked`
/// extension, §B.3.1), each read through the strict
/// local → global → system cascade. Every key is validated
/// UP FRONT — an invalid value is a usage error and an unreadable
/// local/global scope an IO error, both before any status output — and then
/// applied only where Git applies them: CLI flags always win;
/// `status.short` yields to an explicit `--long`/`--porcelain`;
/// `status.branch` affects only the short format (porcelain stays
/// config-immune, matching Git's stable-script contract).
async fn apply_status_config_defaults(args: &mut StatusArgs) -> CliResult<StatusConfigExtras> {
    use crate::internal::config::{
        LocalIdentityTarget, parse_git_config_bool, read_cascaded_config_value_strict,
    };

    async fn read_value(key: &str) -> CliResult<Option<String>> {
        read_cascaded_config_value_strict(LocalIdentityTarget::CurrentRepo, key)
            .await
            .map_err(|error| {
                CliError::fatal(format!("failed to read config '{key}': {error:#}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })
    }
    fn invalid(key: &str, value: &str, expected: &str) -> CliError {
        CliError::command_usage(format!(
            "bad config value '{value}' for '{key}' (expected {expected})"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint(format!(
            "fix the offending value with 'libra config {key} <value>'"
        ))
    }
    async fn read_bool(key: &str) -> CliResult<Option<bool>> {
        match read_value(key).await? {
            Some(value) => match parse_git_config_bool(&value) {
                Some(enabled) => Ok(Some(enabled)),
                None => Err(invalid(key, &value, "a Git boolean")),
            },
            None => Ok(None),
        }
    }

    // Validate every key up front so a bad value fails closed even when the
    // requested format would not consult it.
    let untracked = match read_value("status.showUntrackedFiles").await? {
        Some(value) => Some(match value.trim().to_ascii_lowercase().as_str() {
            "no" => UntrackedFiles::No,
            "normal" => UntrackedFiles::Normal,
            "all" => UntrackedFiles::All,
            _ => {
                return Err(invalid(
                    "status.showUntrackedFiles",
                    &value,
                    "no, normal, or all",
                ));
            }
        }),
        None => None,
    };
    let short = read_bool("status.short").await?;
    let branch = read_bool("status.branch").await?;
    let show_stash = read_bool("status.showStash").await?;
    let relative_paths = read_bool("status.relativePaths").await?;

    // Rename detection default (§B.5): `status.renames`, falling back to
    // `diff.renames`. Accepts a Git boolean or `copy`/`copies`; `copy` is
    // fail-closed in R0 (copy detection is not supported yet) rather than
    // silently degrading to rename detection.
    async fn read_renames(key: &str) -> CliResult<Option<bool>> {
        match read_value(key).await? {
            None => Ok(None),
            Some(value) => {
                let lower = value.trim().to_ascii_lowercase();
                if lower == "copy" || lower == "copies" {
                    return Err(CliError::command_usage(format!(
                        "copy detection is not supported for '{key}'; use true or false"
                    ))
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
                    .with_hint("set the value to true or false"));
                }
                match parse_git_config_bool(&value) {
                    Some(enabled) => Ok(Some(enabled)),
                    None => Err(invalid(key, &value, "a Git boolean or copy/copies")),
                }
            }
        }
    }
    let renames_config = match read_renames("status.renames").await? {
        Some(value) => Some(value),
        None => read_renames("diff.renames").await?,
    };

    // Libra extension (§B.3.1): untracked paths become unstaged rename
    // destinations only under `status.renameUntracked=true`. Strict Git
    // boolean; invalid values fail closed before any output.
    let rename_untracked = read_bool("status.renameUntracked").await?.unwrap_or(false);

    if args.untracked_files.is_none() {
        args.untracked_files = untracked;
    }
    if !args.short && !args.long_format && args.porcelain.is_none() && short == Some(true) {
        args.short = true;
    }
    // Git scopes the status.branch default to the short format; porcelain
    // headers still require an explicit `-b`/`--branch`.
    if args.short && !args.branch && !args.no_branch && branch == Some(true) {
        args.branch = true;
    }
    if !args.show_stash && !args.no_show_stash && show_stash == Some(true) {
        args.show_stash = true;
    }
    args.relative_paths = relative_paths.unwrap_or(true);
    args.renames_config = renames_config;
    Ok(StatusConfigExtras { rename_untracked })
}

/// Resolve and validate all `status.*` defaults without collecting repository
/// state or producing output. Embedded consumers use this before side effects,
/// then pass the returned arguments to [`execute_to_resolved`].
pub(crate) async fn resolve_config_defaults(mut args: StatusArgs) -> CliResult<ResolvedStatusArgs> {
    let extras = apply_status_config_defaults(&mut args).await?;
    Ok(ResolvedStatusArgs { args, extras })
}

/// Resolved status arguments bundled with the module-private extras,
/// produced by [`resolve_config_defaults`] exactly once and consumed by
/// [`execute_to_resolved`]. Opaque outside this module, so the single-read
/// contract cannot be bypassed and the public `StatusArgs` stays unchanged.
pub(crate) struct ResolvedStatusArgs {
    args: StatusArgs,
    extras: StatusConfigExtras,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum PorcelainVersion {
    #[clap(name = "v1")]
    V1,
    #[clap(name = "v2")]
    V2,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum, Default)]
pub enum UntrackedFiles {
    /// Show untracked files (default): only list untracked directories, not their contents.
    #[default]
    Normal,
    /// Show all untracked files, recursively listing files within untracked directories.
    All,
    /// Do not show untracked files
    No,
}

// ---------------------------------------------------------------------------
// Changes
// ---------------------------------------------------------------------------

/// path: to workdir
#[derive(Debug, Default, Clone)]
pub struct Changes {
    pub new: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
    /// Detected renames: (source_path, target_path) pairs.
    pub renamed: Vec<(PathBuf, PathBuf)>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.new.is_empty()
            && self.modified.is_empty()
            && self.deleted.is_empty()
            && self.renamed.is_empty()
    }

    /// to relative path(to cur_dir)
    pub fn to_relative(&self) -> Changes {
        let mut change = self.clone();
        [&mut change.new, &mut change.modified, &mut change.deleted]
            .into_iter()
            .for_each(|paths| {
                *paths = paths.iter().map(util::workdir_to_current).collect();
            });
        change.renamed = change
            .renamed
            .into_iter()
            .map(|(old, new)| {
                (
                    util::workdir_to_current(&old),
                    util::workdir_to_current(&new),
                )
            })
            .collect();
        change
    }
    pub fn polymerization(&self) -> Vec<PathBuf> {
        let mut poly = self.new.clone();
        poly.extend(self.modified.clone());
        poly.extend(self.deleted.clone());
        poly.extend(self.renamed.iter().map(|(_, new)| new.clone()));
        poly
    }

    pub fn extend(&mut self, other: Changes) {
        self.new.extend(other.new);
        self.modified.extend(other.modified);
        self.deleted.extend(other.deleted);
        self.renamed.extend(other.renamed);
    }
}

// ---------------------------------------------------------------------------
// StatusError + CliError mapping
// ---------------------------------------------------------------------------

#[derive(thiserror::Error, Debug)]
pub enum StatusError {
    #[error("failed to open index '{path}': {source}")]
    IndexLoad { path: PathBuf, source: GitError },
    #[error("path '{path}' is not valid UTF-8")]
    InvalidPathEncoding { path: PathBuf },
    #[error("failed to hash '{path}': {source}")]
    FileHash { path: PathBuf, source: io::Error },
    #[error("cannot read tracked path '{path}': {source}")]
    WorktreeRead { path: PathBuf, source: io::Error },
    #[error("failed to list files in '{path}': {source}")]
    ListWorkdirFiles { path: PathBuf, source: io::Error },
    #[error("failed to determine working directory: {source}")]
    Workdir { source: io::Error },
    #[error("{source}")]
    ConfigRead { source: anyhow::Error },
}

impl From<StatusError> for CliError {
    fn from(error: StatusError) -> Self {
        let msg = format!("failed to determine working tree status: {error}");
        match &error {
            StatusError::IndexLoad { .. } => CliError::fatal(msg)
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint("the index file may be corrupted"),
            StatusError::InvalidPathEncoding { .. } => CliError::fatal(msg)
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("path contains non-UTF-8 characters"),
            StatusError::FileHash { .. } => {
                CliError::fatal(msg).with_stable_code(StableErrorCode::IoReadFailed)
            }
            StatusError::WorktreeRead { .. } => CliError::fatal(msg)
                .with_stable_code(StableErrorCode::IoReadFailed)
                .with_hint(
                    "a tracked file could not be read (e.g. permission denied); \
                     status fails closed rather than reporting it as deleted",
                ),
            StatusError::ListWorkdirFiles { .. } => {
                CliError::fatal(msg).with_stable_code(StableErrorCode::IoReadFailed)
            }
            StatusError::Workdir { .. } => {
                CliError::fatal(msg).with_stable_code(StableErrorCode::RepoNotFound)
            }
            StatusError::ConfigRead { .. } => {
                CliError::fatal(msg).with_stable_code(StableErrorCode::IoReadFailed)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// UpstreamInfo
// ---------------------------------------------------------------------------

/// Upstream tracking information for the current branch.
#[derive(Debug, Clone, Serialize)]
pub struct UpstreamInfo {
    /// Tracking ref display name, e.g. "origin/main"
    pub remote_ref: String,
    /// Commits ahead of upstream (None when gone)
    pub ahead: Option<usize>,
    /// Commits behind upstream (None when gone)
    pub behind: Option<usize>,
    /// True when upstream is configured but tracking ref no longer exists
    pub gone: bool,
}

/// In-progress merge metadata surfaced by `status` for recovery guidance.
#[derive(Debug, Clone, Serialize)]
pub struct MergeStatusInfo {
    pub target_ref: String,
    pub conflicted_paths: Vec<String>,
    pub unresolved_count: usize,
}

// ---------------------------------------------------------------------------
// StatusData — shared data layer
// ---------------------------------------------------------------------------

/// Pre-computed status data shared across all renderers (human/JSON/short/porcelain).
#[derive(Clone)]
struct StatusData {
    head: Head,
    head_oid: Option<ObjectHash>,
    has_commits: bool,
    staged: Changes,
    unstaged: Changes,
    unmerged: Vec<UnmergedEntry>,
    ignored_files: Vec<PathBuf>,
    stash_count: Option<usize>,
    upstream: Option<UpstreamInfo>,
    merge_state: Option<MergeStatusInfo>,
    /// A non-merge sequence in progress (cherry-pick/revert/rebase), surfaced
    /// as a one-line human advisory (lore.md 2.6). Merge has its own richer
    /// rendering; porcelain/JSON are unchanged.
    sequence_notice: Option<String>,
    /// lore.md 2.2: a read-only sparse view is ACTIVELY filtering (enabled AND
    /// non-empty AND compiled — matches SparseView::is_active). status itself
    /// is NEVER filtered (it must stay honest about what commit will record);
    /// this is only an advisory that ls-files/diff are scoped. An
    /// enabled-but-empty view is a no-op, so no advisory.
    sparse_view_active: bool,
    porcelain_v2: Option<std::sync::Arc<PorcelainV2Data>>,
    /// Score/exactness per staged rename pair (display-base keys, §B.6.4/5).
    staged_rename_details: RenameDetails,
    /// Score/exactness per unstaged rename pair (only populated under
    /// `status.renameUntracked=true`).
    unstaged_rename_details: RenameDetails,
}

/// Human advisory for a non-merge sequence in progress (read-only detection).
async fn sequence_notice() -> CliResult<Option<String>> {
    use crate::internal::sequencer::{self, ActiveSequenceKind, SequenceKind};
    let active = sequencer::detect_active_operation()
        .await
        .map_err(|error| {
            CliError::fatal(format!(
                "failed to inspect in-progress operation state: {error}"
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint("repair the repository sequencer state, then retry 'libra status'")
        })?;
    Ok(match active {
        Some(ActiveSequenceKind::Am) => Some(
            "You are in the middle of an am operation; use 'libra am --continue', '--skip', or '--abort'."
                .to_string(),
        ),
        Some(ActiveSequenceKind::Known(SequenceKind::CherryPick)) => Some(
            "cherry-pick in progress; run 'libra cherry-pick --continue' or '--abort'".to_string(),
        ),
        Some(ActiveSequenceKind::Known(SequenceKind::Revert)) => {
            Some("revert in progress; run 'libra revert --continue' or '--abort'".to_string())
        }
        Some(ActiveSequenceKind::Known(SequenceKind::Rebase)) => {
            Some("rebase in progress; run 'libra rebase --continue' or '--abort'".to_string())
        }
        // Merge has its own dedicated rendering below.
        Some(ActiveSequenceKind::Known(SequenceKind::Merge)) | None => None,
    })
}

impl StatusData {
    fn is_dirty(&self) -> bool {
        !self.staged.is_empty()
            || !self.unstaged.is_empty()
            || self.merge_state.is_some()
            || !self.unmerged.is_empty()
    }
}

/// Collect all status data in one pass, eliminating duplicate computation
/// between human/JSON/short/porcelain renderers.
async fn collect_status_data(
    args: &StatusArgs,
    extras: StatusConfigExtras,
) -> CliResult<StatusData> {
    // lore.md 2.4: layer-overlay paths are excluded from status like ignored
    // files (a no-op with no layers).
    crate::internal::layer::refresh_exclusion_snapshot().await;
    if is_bare_repository().await {
        return Err(CliError::fatal("this operation must be run in a work tree")
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint("this command requires a working tree; bare repositories do not have one"));
    }
    let ignore_case = effective_ignore_case_for_status().await?;

    let head = Head::current_result()
        .await
        .map_err(|error| status_branch_store_error("resolve HEAD", error))?;
    let head_oid = Head::current_commit_result()
        .await
        .map_err(|error| status_branch_store_error("resolve HEAD commit", error))?;
    let has_commits = head_oid.is_some();

    let mut staged = changes_to_be_committed_safe()
        .await
        .map(|c| c.to_relative())
        .map_err(CliError::from)?;
    let worktree = status_untracked::collect_status_worktree_changes(
        args.untracked_files.unwrap_or(UntrackedFiles::Normal),
        args.ignored,
        ignore_case,
    )
    .map_err(CliError::from)?;
    let mut unstaged = status_untracked::changes_to_current_directory(worktree.unstaged);
    let unmerged = unmerged::collect(&worktree.index)
        .into_iter()
        .map(|entry| {
            let current_path = util::workdir_to_current(&entry.path);
            entry.with_path(current_path)
        })
        .collect::<Vec<_>>();
    let unmerged_paths = unmerged
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<HashSet<_>>();
    unstaged.new.retain(|path| !unmerged_paths.contains(path));
    let ignored_files = worktree
        .ignored_files
        .into_iter()
        .map(util::workdir_to_current)
        .collect();
    let mut maybe_index = Some(worktree.index);

    // Resolve rename detection (§B.5). Precedence: CLI flags always win —
    // `--no-renames` disables, `--find-renames[=N]`/`--renames` enable at the
    // given (or default 50%) threshold. Otherwise the resolved
    // `status.renames`/`diff.renames` config applies (`false` disables). When
    // nothing is set, rename detection is ON at 50%, matching Git.
    // `--cached`/`--check-dirty` (Libra dirty-cache extensions) never run it.
    let rename_percent: Option<u8> = if args.no_renames || args.cached || args.check_dirty {
        None
    } else if let Some(explicit) = args.find_renames {
        Some(explicit)
    } else if args.renames {
        Some(50)
    } else {
        match args.renames_config {
            Some(false) => None,
            Some(true) | None => Some(50),
        }
    };

    // Apply rename detection before collapsing untracked dirs / porcelain
    // metadata. Staged snapshot: old = HEAD tree, new = index stage-0.
    // Unstaged snapshot: old = index stage-0, new = worktree — but untracked
    // paths only become destinations under the `status.renameUntracked`
    // extension (§B.3.1; Git default: a tracked→untracked move is `D` + `??`).
    let mut staged_rename_details: RenameDetails = HashMap::new();
    let mut unstaged_rename_details: RenameDetails = HashMap::new();
    if let Some(percent) = rename_percent
        && percent > 0
    {
        let head_blobs = head_oid
            .as_ref()
            .map(load_head_tree_blobs)
            .unwrap_or_default();
        let index_blobs = maybe_index
            .as_ref()
            .map(load_index_stage0_blobs)
            .unwrap_or_default();
        // Git 0..=60000 similarity scale; percent × 600 (100% → exact-only).
        let config = rename_detect::RenameDetectConfig {
            threshold: (percent as u32).min(100) * 600,
            rename_limit: 1000,
            comparison_budget: Some(rename_detect::STATUS_MAX_SIMILARITY_COMPARISONS),
        };
        detect_renames_in_changes(
            &mut staged,
            &config,
            RenameBlobSide::Known(&head_blobs),
            RenameBlobSide::Known(&index_blobs),
            &mut staged_rename_details,
        );
        // §B.3.1 Git default: unstaged "new" entries are untracked paths,
        // which may only be consumed as rename destinations under the
        // `status.renameUntracked` extension. Skipping detection keeps a
        // tracked→untracked move rendered as `D` + `??`.
        if extras.rename_untracked {
            detect_renames_in_changes(
                &mut unstaged,
                &config,
                RenameBlobSide::Known(&index_blobs),
                RenameBlobSide::Worktree,
                &mut unstaged_rename_details,
            );
        }
    }

    let stash_count = if args.show_stash {
        Some(stash::get_stash_num().map_err(|detail| {
            CliError::fatal(format!("failed to read stash state for status: {detail}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
                .with_hint("repair or remove the corrupt stash log, then retry")
        })?)
    } else {
        None
    };

    // Resolve upstream tracking info
    let upstream = resolve_upstream_info(&head, head_oid.as_ref()).await?;
    let merge_state = match merge::MergeState::load_optional_sync().map_err(|detail| {
        CliError::fatal(format!("failed to inspect merge state: {detail}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })? {
        Some(state) => {
            if maybe_index.is_none() {
                maybe_index = Some(load_status_index()?);
            }
            let index = maybe_index
                .as_ref()
                .ok_or_else(|| CliError::internal("status index should be loaded"))?;
            let conflicted_paths =
                merge::unresolved_conflicted_paths(index, &state.conflicted_paths);
            Some(MergeStatusInfo {
                target_ref: state.target_ref,
                unresolved_count: conflicted_paths.len(),
                conflicted_paths,
            })
        }
        None => None,
    };
    let porcelain_v2 = if matches!(args.porcelain, Some(PorcelainVersion::V2)) {
        let index = maybe_index
            .take()
            .ok_or_else(|| CliError::internal("porcelain v2 metadata should be loaded"))?;
        Some(std::sync::Arc::new(build_porcelain_v2_data(
            index,
            head_oid.as_ref(),
        )))
    } else {
        None
    };

    let mut data = StatusData {
        head,
        head_oid,
        has_commits,
        staged,
        unstaged,
        unmerged,
        ignored_files,
        stash_count,
        upstream,
        merge_state,
        sequence_notice: sequence_notice().await?,
        sparse_view_active: crate::internal::sparse::SparseView::load()
            .await
            .is_active(),
        porcelain_v2,
        staged_rename_details,
        unstaged_rename_details,
    };
    filter_status_data_by_pathspec(&mut data, args)?;
    Ok(data)
}

fn filter_status_data_by_pathspec(data: &mut StatusData, args: &StatusArgs) -> CliResult<()> {
    if args.pathspec.is_empty() {
        return Ok(());
    }
    let pathspecs =
        PathspecSet::from_workdir(&args.pathspec, &util::cur_dir(), &util::working_dir())
            .map_err(pathspec_error_to_cli)?;

    filter_changes_by_pathspec(&mut data.staged, &pathspecs);
    filter_changes_by_pathspec(&mut data.unstaged, &pathspecs);
    data.unmerged
        .retain(|entry| current_relative_matches(&entry.path, &pathspecs));
    data.ignored_files
        .retain(|path| current_relative_matches(path, &pathspecs));
    if let Some(merge_state) = data.merge_state.as_mut() {
        merge_state
            .conflicted_paths
            .retain(|path| pathspecs.matches_path(Path::new(path)));
    }

    Ok(())
}

fn filter_changes_by_pathspec(changes: &mut Changes, pathspecs: &PathspecSet) {
    changes
        .new
        .retain(|path| current_relative_matches(path, pathspecs));
    changes
        .modified
        .retain(|path| current_relative_matches(path, pathspecs));
    changes
        .deleted
        .retain(|path| current_relative_matches(path, pathspecs));
    // Per-end pathspec semantics (§B.3): a rename pair survives only when
    // BOTH endpoints match. An old-only match demotes to a deletion and a
    // new-only match to an addition, so an out-of-scope endpoint can never
    // leak into the output through a rename record.
    let mut kept: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (old, new) in changes.renamed.drain(..) {
        let old_in = current_relative_matches(&old, pathspecs);
        let new_in = current_relative_matches(&new, pathspecs);
        match (old_in, new_in) {
            (true, true) => kept.push((old, new)),
            (true, false) => changes.deleted.push(old),
            (false, true) => changes.new.push(new),
            (false, false) => {}
        }
    }
    changes.deleted.sort();
    changes.new.sort();
    changes.renamed = kept;
}

fn current_relative_matches(path: &Path, pathspecs: &PathspecSet) -> bool {
    pathspecs.matches_path(util::to_workdir_path(path))
}

fn pathspec_error_to_cli(error: PathspecError) -> CliError {
    match error {
        PathspecError::OutsideRepository { .. } => CliError::fatal(error.to_string())
            .with_stable_code(StableErrorCode::CliInvalidTarget)
            .with_hint("all pathspecs must stay within the repository working tree"),
        PathspecError::UnsupportedMagic { .. } | PathspecError::InvalidPattern { .. } => {
            CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("use supported magic: top, exclude, icase, literal, glob")
        }
    }
}

/// Where one side of a rename snapshot draws its blob identities from.
enum RenameBlobSide<'a> {
    /// HEAD tree or index stage-0: repo-relative path → (oid, mode), a
    /// content-addressed fact (`KnownObjectId`, §B.4.1).
    Known(&'a HashMap<PathBuf, (ObjectHash, u32)>),
    /// The worktree: OID is streamed from the file during this call
    /// (`ComputedWorktreeThisCall`).
    Worktree,
}

/// Content provider for inexact scoring: HEAD/index blobs are read from the
/// object store by OID (de-duplicated, budgeted), worktree files are read
/// under the separate worktree budget (§B.7). The engine caches spanhashes
/// per path, so each path is requested at most once per side.
struct StatusContentSource {
    old_is_worktree: bool,
    new_is_worktree: bool,
    objects: rename_detect::ObjectReadBudget,
    worktree: rename_detect::WorktreeReadBudget,
}

impl StatusContentSource {
    fn read(
        &mut self,
        path: &Path,
        blob: &rename_detect::BlobRef,
        from_worktree: bool,
    ) -> rename_detect::ContentOutcome {
        use rename_detect::{BlobEvidence, ContentOutcome, SkipReason};
        match blob.evidence {
            BlobEvidence::KnownObjectId { oid } if !from_worktree => self.objects.read_blob(&oid),
            _ if from_worktree => {
                let abs = util::workdir_to_absolute(path);
                self.worktree.read_worktree_blob(&abs)
            }
            // A worktree-computed OID on the object side, or an Unknown blob:
            // no trustworthy object to read.
            _ => ContentOutcome::Skipped(SkipReason::ObjectUnavailable),
        }
    }
}

impl rename_detect::RenameContentSource for StatusContentSource {
    fn old_content(
        &mut self,
        path: &Path,
        blob: &rename_detect::BlobRef,
    ) -> rename_detect::ContentOutcome {
        let from_worktree = self.old_is_worktree;
        self.read(path, blob, from_worktree)
    }

    fn new_content(
        &mut self,
        path: &Path,
        blob: &rename_detect::BlobRef,
    ) -> rename_detect::ContentOutcome {
        let from_worktree = self.new_is_worktree;
        self.read(path, blob, from_worktree)
    }
}

/// Build one side of a [`rename_detect::RenameSnapshot`] from a change list.
///
/// `paths` are in the change list's own base (repo- or cwd-relative); each is
/// mapped to a repo-relative key via [`util::to_workdir_path`] so the HEAD/
/// index lookups and worktree reads are correct from any working directory
/// (fixing the historical subdirectory bug). The returned map is keyed by the
/// repo-relative path.
fn build_rename_side(
    paths: &[PathBuf],
    side: &RenameBlobSide<'_>,
) -> HashMap<PathBuf, rename_detect::BlobRef> {
    use rename_detect::{BlobEvidence, BlobKind, BlobRef};
    let mut map = HashMap::new();
    for path in paths {
        let repo_key = util::to_workdir_path(path);
        let blob = match side {
            RenameBlobSide::Known(known) => {
                let Some((oid, mode)) = known.get(&repo_key).copied() else {
                    continue;
                };
                BlobRef {
                    kind: BlobKind::from_mode(mode),
                    mode,
                    size: None,
                    evidence: BlobEvidence::KnownObjectId { oid },
                }
            }
            RenameBlobSide::Worktree => {
                let abs = util::workdir_to_absolute(&repo_key);
                let (kind, mode) = match std::fs::symlink_metadata(&abs) {
                    Ok(meta) if meta.file_type().is_symlink() => (BlobKind::Symlink, 0o120000),
                    Ok(_) => (BlobKind::Regular, 0o100644),
                    Err(_) => continue,
                };
                let Ok(oid) = calc_file_blob_hash(&abs) else {
                    continue;
                };
                BlobRef {
                    kind,
                    mode,
                    size: None,
                    evidence: BlobEvidence::ComputedWorktreeThisCall { oid },
                }
            }
        };
        map.insert(repo_key, blob);
    }
    map
}

/// Per-pair rename detail: percentage score and exactness (§B.6.4/§B.6.5),
/// keyed by the display-base `(old, new)` pair recorded in `Changes.renamed`.
type RenameDetails = HashMap<(PathBuf, PathBuf), (u32, bool)>;

/// Detect renames between the `deleted` (old) and `new` sides of `changes`
/// using the diffcore engine (exact by OID → unique basename → bounded
/// exhaustive inexact, §B.4.2). Matched pairs are recorded in
/// `changes.renamed` and pruned from `deleted`/`new`; each pair's score and
/// exactness are added to `details`. Paths keep the change list's original
/// base for display; detection runs on repo-relative keys.
fn detect_renames_in_changes(
    changes: &mut Changes,
    config: &rename_detect::RenameDetectConfig,
    old_side: RenameBlobSide<'_>,
    new_side: RenameBlobSide<'_>,
    details: &mut RenameDetails,
) {
    if changes.deleted.is_empty() || changes.new.is_empty() {
        return;
    }
    let snapshot = rename_detect::RenameSnapshot {
        old_map: build_rename_side(&changes.deleted, &old_side),
        new_map: build_rename_side(&changes.new, &new_side),
    };
    let mut source = StatusContentSource {
        old_is_worktree: matches!(old_side, RenameBlobSide::Worktree),
        new_is_worktree: matches!(new_side, RenameBlobSide::Worktree),
        objects: rename_detect::ObjectReadBudget::with_defaults(),
        worktree: rename_detect::WorktreeReadBudget::with_defaults(),
    };
    let outcome = rename_detect::match_pairs(&snapshot, config, &mut source);
    if outcome.matches.is_empty() {
        return;
    }

    // Map repo-relative matches back to the change list's display base and
    // prune consumed endpoints.
    let mut consumed_old: HashSet<PathBuf> = HashSet::new();
    let mut consumed_new: HashSet<PathBuf> = HashSet::new();
    let mut renamed: Vec<(PathBuf, PathBuf)> = Vec::new();
    for m in &outcome.matches {
        let old_display = util::workdir_to_current(&m.old);
        let new_display = util::workdir_to_current(&m.new);
        consumed_old.insert(old_display.clone());
        consumed_new.insert(new_display.clone());
        details.insert(
            (old_display.clone(), new_display.clone()),
            (m.score_percent(), m.exact),
        );
        renamed.push((old_display, new_display));
    }
    changes.deleted.retain(|p| !consumed_old.contains(p));
    changes.new.retain(|p| !consumed_new.contains(p));
    changes.deleted.sort();
    changes.new.sort();
    renamed.sort_by(|a, b| a.1.cmp(&b.1));
    changes.renamed.extend(renamed);
}

/// Numeric Unix mode for a `TreeItemMode` (`100644`/`100755`/`120000`/
/// `160000`), matching Git's stored blob modes.
fn tree_item_mode_to_unix(mode: TreeItemMode) -> u32 {
    match mode {
        TreeItemMode::Blob => 0o100644,
        TreeItemMode::BlobExecutable => 0o100755,
        TreeItemMode::Link => 0o120000,
        TreeItemMode::Commit => 0o160000,
        TreeItemMode::Tree => 0o040000,
    }
}

/// HEAD tree blobs keyed by repo-relative path → (oid, mode) (§B.4.1 old side
/// of the staged snapshot).
fn load_head_tree_blobs(head_oid: &ObjectHash) -> HashMap<PathBuf, (ObjectHash, u32)> {
    let commit = Commit::load(head_oid);
    let tree = Tree::load(&commit.tree_id);
    tree.get_plain_items_with_mode()
        .into_iter()
        .map(|(path, hash, mode)| (path, (hash, tree_item_mode_to_unix(mode))))
        .collect()
}

/// Index stage-0 blobs keyed by repo-relative path → (oid, mode) (index side
/// of both snapshots).
fn load_index_stage0_blobs(index: &Index) -> HashMap<PathBuf, (ObjectHash, u32)> {
    index
        .tracked_entries(0)
        .into_iter()
        .map(|entry| (PathBuf::from(&entry.name), (entry.hash, entry.mode)))
        .collect()
}

pub(crate) fn load_status_index() -> CliResult<Index> {
    let index_path =
        path::try_index().map_err(|source| CliError::from(StatusError::Workdir { source }))?;
    Index::load(&index_path).map_err(|source| {
        CliError::from(StatusError::IndexLoad {
            path: index_path,
            source,
        })
    })
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Collect repository status and render it inside the same `{ok, command,
/// data}` envelope that `libra status --json` prints, so `/api/repo/status`
/// stays byte-compatible with the CLI output.
///
/// Internally re-uses [`collect_status_data`] + [`build_status_json`] with a
/// default [`StatusArgs`] (untracked files in normal mode, no porcelain v2,
/// no ignored files, no stash count).
///
/// Status collection currently resolves storage from the process working
/// directory; the embedded web server expects to be launched from (or with
/// `--cwd`/`--repo` already chdir'd to) the repository root. Callers that
/// need to scope to a specific path should pass it via `working_dir`.
pub async fn collect_status_json_envelope_for_api(
    working_dir: &std::path::Path,
) -> CliResult<serde_json::Value> {
    use std::path::PathBuf;

    let mut args = StatusArgs::default();
    let canon_working =
        std::fs::canonicalize(working_dir).unwrap_or_else(|_| PathBuf::from(working_dir));
    let canon_cwd = std::env::current_dir()
        .ok()
        .and_then(|cwd| std::fs::canonicalize(&cwd).ok());
    if canon_cwd.as_deref() != Some(canon_working.as_path()) {
        return Err(CliError::fatal(format!(
            "/api/repo/status currently requires the libra process to run inside its repository root. Expected '{}', found '{}'. Re-launch `libra code` from the repo or open an issue if you need cross-directory status.",
            canon_working.display(),
            canon_cwd
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<unavailable>".to_string()),
        )));
    }

    // Byte-parity with `libra status --json`: the API honors the same
    // resolved `status.*` defaults (and the same fail-closed validation) as
    // the CLI entry points.
    let extras = apply_status_config_defaults(&mut args).await?;
    let args = args;
    let data = collect_status_data(&args, extras).await?;
    let inner = build_status_json(&data, &args);
    Ok(serde_json::json!({
        "ok": true,
        "command": "status",
        "data": inner,
    }))
}

pub async fn execute(args: StatusArgs) {
    if let Err(err) = execute_to(args, &mut std::io::stdout()).await {
        err.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting. JSON mode propagates status-computation failures as
/// structured CLI errors; text mode uses the same structured error contract.
pub async fn execute_safe(mut args: StatusArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    // Fail closed on invalid `status.*` config before any mode runs or any
    // output is produced; CLI flags keep precedence inside the resolver.
    let extras = apply_status_config_defaults(&mut args).await?;
    let args = args;

    // Dirty-set cache modes (lore.md 1.1). NOTE: only this CLI entry routes
    // them — the legacy `execute_to` writer entry ignores the flags (its
    // callers never set them).
    //
    // Part C W0 (§C.11 line 1507a): `working_dirty`/`working_dirty_meta` are
    // repository-global (id=1 meta), so these cache-SEMANTIC modes fail closed
    // in a linked worktree until W1 scopes the DirtyCache call chain — they
    // would otherwise read or prune the main worktree's dirty state. Plain
    // `status` (below) is unaffected: it never consults the dirty cache, so it
    // already computes a fresh, correct result in any worktree.
    if args.scan || args.cached || args.check_dirty {
        crate::command::ensure_main_worktree_because(
            "status --scan/--cached/--check-dirty",
            "the dirty cache is not yet worktree-scoped",
        )?;
    }
    if args.scan {
        return run_status_scan(&args, extras, output).await;
    }
    if args.cached || args.check_dirty {
        return run_status_cache_mode(&args, extras, output).await;
    }

    let data = collect_status_data(&args, extras).await?;

    if output.is_json() {
        let json_data = build_status_json(&data, &args);
        emit_json_data("status", &json_data, output)?;
    } else if !output.quiet {
        let mut stdout = std::io::stdout();
        render_status_to_writer(&data, &args, output, &mut stdout).await?;
    }

    // --exit-code: dirty → exit 1 (silent; do not emit an error line)
    if args.exit_code && data.is_dirty() {
        return Err(CliError::silent_exit(1));
    }

    Ok(())
}

// ─── Dirty-set cache modes (lore.md §1.1) ───────────────────────────────────

/// The raw (repo-relative) staged + unstaged sets for dirty-cache snapshots.
/// This remains a full reconcile so `status --scan` can discover every dirty
/// path before replacing the cache.
async fn compute_raw_sets() -> CliResult<(Changes, Changes)> {
    let staged = changes_to_be_committed_safe()
        .await
        .map_err(CliError::from)?;
    let ignore_case = effective_ignore_case_for_status().await?;
    let unstaged = changes_to_be_staged_with_ignore_case(ignore_case).map_err(CliError::from)?;
    Ok((staged, unstaged))
}

async fn effective_ignore_case_for_status() -> CliResult<bool> {
    crate::utils::path_case::effective_ignore_case()
        .await
        .map_err(|error| {
            CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
        })
}

fn dirty_cache_error(action: &str, error: anyhow::Error) -> CliError {
    CliError::fatal(format!("failed to {action} the dirty cache: {error}"))
        .with_stable_code(StableErrorCode::IoWriteFailed)
}

/// Snapshot rows from the raw sets ('/'-normalized repo-relative paths).
fn snapshot_rows(
    staged: &Changes,
    unstaged: &Changes,
) -> Result<Vec<(String, &'static str)>, CliError> {
    use crate::internal::dirty;
    let mut rows: Vec<(String, &'static str)> = Vec::new();
    let mut push = |paths: &[PathBuf], kind: &'static str| -> Result<(), CliError> {
        for path in paths {
            // Strict: the reconcile already refuses undecodable paths, so this
            // only fires defensively — the scan aborts rather than caching a
            // lossy-mangled path that would later verify as a different file.
            let stored = dirty::native_path_to_stored(path)
                .map_err(|e| dirty_cache_error("encode a path for", e))?;
            rows.push((stored, kind));
        }
        Ok(())
    };
    push(&unstaged.new, dirty::KIND_NEW)?;
    push(&unstaged.modified, dirty::KIND_MODIFIED)?;
    push(&unstaged.deleted, dirty::KIND_DELETED)?;
    push(&staged.new, dirty::KIND_STAGED_NEW)?;
    push(&staged.modified, dirty::KIND_STAGED_MODIFIED)?;
    push(&staged.deleted, dirty::KIND_STAGED_DELETED)?;
    Ok(rows)
}

/// `status --scan`: run the full safe reconcile and atomically replace the
/// cache snapshot from it. TOCTOU-safe: the index fingerprint and HEAD are
/// captured BEFORE the reconcile and re-verified AFTER — a concurrent index
/// writer aborts the cache commit (the old snapshot stays intact) instead of
/// stamping rows computed against an older index as fresh.
async fn run_status_scan(
    args: &StatusArgs,
    extras: StatusConfigExtras,
    output: &OutputConfig,
) -> CliResult<()> {
    use crate::internal::{
        db::get_db_conn_instance,
        dirty::{DirtyCache, ScanLockOutcome},
    };

    let index_path =
        path::try_index().map_err(|source| CliError::from(StatusError::Workdir { source }))?;
    let db = get_db_conn_instance().await;
    let pid = std::process::id() as i64;
    match DirtyCache::try_acquire_scan_lock_with_conn(&db, pid)
        .await
        .map_err(|e| dirty_cache_error("lock", e))?
    {
        ScanLockOutcome::Acquired { stole } => {
            if stole {
                crate::utils::error::emit_warning(
                    "stole a stale dirty-cache scan lock (previous scanner crashed?)",
                );
            }
        }
        ScanLockOutcome::Held { pid, since } => {
            return Err(CliError::failure(format!(
                "another `status --scan` holds the dirty-cache lock (pid {pid}, since {since})"
            ))
            .with_stable_code(StableErrorCode::ConflictOperationBlocked)
            .with_hint("wait for it to finish, or re-run later (stale locks are stolen)"));
        }
    }
    // Everything below must release the lock — including error paths.
    let result = run_status_scan_locked(args, output, &index_path, extras).await;
    let _ = DirtyCache::release_scan_lock_with_conn(&db, pid).await;
    result?;
    // Re-open a plain connection for the final read in JSON mode is not
    // needed; run_status_scan_locked rendered already.
    Ok(())
}

async fn run_status_scan_locked(
    args: &StatusArgs,
    output: &OutputConfig,
    index_path: &std::path::Path,
    extras: StatusConfigExtras,
) -> CliResult<()> {
    use sea_orm::TransactionTrait;

    use crate::internal::{
        db::get_db_conn_instance,
        dirty::{DirtyCache, current_index_fingerprint},
    };

    let fingerprint_before =
        current_index_fingerprint(index_path).map_err(|e| dirty_cache_error("fingerprint", e))?;
    let head_before = Head::current_commit().await.map(|oid| oid.to_string());
    let scan_started_at = crate::internal::dirty::now_timestamp();

    // The same full safe reconcile as the default status, raw + display.
    let (staged_raw, unstaged_raw) = compute_raw_sets().await?;
    let data = collect_status_data(args, extras).await?;

    let fingerprint_after =
        current_index_fingerprint(index_path).map_err(|e| dirty_cache_error("fingerprint", e))?;
    let head_after = Head::current_commit().await.map(|oid| oid.to_string());
    if fingerprint_before != fingerprint_after || head_before != head_after {
        return Err(CliError::failure(
            "the index or HEAD changed while scanning; the dirty cache was left untouched",
        )
        .with_stable_code(StableErrorCode::ConflictOperationBlocked)
        .with_hint("re-run 'libra status --scan' once the concurrent operation finishes"));
    }

    let rows = snapshot_rows(&staged_raw, &unstaged_raw)?;
    let row_count = rows.len();
    let db = get_db_conn_instance().await;
    let txn = db
        .begin()
        .await
        .map_err(|e| dirty_cache_error("open a transaction for", anyhow::anyhow!(e)))?;
    DirtyCache::replace_all_with_conn(
        &txn,
        &rows,
        &fingerprint_before,
        head_before.as_deref(),
        &scan_started_at,
    )
    .await
    .map_err(|e| dirty_cache_error("write", e))?;
    txn.commit()
        .await
        .map_err(|e| dirty_cache_error("commit", anyhow::anyhow!(e)))?;

    if output.is_json() {
        let mut json_data = build_status_json(&data, args);
        json_data["mode"] = serde_json::json!("scan");
        json_data["cached_paths"] = serde_json::json!(row_count);
        emit_json_data("status", &json_data, output)?;
    } else if !output.quiet {
        let mut stdout = std::io::stdout();
        render_status_to_writer(&data, args, output, &mut stdout).await?;
        println!("dirty cache rebuilt ({row_count} paths)");
    }
    if args.exit_code && data.is_dirty() {
        return Err(CliError::silent_exit(1));
    }
    Ok(())
}

/// Classify a manual (`kind='unknown'`) mark against the index, bounded and
/// panic-free (deliberately no `Index::is_modified`, which panics on missing
/// entries/files): returns the effective kind, or `None` when clean.
fn classify_manual_mark(
    index: &Index,
    workdir: &std::path::Path,
    stored: &str,
) -> Option<&'static str> {
    use crate::internal::dirty;
    let native = dirty::stored_path_to_native(stored);
    let Some(path_str) = native.to_str() else {
        return Some(dirty::KIND_NEW); // undecodable: over-report
    };
    let tracked = index.tracked(path_str, 0);
    let abs = workdir.join(&native);
    let exists = abs.symlink_metadata().is_ok();
    match (tracked, exists) {
        (false, true) => Some(dirty::KIND_NEW),
        (false, false) => None, // neither tracked nor present: not dirty
        (true, false) => Some(dirty::KIND_DELETED),
        (true, true) => {
            // Content confirm (no stat shortcut: manual marks are few, and a
            // wrong stat shortcut here would silently drop a real edit).
            match calc_file_blob_hash(&abs) {
                Ok(hash) if index.verify_hash(path_str, 0, &hash) => None,
                Ok(_) => Some(dirty::KIND_MODIFIED),
                Err(_) => Some(dirty::KIND_MODIFIED), // unreadable: over-report
            }
        }
    }
}

/// `status --cached` and `status --check-dirty`: consume / re-verify the
/// cache. Any freshness doubt degrades to the full reconcile (the cache may
/// over-report or degrade, never silently under-report).
async fn run_status_cache_mode(
    args: &StatusArgs,
    extras: StatusConfigExtras,
    output: &OutputConfig,
) -> CliResult<()> {
    use sea_orm::TransactionTrait;

    use crate::internal::{
        db::get_db_conn_instance,
        dirty::{self, CacheState, DirtyCache, current_index_fingerprint},
    };

    let index_path =
        path::try_index().map_err(|source| CliError::from(StatusError::Workdir { source }))?;
    let fingerprint =
        current_index_fingerprint(&index_path).map_err(|e| dirty_cache_error("fingerprint", e))?;
    let head_oid = Head::current_commit().await.map(|oid| oid.to_string());
    let db = get_db_conn_instance().await;
    let meta = DirtyCache::meta_with_conn(&db)
        .await
        .map_err(|e| dirty_cache_error("read", e))?;
    let state = DirtyCache::classify(meta.as_ref(), &fingerprint, head_oid.as_deref());

    if state != CacheState::Fresh {
        // Degrade to the full reconcile — never trust a doubtful cache.
        crate::utils::error::emit_warning(format!(
            "dirty cache is {}; falling back to the full status (run 'libra status --scan' to rebuild)",
            state.as_str()
        ));
        let data = collect_status_data(args, extras).await?;
        if output.is_json() {
            let mut json_data = build_status_json(&data, args);
            json_data["mode"] =
                serde_json::json!(if args.cached { "cached" } else { "check_dirty" });
            json_data["freshness"] = serde_json::json!("full");
            json_data["cache_state"] = serde_json::json!(state.as_str());
            emit_json_data("status", &json_data, output)?;
        } else if !output.quiet {
            let mut stdout = std::io::stdout();
            render_status_to_writer(&data, args, output, &mut stdout).await?;
        }
        if args.exit_code && data.is_dirty() {
            return Err(CliError::silent_exit(1));
        }
        return Ok(());
    }

    let rows = DirtyCache::list_with_conn(&db)
        .await
        .map_err(|e| dirty_cache_error("read", e))?;
    let workdir = util::try_working_dir()
        .map_err(|source| CliError::from(StatusError::Workdir { source }))?;
    let index = load_status_index()?;

    // Build the raw sets from the cache (staged snapshot + unstaged rows +
    // classified manual marks), optionally re-verifying (--check-dirty).
    let mut staged = Changes::default();
    let mut unstaged = Changes::default();
    let mut pruned: Vec<(String, String)> = Vec::new();
    let mut confirmed: Vec<(String, String)> = Vec::new();
    for row in &rows {
        let native = dirty::stored_path_to_native(&row.path);
        let verify = args.check_dirty;
        match row.kind.as_str() {
            dirty::KIND_STAGED_NEW => staged.new.push(native),
            dirty::KIND_STAGED_MODIFIED => staged.modified.push(native),
            dirty::KIND_STAGED_DELETED => staged.deleted.push(native),
            dirty::KIND_NEW => {
                // An undecodable stored path cannot be re-verified — keep it
                // (the cache must never under-report a recorded fact).
                let Some(path_str) = native.to_str() else {
                    unstaged.new.push(native);
                    continue;
                };
                let still = !verify
                    || (workdir.join(&native).symlink_metadata().is_ok()
                        && !index.tracked(path_str, 0));
                if still {
                    unstaged.new.push(native);
                    if verify {
                        confirmed.push((row.path.clone(), row.kind.clone()));
                    }
                } else {
                    pruned.push((row.path.clone(), row.kind.clone()));
                }
            }
            dirty::KIND_MODIFIED => {
                let Some(path_str) = native.to_str() else {
                    unstaged.modified.push(native);
                    continue;
                };
                let abs = workdir.join(&native);
                let still = !verify || {
                    index.tracked(path_str, 0)
                        && abs.symlink_metadata().is_ok()
                        && match calc_file_blob_hash(&abs) {
                            Ok(hash) => !index.verify_hash(path_str, 0, &hash),
                            Err(_) => true, // unreadable: keep (over-report)
                        }
                };
                if still {
                    unstaged.modified.push(native);
                    if verify {
                        confirmed.push((row.path.clone(), row.kind.clone()));
                    }
                } else {
                    pruned.push((row.path.clone(), row.kind.clone()));
                }
            }
            dirty::KIND_DELETED => {
                let Some(path_str) = native.to_str() else {
                    unstaged.deleted.push(native);
                    continue;
                };
                let still = !verify
                    || (index.tracked(path_str, 0)
                        && workdir.join(&native).symlink_metadata().is_err());
                if still {
                    unstaged.deleted.push(native);
                    if verify {
                        confirmed.push((row.path.clone(), row.kind.clone()));
                    }
                } else {
                    pruned.push((row.path.clone(), row.kind.clone()));
                }
            }
            _ => {
                // Manual 'unknown' marks: classified in memory, always content
                // confirmed (both modes — cheap, marks are few).
                match classify_manual_mark(&index, &workdir, &row.path) {
                    Some(dirty::KIND_NEW) => unstaged.new.push(native),
                    Some(dirty::KIND_DELETED) => unstaged.deleted.push(native),
                    Some(_) => unstaged.modified.push(native),
                    None => {
                        if verify {
                            pruned.push((row.path.clone(), row.kind.clone()));
                        }
                        // --cached: clean manual marks are dropped from the
                        // VIEW but kept in the cache (read-only fast path).
                    }
                }
            }
        }
    }
    let checked = rows.len();
    // Re-verify the epoch AFTER processing: a concurrent index/HEAD change
    // since the initial classify would make this view (and any prune) stale —
    // degrade instead of committing or rendering it as fresh.
    let fingerprint_now =
        current_index_fingerprint(&index_path).map_err(|e| dirty_cache_error("fingerprint", e))?;
    let head_now = Head::current_commit().await.map(|oid| oid.to_string());
    if fingerprint_now != fingerprint || head_now != head_oid {
        crate::utils::error::emit_warning(
            "the index or HEAD changed while reading the dirty cache; falling back to the full status",
        );
        let data = collect_status_data(args, extras).await?;
        if output.is_json() {
            let mut json_data = build_status_json(&data, args);
            json_data["mode"] =
                serde_json::json!(if args.cached { "cached" } else { "check_dirty" });
            json_data["freshness"] = serde_json::json!("full");
            json_data["cache_state"] = serde_json::json!("stale");
            emit_json_data("status", &json_data, output)?;
        } else if !output.quiet {
            let mut stdout = std::io::stdout();
            render_status_to_writer(&data, args, output, &mut stdout).await?;
        }
        if args.exit_code && data.is_dirty() {
            return Err(CliError::silent_exit(1));
        }
        return Ok(());
    }
    if args.check_dirty && (!pruned.is_empty() || !confirmed.is_empty()) {
        let txn = db
            .begin()
            .await
            .map_err(|e| dirty_cache_error("open a transaction for", anyhow::anyhow!(e)))?;
        DirtyCache::prune_and_confirm_with_conn(&txn, &pruned, &confirmed)
            .await
            .map_err(|e| dirty_cache_error("update", e))?;
        txn.commit()
            .await
            .map_err(|e| dirty_cache_error("commit", anyhow::anyhow!(e)))?;
    }

    // Assemble display data: cheap fresh pieces (head/upstream/merge state),
    // cache-derived changes (cwd-relative for display), NO rename detection
    // (would need object loads; documented) and no worktree walk.
    let head = Head::current().await;
    let head_oid_hash = Head::current_commit().await;
    let staged = staged.to_relative();
    let mut unstaged = unstaged.to_relative();
    // Honor the resolved display defaults exactly like the full status
    // (P1-05d): `status.showUntrackedFiles=no`/`-uno` hides untracked
    // entries (the cache stores explicit paths, so `normal` and `all`
    // render identically here), and `--show-stash`/`status.showStash`
    // surfaces the stash count. `status.relativePaths=false` is applied by
    // the shared renderer.
    if args.untracked_files == Some(UntrackedFiles::No) {
        unstaged.new.clear();
    }
    let stash_count = if args.show_stash {
        Some(stash::get_stash_num().map_err(|detail| {
            CliError::fatal(format!("failed to read stash state for status: {detail}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
                .with_hint("repair or remove the corrupt stash log, then retry")
        })?)
    } else {
        None
    };
    let upstream = resolve_upstream_info(&head, head_oid_hash.as_ref()).await?;
    let merge_state = match merge::MergeState::load_optional_sync().map_err(|detail| {
        CliError::fatal(format!("failed to inspect merge state: {detail}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })? {
        Some(state) => {
            let conflicted_paths =
                merge::unresolved_conflicted_paths(&index, &state.conflicted_paths);
            Some(MergeStatusInfo {
                target_ref: state.target_ref.clone(),
                unresolved_count: conflicted_paths.len(),
                conflicted_paths,
            })
        }
        None => None,
    };
    let mut data = StatusData {
        head,
        has_commits: head_oid_hash.is_some(),
        head_oid: head_oid_hash,
        staged,
        unstaged,
        unmerged: vec![],
        ignored_files: vec![],
        stash_count,
        upstream,
        merge_state,
        sequence_notice: sequence_notice().await?,
        sparse_view_active: crate::internal::sparse::SparseView::load()
            .await
            .is_active(),
        porcelain_v2: None,
        staged_rename_details: RenameDetails::new(),
        unstaged_rename_details: RenameDetails::new(),
    };
    filter_status_data_by_pathspec(&mut data, args)?;

    if output.is_json() {
        let mut json_data = build_status_json(&data, args);
        json_data["mode"] = serde_json::json!(if args.cached { "cached" } else { "check_dirty" });
        json_data["freshness"] = serde_json::json!("cached");
        json_data["cache_state"] = serde_json::json!("fresh");
        json_data["cached_paths"] = serde_json::json!(checked);
        if args.check_dirty {
            json_data["checked_paths"] = serde_json::json!(checked);
            json_data["stale_paths"] = serde_json::json!(
                pruned
                    .iter()
                    .map(|(path, _)| path.clone())
                    .collect::<Vec<_>>()
            );
        }
        emit_json_data("status", &json_data, output)?;
    } else if !output.quiet {
        let mut stdout = std::io::stdout();
        render_status_to_writer(&data, args, output, &mut stdout).await?;
        if args.check_dirty {
            println!(
                "dirty cache re-verified ({} checked, {} pruned)",
                checked,
                pruned.len()
            );
        }
    }
    if args.exit_code && data.is_dirty() {
        return Err(CliError::silent_exit(1));
    }
    Ok(())
}

/// Legacy entry point that writes status to the given writer.
/// Used by the old `execute()` path and tests.
pub async fn execute_to(mut args: StatusArgs, writer: &mut impl Write) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    let extras = apply_status_config_defaults(&mut args).await?;
    execute_to_resolved(ResolvedStatusArgs { args, extras }, writer).await
}

/// Collect and render status from arguments whose config defaults were already
/// resolved by [`resolve_config_defaults`]. This avoids a second, potentially
/// inconsistent config read after an embedded caller has crossed a side-effect
/// boundary.
pub(crate) async fn execute_to_resolved(
    resolved: ResolvedStatusArgs,
    writer: &mut impl Write,
) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    // Exactly one config read: the bundle carries the resolution performed
    // by `resolve_config_defaults` across the side-effect boundary, so no
    // second (potentially inconsistent) read ever happens here.
    let ResolvedStatusArgs { args, extras } = resolved;
    let data = collect_status_data(&args, extras).await?;
    let output = OutputConfig::default();
    render_status_to_writer(&data, &args, &output, writer).await
}

// ---------------------------------------------------------------------------
// Rendering dispatcher
// ---------------------------------------------------------------------------

async fn render_status_to_writer(
    data: &StatusData,
    args: &StatusArgs,
    output: &OutputConfig,
    writer: &mut impl Write,
) -> CliResult<()> {
    let write_error =
        |err: io::Error| CliError::io(format!("failed to write status output: {err}"));
    let mut buffer = Vec::new();

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
                &data.ignored_files,
                data.porcelain_v2.as_deref(),
                &data.staged_rename_details,
                &data.unstaged_rename_details,
                args.null_terminated,
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
                args.null_terminated,
                &mut buffer,
            )?;
            if args.ignored && !data.ignored_files.is_empty() {
                for file in &data.ignored_files {
                    if args.null_terminated {
                        write!(&mut buffer, "!! {}", file.display()).map_err(write_error)?;
                        buffer.push(b'\0');
                    } else {
                        writeln!(&mut buffer, "!! {}", file.display()).map_err(write_error)?;
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
        output_short_format_with_config(
            &data.staged,
            &data.unstaged,
            &data.unmerged,
            output,
            args.null_terminated,
            &mut buffer,
        )
        .await?;
        if args.ignored {
            for file in &data.ignored_files {
                if args.null_terminated {
                    write!(&mut buffer, "!! {}", file.display()).map_err(write_error)?;
                    buffer.push(b'\0');
                } else {
                    writeln!(&mut buffer, "!! {}", file.display()).map_err(write_error)?;
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
        let rooted = util::to_workdir_path(path);
        if path.to_string_lossy().ends_with('/') {
            PathBuf::from(format!("{}/", rooted.display()))
        } else {
            rooted
        }
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
    let mut rooted = data.clone();
    rooted.staged = changes(&data.staged);
    rooted.unstaged = changes(&data.unstaged);
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
        |err: io::Error| CliError::io(format!("failed to write status output: {err}"));

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
        render_upstream_human(upstream, buffer)?;
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
        );
        if args.column {
            render_columnated_labeled_entries(buffer, &entries, colored::Color::BrightGreen)?;
        } else {
            for (label, path) in entries {
                let line = format!("\t{label} {path}");
                writeln!(buffer, "{}", line.bright_green()).map_err(write_error)?;
            }
        }
    }

    // Unstaged changes (modified + deleted)
    if !data.unstaged.deleted.is_empty() || !data.unstaged.modified.is_empty() {
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
            &[],
            "",
            &data.unstaged.renamed,
            "renamed:",
        );
        if args.column {
            render_columnated_labeled_entries(buffer, &entries, colored::Color::BrightRed)?;
        } else {
            for (label, path) in entries {
                let line = format!("\t{label} {path}");
                writeln!(buffer, "{}", line.bright_red()).map_err(write_error)?;
            }
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
                    entry.path.display().to_string(),
                )
            })
            .collect::<Vec<_>>();
        if args.column {
            render_columnated_labeled_entries(buffer, &entries, colored::Color::BrightRed)?;
        } else {
            for (label, path) in entries {
                let line = format!("\t{label} {path}");
                writeln!(buffer, "{}", line.bright_red()).map_err(write_error)?;
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
            render_columnated_paths(buffer, &data.unstaged.new)?;
        } else {
            for f in &data.unstaged.new {
                let str = format!("\t{}", f.display());
                writeln!(buffer, "{}", str.bright_red()).map_err(write_error)?;
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
            render_columnated_paths(buffer, &data.ignored_files)?;
        } else {
            for f in &data.ignored_files {
                let str = format!("\t{}", f.display());
                writeln!(buffer, "{}", str.bright_red()).map_err(write_error)?;
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
) -> Vec<(&'a str, String)> {
    let mut entries = Vec::new();
    for f in deleted {
        entries.push((deleted_label, f.display().to_string()));
    }
    for f in modified {
        entries.push((modified_label, f.display().to_string()));
    }
    for (old, new) in renamed {
        entries.push((
            renamed_label,
            format!("{} -> {}", old.display(), new.display()),
        ));
    }
    for f in new_files {
        entries.push((new_label, f.display().to_string()));
    }
    entries
}

/// Render labeled entries in aligned columns.
fn render_columnated_labeled_entries(
    buffer: &mut Vec<u8>,
    entries: &[(&str, String)],
    color: colored::Color,
) -> CliResult<()> {
    let write_error =
        |err: io::Error| CliError::io(format!("failed to write status output: {err}"));
    if entries.is_empty() {
        return Ok(());
    }
    let max_label_width = entries.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
    for (label, path) in entries {
        let line = format!("\t{label:max_label_width$} {path}");
        let colored = match color {
            colored::Color::BrightGreen => line.bright_green().to_string(),
            colored::Color::BrightRed => line.bright_red().to_string(),
            _ => line,
        };
        writeln!(buffer, "{colored}").map_err(write_error)?;
    }
    Ok(())
}

/// Render plain paths in multiple columns like `ls`.
fn render_columnated_paths(buffer: &mut Vec<u8>, paths: &[PathBuf]) -> CliResult<()> {
    let write_error =
        |err: io::Error| CliError::io(format!("failed to write status output: {err}"));
    if paths.is_empty() {
        return Ok(());
    }

    let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
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
            if col + 1 < num_cols {
                write!(buffer, "{name:col_width$}").map_err(write_error)?;
            } else {
                write!(buffer, "{name}").map_err(write_error)?;
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
        |err: io::Error| CliError::io(format!("failed to write status output: {err}"));

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

fn render_upstream_human(upstream: &UpstreamInfo, buffer: &mut Vec<u8>) -> CliResult<()> {
    let write_error =
        |err: io::Error| CliError::io(format!("failed to write status output: {err}"));

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
        writeln!(
            buffer,
            "Your branch is ahead of '{}' by {} commit{}.",
            upstream.remote_ref,
            ahead,
            if ahead == 1 { "" } else { "s" }
        )
        .map_err(write_error)?;
        writeln!(
            buffer,
            "  (use \"libra push\" to publish your local commits)"
        )
        .map_err(write_error)?;
    } else if ahead == 0 && behind > 0 {
        writeln!(
            buffer,
            "Your branch is behind '{}' by {} commit{}.",
            upstream.remote_ref,
            behind,
            if behind == 1 { "" } else { "s" }
        )
        .map_err(write_error)?;
        writeln!(buffer, "  (use \"libra pull\" to update your local branch)")
            .map_err(write_error)?;
    } else {
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
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// JSON rendering
// ---------------------------------------------------------------------------

fn build_status_json(data: &StatusData, _args: &StatusArgs) -> serde_json::Value {
    let paths_to_json = |paths: &[PathBuf]| -> Vec<serde_json::Value> {
        paths
            .iter()
            .map(|p| serde_json::Value::String(p.display().to_string()))
            .collect()
    };

    let renamed_to_json = |renamed: &[(PathBuf, PathBuf)]| -> Vec<serde_json::Value> {
        renamed
            .iter()
            .map(|(old, new)| {
                serde_json::json!({
                    "from": old.display().to_string(),
                    "to": new.display().to_string(),
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
                "from": old.display().to_string(),
                "to": new.display().to_string(),
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
        "renames": renames,
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
    output_porcelain_with_unmerged(staged, unstaged, &[], null_terminated, writer)
}

fn output_porcelain_with_unmerged(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
    null_terminated: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let write_err = |e: io::Error| CliError::io(format!("failed to write status output: {e}"));

    // Renames render as a single `R  <old> -> <new>` record (Git porcelain v1
    // §B.6.3), never as two `R` endpoint rows. Under `-z` the record is
    // `XY SP <new> NUL <old> NUL` (new before old, matching Git).
    let mut endpoints: HashSet<PathBuf> = HashSet::new();
    let mut rename_lines: Vec<(char, char, PathBuf, PathBuf)> = Vec::new();
    for (old, new) in &staged.renamed {
        endpoints.insert(old.clone());
        endpoints.insert(new.clone());
        // The Y column carries the worktree state of the rename's NEW path
        // (Git: `RM`/`RD` when the destination is modified/deleted unstaged;
        // the endpoint row is suppressed below so this is its only signal).
        let y = if unstaged.modified.contains(new) {
            'M'
        } else if unstaged.deleted.contains(new) {
            'D'
        } else {
            ' '
        };
        rename_lines.push(('R', y, old.clone(), new.clone()));
    }
    for (old, new) in &unstaged.renamed {
        endpoints.insert(old.clone());
        endpoints.insert(new.clone());
        rename_lines.push((' ', 'R', old.clone(), new.clone()));
    }

    let status_list = generate_short_format_status_with_unmerged(staged, unstaged, unmerged);
    for (file, staged_status, unstaged_status) in status_list {
        if endpoints.contains(&file) {
            continue;
        }
        write!(
            writer,
            "{staged_status}{unstaged_status} {}",
            file.display()
        )
        .map_err(write_err)?;
        writer
            .write_all(if null_terminated { b"\0" } else { b"\n" })
            .map_err(write_err)?;
    }
    for (x, y, old, new) in rename_lines {
        if null_terminated {
            write!(writer, "{x}{y} {}", new.display()).map_err(write_err)?;
            writer.write_all(b"\0").map_err(write_err)?;
            write!(writer, "{}", old.display()).map_err(write_err)?;
            writer.write_all(b"\0").map_err(write_err)?;
        } else {
            writeln!(writer, "{x}{y} {} -> {}", old.display(), new.display()).map_err(write_err)?;
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

struct PorcelainV2Data {
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
fn index_mode_to_tree_item_mode(mode: u32) -> TreeItemMode {
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

fn current_to_workdir(path: &std::path::Path) -> PathBuf {
    let abs_path = util::cur_dir().join(path);
    util::to_workdir_path(&abs_path)
}

#[cfg(unix)]
fn get_worktree_mode(file_path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let workdir_path = current_to_workdir(file_path);
    let abs_path = util::workdir_to_absolute(&workdir_path);
    if let Ok(metadata) = std::fs::symlink_metadata(&abs_path) {
        if metadata.file_type().is_symlink() {
            0o120000
        } else if metadata.permissions().mode() & 0o111 != 0 {
            0o100755
        } else {
            0o100644
        }
    } else {
        0o100644
    }
}

#[cfg(not(unix))]
fn get_worktree_mode(_file_path: &std::path::Path) -> u32 {
    0o100644
}

fn is_submodule_mode(mode: u32) -> bool {
    mode == 0o160000
}

fn get_submodule_status(_file_path: &std::path::Path) -> String {
    "S...".to_string()
}

fn build_porcelain_v2_data(index: Index, head_oid: Option<&ObjectHash>) -> PorcelainV2Data {
    let head_tree_items = if let Some(commit_hash) = head_oid {
        let commit = Commit::load(commit_hash);
        let tree = Tree::load(&commit.tree_id);
        tree.get_plain_items_with_mode()
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
    writer: &mut impl Write,
) -> CliResult<()> {
    let write_err = |e: io::Error| CliError::io(format!("failed to write status output: {e}"));
    let old_workdir = current_to_workdir(old);
    let new_workdir = current_to_workdir(new);

    // Staged rename: HEAD side is the OLD path, index side is the NEW path.
    // Unstaged-only rename (`.R`): there is no staged component, so Git copies
    // the index fields into the HEAD fields.
    let staged_rename = x == 'R';
    let (mode_head, hash_head) = if staged_rename {
        metadata
            .head_tree_items
            .get(&old_workdir)
            .map(|info| (info.mode, info.hash.clone()))
            .unwrap_or((0, zero_hash.to_string()))
    } else {
        // `.R`: filled from the index below (fixup).
        (0, zero_hash.to_string())
    };
    let index_key = if staged_rename {
        &new_workdir
    } else {
        &old_workdir
    };
    let index_str = index_key.to_str().unwrap_or_default();
    let (mode_index, hash_index) = metadata
        .index
        .get(index_str, 0)
        .map(|entry| (entry.mode, entry.hash.to_string()))
        .unwrap_or((0o100644, zero_hash.to_string()));
    let (mode_head, hash_head) = if staged_rename {
        (mode_head, hash_head)
    } else {
        (mode_index, hash_index.clone())
    };
    // A worktree-deleted destination (`RD`) has no worktree entry: mW must
    // be 000000 like an ordinary v2 deleted row, not a fabricated 100644.
    let mode_worktree = if y == 'D' { 0 } else { get_worktree_mode(new) };
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
        write!(writer, "{}", new.display()).map_err(write_err)?;
        writer.write_all(b"\0").map_err(write_err)?;
        write!(writer, "{}", old.display()).map_err(write_err)?;
        writer.write_all(b"\0").map_err(write_err)?;
    } else {
        write!(writer, "{}\t{}", new.display(), old.display()).map_err(write_err)?;
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
    ignored: &[PathBuf],
    metadata: Option<&PorcelainV2Data>,
    staged_rename_details: &RenameDetails,
    unstaged_rename_details: &RenameDetails,
    null_terminated: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let metadata =
        metadata.ok_or_else(|| CliError::internal("missing porcelain v2 metadata for status"))?;
    let zero_hash = zero_hash_str();
    let write_err = |e: io::Error| CliError::io(format!("failed to write status output: {e}"));

    for entry in unmerged {
        write_unmerged_porcelain_v2(entry, &zero_hash, null_terminated, writer)?;
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
        let score = staged_rename_details
            .get(&(old.clone(), new.clone()))
            .map(|(pct, _)| *pct)
            .unwrap_or(100);
        write_rename_porcelain_v2(
            old,
            new,
            'R',
            unstaged_char,
            score,
            metadata,
            &zero_hash,
            null_terminated,
            writer,
        )?;
    }
    for (old, new) in &unstaged.renamed {
        endpoints.insert(old.clone());
        endpoints.insert(new.clone());
        let score = unstaged_rename_details
            .get(&(old.clone(), new.clone()))
            .map(|(pct, _)| *pct)
            .unwrap_or(100);
        write_rename_porcelain_v2(
            old,
            new,
            '.',
            'R',
            score,
            metadata,
            &zero_hash,
            null_terminated,
            writer,
        )?;
    }

    let status_list = generate_short_format_status(staged, unstaged);
    for (file, staged_status, unstaged_status) in status_list {
        if endpoints.contains(&file) {
            continue;
        }
        if staged_status == '?' && unstaged_status == '?' {
            write!(writer, "? {}", file.display()).map_err(write_err)?;
            if null_terminated {
                writer.write_all(b"\0").map_err(write_err)?;
            } else {
                writer.write_all(b"\n").map_err(write_err)?;
            }
            continue;
        }

        let workdir_path = current_to_workdir(&file);
        let file_str = workdir_path.to_str().unwrap_or_default();

        let (mode_index, hash_index) = if let Some(entry) = metadata.index.get(file_str, 0) {
            (entry.mode, entry.hash.to_string())
        } else {
            (0o100644, zero_hash.clone())
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
            get_worktree_mode(&file)
        };

        let sub = if is_submodule_mode(mode_index) || is_submodule_mode(mode_head) {
            get_submodule_status(&file)
        } else {
            "N...".to_string()
        };

        write!(
            writer,
            "1 {}{} {} {} {} {} {} {} {}",
            staged_status,
            unstaged_status,
            sub,
            format_mode(mode_head),
            format_mode(mode_index),
            format_mode(mode_worktree),
            hash_head,
            hash_index,
            file.display()
        )
        .map_err(write_err)?;
        if null_terminated {
            writer.write_all(b"\0").map_err(write_err)?;
        } else {
            writer.write_all(b"\n").map_err(write_err)?;
        }
    }

    for file in ignored {
        write!(writer, "! {}", file.display()).map_err(write_err)?;
        if null_terminated {
            writer.write_all(b"\0").map_err(write_err)?;
        } else {
            writer.write_all(b"\n").map_err(write_err)?;
        }
    }
    Ok(())
}

fn zero_hash_str() -> String {
    ObjectHash::zero_str(get_hash_kind())
}

fn write_unmerged_porcelain_v2(
    entry: &UnmergedEntry,
    zero_hash: &str,
    null_terminated: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let write_err = |e: io::Error| CliError::io(format!("failed to write status output: {e}"));
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
        "u {}{} N... {} {} {} {} {} {} {} {}",
        staged_status,
        unstaged_status,
        mode(1),
        mode(2),
        mode(3),
        format_mode(get_unmerged_worktree_mode(&entry.path)),
        hash(1),
        hash(2),
        hash(3),
        entry.path.display()
    )
    .map_err(write_err)?;
    if null_terminated {
        writer.write_all(b"\0").map_err(write_err)?;
    } else {
        writer.write_all(b"\n").map_err(write_err)?;
    }
    Ok(())
}

fn get_unmerged_worktree_mode(file_path: &std::path::Path) -> u32 {
    let workdir_path = current_to_workdir(file_path);
    let abs_path = util::workdir_to_absolute(&workdir_path);
    if std::fs::symlink_metadata(&abs_path).is_ok() {
        get_worktree_mode(file_path)
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Short format
// ---------------------------------------------------------------------------

/// Core logic for generating short format status without color (for testing)
pub fn generate_short_format_status(
    staged: &Changes,
    unstaged: &Changes,
) -> Vec<(std::path::PathBuf, char, char)> {
    generate_short_format_status_with_unmerged(staged, unstaged, &[])
}

pub(crate) fn generate_short_format_status_with_unmerged(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
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
    for (old, new) in &staged.renamed {
        file_status.insert(old.clone(), ('R', ' '));
        file_status.insert(new.clone(), ('R', ' '));
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

    process_unstaged_changes(&unstaged.modified, &mut file_status, 'M');
    process_unstaged_changes(&unstaged.deleted, &mut file_status, 'D');
    for (old, new) in &unstaged.renamed {
        process_unstaged_changes(std::slice::from_ref(old), &mut file_status, 'R');
        process_unstaged_changes(std::slice::from_ref(new), &mut file_status, 'R');
    }

    for file in &unstaged.new {
        file_status.insert(file.clone(), ('?', '?'));
    }
    for entry in unmerged {
        file_status.insert(entry.path.clone(), entry.xy());
    }

    let mut sorted_files: Vec<_> = file_status.iter().collect();
    sorted_files.sort_by(|a, b| a.0.cmp(b.0));

    sorted_files
        .into_iter()
        .map(|(file, (staged_status, unstaged_status))| {
            (file.clone(), *staged_status, *unstaged_status)
        })
        .collect()
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
        &OutputConfig::default(),
        false,
        writer,
    )
    .await
}

/// A short-format render item: either a plain per-path change or a rename
/// pair (rendered with Git's `old -> new` arrow instead of two `R` rows).
enum ShortItem {
    Path {
        file: PathBuf,
        x: char,
        y: char,
    },
    Rename {
        old: PathBuf,
        new: PathBuf,
        x: char,
        y: char,
    },
}

impl ShortItem {
    /// Sort key — Git orders renames by their new (destination) path.
    fn sort_key(&self) -> &Path {
        match self {
            ShortItem::Path { file, .. } => file,
            ShortItem::Rename { new, .. } => new,
        }
    }
}

/// Short format output with color controlled by OutputConfig.
///
/// Renames are rendered as a single `R  <old> -> <new>` line (Git's short
/// rename form), not as two separate `R` rows (§B.6.1). Under `-z` the record
/// is `XY SP <new> NUL <old> NUL` (new before old, matching Git).
async fn output_short_format_with_config(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
    output: &OutputConfig,
    null_terminated: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let use_colors = should_use_colors(output).await;
    let write_err = |e: io::Error| CliError::io(format!("failed to write status output: {e}"));

    // Collect rename pairs and their endpoints so the flattened tuple list can
    // exclude them (they render as arrow lines instead).
    let mut items: Vec<ShortItem> = Vec::new();
    let mut endpoints: HashSet<PathBuf> = HashSet::new();
    for (old, new) in &staged.renamed {
        endpoints.insert(old.clone());
        endpoints.insert(new.clone());
        // The Y column carries the worktree state of the NEW path (`RM`/`RD`)
        // — the endpoint row is suppressed, so this is its only signal.
        let y = if unstaged.modified.contains(new) {
            'M'
        } else if unstaged.deleted.contains(new) {
            'D'
        } else {
            ' '
        };
        items.push(ShortItem::Rename {
            old: old.clone(),
            new: new.clone(),
            x: 'R',
            y,
        });
    }
    for (old, new) in &unstaged.renamed {
        endpoints.insert(old.clone());
        endpoints.insert(new.clone());
        items.push(ShortItem::Rename {
            old: old.clone(),
            new: new.clone(),
            x: ' ',
            y: 'R',
        });
    }

    for (file, x, y) in generate_short_format_status_with_unmerged(staged, unstaged, unmerged) {
        if endpoints.contains(&file) {
            continue;
        }
        items.push(ShortItem::Path { file, x, y });
    }
    items.sort_by(|a, b| a.sort_key().cmp(b.sort_key()));

    for item in items {
        match item {
            ShortItem::Path { file, x, y } => {
                if use_colors {
                    write!(writer, "{}", format_colored_status(x, y, &file)).map_err(write_err)?;
                } else {
                    write!(writer, "{x}{y} {}", file.display()).map_err(write_err)?;
                }
                writer
                    .write_all(if null_terminated { b"\0" } else { b"\n" })
                    .map_err(write_err)?;
            }
            ShortItem::Rename { old, new, x, y } => {
                if null_terminated {
                    // `XY SP <new> NUL <old> NUL` (§B.6.1).
                    write!(writer, "{x}{y} {}", new.display()).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                    write!(writer, "{}", old.display()).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                } else if use_colors {
                    let head = format_colored_status(x, y, &old);
                    // `format_colored_status` renders `XY <old>`; append the arrow.
                    writeln!(writer, "{head} -> {}", new.display()).map_err(write_err)?;
                } else {
                    writeln!(writer, "{x}{y} {} -> {}", old.display(), new.display())
                        .map_err(write_err)?;
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

fn format_colored_status(
    staged_status: char,
    unstaged_status: char,
    file: &std::path::Path,
) -> String {
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

    format!("{}{} {}", colored_staged, colored_unstaged, file.display())
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
    let write_err = |e: io::Error| CliError::io(format!("failed to write status output: {e}"));
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
                    let ahead = u.ahead.unwrap_or(0);
                    let behind = u.behind.unwrap_or(0);
                    if ahead > 0 && behind > 0 {
                        format!("## {tracking} [ahead {ahead}, behind {behind}]")
                    } else if ahead > 0 {
                        format!("## {tracking} [ahead {ahead}]")
                    } else if behind > 0 {
                        format!("## {tracking} [behind {behind}]")
                    } else {
                        format!("## {tracking}")
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
    let write_err = |e: io::Error| CliError::io(format!("failed to write status output: {e}"));
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
        if !u.gone && show_ahead_behind {
            let ahead = u.ahead.unwrap_or(0);
            let behind = u.behind.unwrap_or(0);
            write!(writer, "# branch.ab +{ahead} -{behind}").map_err(write_err)?;
            writer.write_all(term).map_err(write_err)?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Upstream tracking resolution
// ---------------------------------------------------------------------------

fn status_branch_store_error(context: &str, error: BranchStoreError) -> CliError {
    match error {
        BranchStoreError::Query(detail) => {
            CliError::fatal(format!("failed to {context}: {detail}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        }
        other => CliError::fatal(format!("failed to {context}: {other}"))
            .with_stable_code(StableErrorCode::RepoCorrupt),
    }
}

fn status_config_read_error(context: &str, error: anyhow::Error) -> CliError {
    CliError::fatal(format!("failed to {context}: {error}"))
        .with_stable_code(StableErrorCode::IoReadFailed)
}

async fn resolve_upstream_info(
    head: &Head,
    local_commit: Option<&ObjectHash>,
) -> CliResult<Option<UpstreamInfo>> {
    let branch_name = match head {
        Head::Branch(name) => name.clone(),
        Head::Detached(_) => return Ok(None),
    };

    let branch_config = match ConfigKv::branch_config(&branch_name).await {
        Ok(Some(config)) => config,
        Ok(None) => return Ok(None),
        Err(error) => {
            return Err(status_config_read_error(
                &format!("read branch configuration for '{branch_name}'"),
                error,
            ));
        }
    };

    let remote = &branch_config.remote;
    let merge_branch = &branch_config.merge;
    let remote_ref_display = format!("{remote}/{merge_branch}");

    let tracking_branch = Branch::find_branch_result(merge_branch, Some(remote))
        .await
        .map_err(|error| status_branch_store_error("resolve upstream branch", error))?;

    let tracking_commit = match tracking_branch {
        Some(b) => b.commit,
        None => {
            // Upstream configured but tracking ref doesn't exist → gone
            return Ok(Some(UpstreamInfo {
                remote_ref: remote_ref_display,
                ahead: None,
                behind: None,
                gone: true,
            }));
        }
    };

    let local_commit = match local_commit {
        Some(commit) => commit,
        None => {
            // Unborn branch: no local commit to compare against.
            // Return None for ahead/behind — numeric counts would imply
            // a comparison that never happened.
            return Ok(Some(UpstreamInfo {
                remote_ref: remote_ref_display,
                ahead: None,
                behind: None,
                gone: false,
            }));
        }
    };

    let (ahead, behind) = compute_ahead_behind(local_commit, &tracking_commit);

    Ok(Some(UpstreamInfo {
        remote_ref: remote_ref_display,
        ahead: Some(ahead),
        behind: Some(behind),
        gone: false,
    }))
}

/// Compute the number of commits ahead/behind between two refs.
///
/// Performs a bidirectional BFS from both tips, classifying each commit as
/// local-only, remote-only, or common (reachable from both sides).  Once a
/// commit is found from the opposite side it is reclassified as common and
/// its ancestors are not enqueued again, which reduces redundant work when
/// the histories share a recent merge-base.
///
/// **Complexity**: proportional to the number of commits reachable from
/// both tips until the queues are drained.  For disjoint histories (no
/// common ancestor) this visits all reachable commits from both sides.
/// Falls back gracefully when a commit object is missing or corrupt
/// (e.g. shallow clone) by stopping traversal on that branch.
pub(crate) fn compute_ahead_behind(local: &ObjectHash, remote: &ObjectHash) -> (usize, usize) {
    if local == remote {
        return (0, 0);
    }

    let mut local_only: HashSet<ObjectHash> = HashSet::new();
    let mut remote_only: HashSet<ObjectHash> = HashSet::new();
    let mut common: HashSet<ObjectHash> = HashSet::new();
    let mut local_queue: VecDeque<ObjectHash> = VecDeque::new();
    let mut remote_queue: VecDeque<ObjectHash> = VecDeque::new();

    local_queue.push_back(*local);
    remote_queue.push_back(*remote);

    while !local_queue.is_empty() || !remote_queue.is_empty() {
        // Expand one commit from the local side.
        if let Some(hash) = local_queue.pop_front() {
            if common.contains(&hash) {
                // Already common — skip without expanding parents.
                continue;
            } else if remote_only.remove(&hash) {
                // Discovered from the remote side too → merge-base.
                common.insert(hash);
            } else if local_only.insert(hash)
                && let Some(commit) = Commit::try_load(&hash)
            {
                for parent in &commit.parent_commit_ids {
                    if !common.contains(parent) {
                        local_queue.push_back(*parent);
                    }
                }
            }
        }

        // Expand one commit from the remote side.
        if let Some(hash) = remote_queue.pop_front() {
            if common.contains(&hash) {
                continue;
            } else if local_only.remove(&hash) {
                common.insert(hash);
            } else if remote_only.insert(hash)
                && let Some(commit) = Commit::try_load(&hash)
            {
                for parent in &commit.parent_commit_ids {
                    if !common.contains(parent) {
                        remote_queue.push_back(*parent);
                    }
                }
            }
        }
    }

    (local_only.len(), remote_only.len())
}

// ---------------------------------------------------------------------------
// Bare repository detection
// ---------------------------------------------------------------------------

async fn is_bare_repository() -> bool {
    matches!(
        ConfigKv::get("core.bare").await.ok().flatten().map(|e| e.value),
        Some(value) if value.eq_ignore_ascii_case("true")
    )
}

// ---------------------------------------------------------------------------
// Untracked directory collapsing
// ---------------------------------------------------------------------------

pub(crate) fn collapse_untracked_directories(
    untracked_files: Vec<PathBuf>,
    index: &Index,
) -> Vec<PathBuf> {
    use std::collections::BTreeSet;

    if untracked_files.is_empty() {
        return untracked_files;
    }

    let mut dir_files: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    let mut root_files: Vec<PathBuf> = Vec::new();

    for file in &untracked_files {
        let components: Vec<_> = file.components().collect();
        if components.len() > 1 {
            let top_dir = PathBuf::from(components[0].as_os_str());
            dir_files.entry(top_dir).or_default().push(file.clone());
        } else {
            root_files.push(file.clone());
        }
    }

    let mut result: BTreeSet<PathBuf> = BTreeSet::new();

    for file in root_files {
        result.insert(file);
    }

    for (dir, files) in dir_files {
        let dir_prefix = format!("{}/", dir.display());
        let has_tracked_files = index.tracked_files().iter().any(|f| {
            f.to_str()
                .map(|s| s.starts_with(&dir_prefix))
                .unwrap_or(false)
        });

        if has_tracked_files {
            for file in files {
                result.insert(file);
            }
        } else {
            let dir_str = format!("{}/", dir.display());
            let dir_path = PathBuf::from(dir_str);
            result.insert(dir_path);
        }
    }

    result.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Clean check
// ---------------------------------------------------------------------------

/// Check if the working tree is clean.
///
/// Returns `false` when the status cannot be determined (e.g. corrupt index).
pub async fn is_clean() -> bool {
    let staged = match changes_to_be_committed_safe().await {
        Ok(c) => c,
        Err(err) => {
            tracing::error!("failed to calculate committed changes: {err}");
            return false;
        }
    };
    let unstaged = match changes_to_be_staged() {
        Ok(c) => c,
        Err(err) => {
            tracing::error!("failed to calculate staged changes: {err}");
            return false;
        }
    };
    staged.is_empty() && unstaged.is_empty()
}

// ---------------------------------------------------------------------------
// Status computation (public API preserved)
// ---------------------------------------------------------------------------

/// Convenience wrapper around [`changes_to_be_committed_safe`].
///
/// On error (e.g. corrupt index), logs the failure and returns an empty
/// [`Changes`] set instead of panicking.
pub async fn changes_to_be_committed() -> Changes {
    match changes_to_be_committed_safe().await {
        Ok(changes) => changes,
        Err(err) => {
            tracing::error!("changes_to_be_committed failed: {err}");
            Changes::default()
        }
    }
}

pub async fn changes_to_be_committed_safe() -> Result<Changes, StatusError> {
    let mut changes = Changes::default();
    let index_path = path::try_index().map_err(|source| StatusError::Workdir { source })?;
    let index = Index::load(&index_path).map_err(|source| StatusError::IndexLoad {
        path: index_path.clone(),
        source,
    })?;
    let head_commit = Head::current_commit().await;
    let tracked_files = index.tracked_files();

    if head_commit.is_none() {
        changes.new = tracked_files;
        return Ok(changes);
    }

    let head_commit = match head_commit {
        Some(head_commit) => head_commit,
        None => return Ok(changes),
    };
    let commit = Commit::load(&head_commit);
    let tree = Tree::load(&commit.tree_id);
    let tree_files = tree.get_plain_items_with_mode();

    for (item_path, item_hash, item_mode) in tree_files.iter() {
        let item_str = item_path
            .to_str()
            .ok_or_else(|| StatusError::InvalidPathEncoding {
                path: item_path.clone(),
            })?;
        if index.tracked(item_str, 0) {
            // A staged change is either a content change (blob hash differs) OR a
            // mode change (e.g. `add --chmod=+x`): the index records 100755 while
            // the HEAD tree still has 100644, with the same blob.
            let content_changed = !index.verify_hash(item_str, 0, item_hash);
            let mode_changed = index
                .get(item_str, 0)
                .is_some_and(|entry| index_mode_to_tree_item_mode(entry.mode) != *item_mode);
            if content_changed || mode_changed {
                changes.modified.push(item_path.clone());
            }
        } else {
            changes.deleted.push(item_path.clone());
        }
    }
    let tree_files_set: HashSet<PathBuf> =
        tree_files.into_iter().map(|(path, _, _)| path).collect();
    changes.new = tracked_files
        .into_iter()
        .filter(|path| !tree_files_set.contains(path))
        .collect();

    Ok(changes)
}

/// Compare the difference between `index` and the `workdir` using the default ignore rules.
pub fn changes_to_be_staged() -> Result<Changes, StatusError> {
    changes_to_be_staged_with_policy(IgnorePolicy::Respect)
}

/// Variant of [`changes_to_be_staged`] that lets callers pick the ignore strategy explicitly.
/// Commands such as `add --force` or `status --ignored` can switch policies as needed.
pub fn changes_to_be_staged_with_policy(policy: IgnorePolicy) -> Result<Changes, StatusError> {
    let workdir = util::try_working_dir().map_err(|source| StatusError::Workdir { source })?;
    let ignore_case = effective_ignore_case_for_workdir(&workdir)?;
    changes_to_be_staged_with_policy_and_ignore_case(policy, ignore_case)
}

pub(crate) fn changes_to_be_staged_with_ignore_case(
    ignore_case: bool,
) -> Result<Changes, StatusError> {
    changes_to_be_staged_with_policy_and_ignore_case(IgnorePolicy::Respect, ignore_case)
}

fn changes_to_be_staged_with_policy_and_ignore_case(
    policy: IgnorePolicy,
    ignore_case: bool,
) -> Result<Changes, StatusError> {
    let workdir = util::try_working_dir().map_err(|source| StatusError::Workdir { source })?;
    let index_path = path::try_index().map_err(|source| StatusError::Workdir { source })?;
    let index = Index::load(&index_path).map_err(|source| StatusError::IndexLoad {
        path: index_path.clone(),
        source,
    })?;
    let (mut visible, ignored) =
        changes_to_be_staged_split_with_index(&workdir, &index, ignore_case)?;
    match policy {
        IgnorePolicy::Respect => Ok(visible),
        IgnorePolicy::OnlyIgnored => Ok(ignored),
        IgnorePolicy::IncludeIgnored => {
            visible.extend(ignored);
            Ok(visible)
        }
    }
}

pub fn changes_to_be_staged_split_safe() -> Result<(Changes, Changes), StatusError> {
    let workdir = util::try_working_dir().map_err(|source| StatusError::Workdir { source })?;
    let ignore_case = effective_ignore_case_for_workdir(&workdir)?;
    changes_to_be_staged_split_safe_with_ignore_case(ignore_case)
}

pub(crate) fn changes_to_be_staged_split_safe_with_ignore_case(
    ignore_case: bool,
) -> Result<(Changes, Changes), StatusError> {
    let workdir = util::try_working_dir().map_err(|source| StatusError::Workdir { source })?;
    let index_path = path::try_index().map_err(|source| StatusError::Workdir { source })?;
    let index = Index::load(&index_path).map_err(|source| StatusError::IndexLoad {
        path: index_path.clone(),
        source,
    })?;
    changes_to_be_staged_split_with_index(&workdir, &index, ignore_case)
}

/// List changes to be staged with --force semantics (recurse into ignored directories)
pub fn changes_to_be_staged_split_force() -> Result<(Changes, Changes), StatusError> {
    let workdir = util::try_working_dir().map_err(|source| StatusError::Workdir { source })?;
    let ignore_case = effective_ignore_case_for_workdir(&workdir)?;
    changes_to_be_staged_split_force_with_ignore_case(ignore_case)
}

fn effective_ignore_case_for_workdir(workdir: &Path) -> Result<bool, StatusError> {
    crate::utils::path_case::effective_ignore_case_for_dir_sync(workdir)
        .map_err(|source| StatusError::ConfigRead { source })
}

pub(crate) fn changes_to_be_staged_split_force_with_ignore_case(
    ignore_case: bool,
) -> Result<(Changes, Changes), StatusError> {
    let workdir = util::try_working_dir().map_err(|source| StatusError::Workdir { source })?;
    let index_path = path::try_index().map_err(|source| StatusError::Workdir { source })?;
    let index = Index::load(&index_path).map_err(|source| StatusError::IndexLoad {
        path: index_path.clone(),
        source,
    })?;
    changes_to_be_staged_split_force_with_index(&workdir, &index, ignore_case)
}

fn changes_to_be_staged_split_force_with_index(
    workdir: &PathBuf,
    index: &Index,
    ignore_case: bool,
) -> Result<(Changes, Changes), StatusError> {
    let mut visible = Changes::default();
    let mut ignored = Changes::default();
    let tracked_files = index.tracked_files();
    let tracked_fold = tracked_files_by_fold(&tracked_files, ignore_case);
    for file in tracked_files.iter() {
        let file_str = file
            .to_str()
            .ok_or_else(|| StatusError::InvalidPathEncoding { path: file.clone() })?;
        let file_abs = workdir.join(file);
        if file_abs.symlink_metadata().is_err() {
            visible.deleted.push(file.clone());
        } else if index.is_modified(file_str, 0, workdir) {
            let file_hash =
                calc_file_blob_hash(&file_abs).map_err(|source| StatusError::FileHash {
                    path: file_abs.clone(),
                    source,
                })?;
            if !index.verify_hash(file_str, 0, &file_hash) {
                visible.modified.push(file.clone());
            }
        }
    }
    let (files, ignored_files) = list_workdir_files_split_force(workdir).map_err(|source| {
        StatusError::ListWorkdirFiles {
            path: workdir.clone(),
            source,
        }
    })?;
    for file in files {
        let file_str = file
            .to_str()
            .ok_or_else(|| StatusError::InvalidPathEncoding { path: file.clone() })?;
        if !index.tracked(file_str, 0) && !is_same_file_tracked_alias(workdir, &file, &tracked_fold)
        {
            visible.new.push(file);
        }
    }
    for file in ignored_files {
        let file_str = file
            .to_str()
            .ok_or_else(|| StatusError::InvalidPathEncoding { path: file.clone() })?;
        if !index.tracked(file_str, 0) && !is_same_file_tracked_alias(workdir, &file, &tracked_fold)
        {
            ignored.new.push(file);
        }
    }
    Ok((visible, ignored))
}

fn changes_to_be_staged_split_with_index(
    workdir: &PathBuf,
    index: &Index,
    ignore_case: bool,
) -> Result<(Changes, Changes), StatusError> {
    let mut visible = Changes::default();
    let mut ignored = Changes::default();
    let tracked_files = index.tracked_files();
    let tracked_fold = tracked_files_by_fold(&tracked_files, ignore_case);
    for file in tracked_files.iter() {
        let file_str = file
            .to_str()
            .ok_or_else(|| StatusError::InvalidPathEncoding { path: file.clone() })?;
        let file_abs = workdir.join(file);
        if file_abs.symlink_metadata().is_err() {
            visible.deleted.push(file.clone());
        } else if index.is_modified(file_str, 0, workdir) {
            let file_hash =
                calc_file_blob_hash(&file_abs).map_err(|source| StatusError::FileHash {
                    path: file_abs.clone(),
                    source,
                })?;
            if !index.verify_hash(file_str, 0, &file_hash) {
                visible.modified.push(file.clone());
            }
        }
    }
    let (files, ignored_files) =
        list_workdir_files_split_safe(workdir).map_err(|source| StatusError::ListWorkdirFiles {
            path: workdir.clone(),
            source,
        })?;
    for file in files {
        let file_str = file
            .to_str()
            .ok_or_else(|| StatusError::InvalidPathEncoding { path: file.clone() })?;
        if !index.tracked(file_str, 0) && !is_same_file_tracked_alias(workdir, &file, &tracked_fold)
        {
            visible.new.push(file);
        }
    }
    for file in ignored_files {
        let file_str = file
            .to_str()
            .ok_or_else(|| StatusError::InvalidPathEncoding { path: file.clone() })?;
        if !index.tracked(file_str, 0) && !is_same_file_tracked_alias(workdir, &file, &tracked_fold)
        {
            ignored.new.push(file);
        }
    }
    Ok((visible, ignored))
}

fn tracked_files_by_fold(tracked_files: &[PathBuf], ignore_case: bool) -> HashMap<String, PathBuf> {
    if !ignore_case {
        return HashMap::new();
    }
    tracked_files
        .iter()
        .map(|path| {
            (
                crate::utils::path_case::fold_path_key(path.to_string_lossy().as_ref()),
                path.clone(),
            )
        })
        .collect()
}

fn is_same_file_tracked_alias(
    workdir: &Path,
    file: &Path,
    tracked_fold: &HashMap<String, PathBuf>,
) -> bool {
    let key = crate::utils::path_case::fold_path_key(file.to_string_lossy().as_ref());
    tracked_fold.get(&key).is_some_and(|tracked| {
        crate::utils::path_case::is_same_file_case_alias(workdir, file, tracked)
    })
}

fn list_workdir_files_split_safe(workdir: &PathBuf) -> io::Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut files = Vec::new();
    let mut ignored = Vec::new();
    let mut pending_dirs = vec![workdir.clone()];

    while let Some(dir) = pending_dirs.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            // Always skip `.libra` (Libra metadata) and `.git` (like Git, which
            // hardcodes ignoring `.git`); neither is ever surfaced or staged.
            if entry.file_name() == std::ffi::OsStr::new(util::ROOT_DIR)
                || entry.file_name() == std::ffi::OsStr::new(util::GIT_DIR)
            {
                continue;
            }

            let file_type = entry.file_type()?;
            let relative = path
                .strip_prefix(workdir)
                .map_err(|err| io::Error::other(err.to_string()))?
                .to_path_buf();
            if file_type.is_dir() {
                if util::check_gitignore(workdir, &path) {
                    ignored.push(relative);
                } else {
                    pending_dirs.push(path);
                }
            } else if file_type.is_file() || file_type.is_symlink() {
                if util::check_gitignore(workdir, &path) {
                    ignored.push(relative);
                } else {
                    files.push(relative);
                }
            }
        }
    }

    Ok((files, ignored))
}

/// List workdir files with --force semantics: recurse into ignored directories
/// and include their files in the ignored list
fn list_workdir_files_split_force(workdir: &PathBuf) -> io::Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut files = Vec::new();
    let mut ignored = Vec::new();
    let mut pending_dirs = vec![workdir.clone()];

    while let Some(dir) = pending_dirs.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            // Always skip `.libra` (Libra metadata) and `.git` (like Git, which
            // hardcodes ignoring `.git`); `--force` must not stage `.git` either.
            if entry.file_name() == std::ffi::OsStr::new(util::ROOT_DIR)
                || entry.file_name() == std::ffi::OsStr::new(util::GIT_DIR)
            {
                continue;
            }

            let file_type = entry.file_type()?;
            let relative = path
                .strip_prefix(workdir)
                .map_err(|err| io::Error::other(err.to_string()))?
                .to_path_buf();
            if file_type.is_dir() {
                // Always recurse into directories, even ignored ones.
                // We never push the directory entry itself — only its files
                // — so `add --force` sees concrete blobs, not a path that
                // would panic when `Blob::from_file` tries to read it.
                pending_dirs.push(path.clone());
            } else if file_type.is_file() || file_type.is_symlink() {
                if util::check_gitignore(workdir, &path) {
                    ignored.push(relative);
                } else {
                    files.push(relative);
                }
            }
        }
    }

    Ok((files, ignored))
}

/// List ignored files (not tracked by index, but ignored by configured rules) under workdir
pub fn list_ignored_files() -> Result<Changes, StatusError> {
    changes_to_be_staged_with_policy(IgnorePolicy::OnlyIgnored)
}

#[cfg(test)]
mod test {
    use sea_orm::{ConnectionTrait, Statement};
    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        internal::db::{get_db_conn_instance, reset_db_conn_instance_for_path},
        utils::{
            error::StableErrorCode,
            test::{self, ChangeDirGuard},
        },
    };

    /// Pin the `Display` format for the static-message variants of
    /// [`StatusError`]. Only `InvalidPathEncoding` has a fully static
    /// pattern — the others are all source-chained (`{source}`) and
    /// owned by their wrapped error type, so they're intentionally
    /// skipped. The CliError mapping above prefixes "failed to determine
    /// working tree status: " in front of every variant before sending
    /// it to the human / --json envelope, so direct-Display matters
    /// less for this enum than for typed errors with more variants.
    #[test]
    fn status_error_display_pins_invalid_path_encoding_variant() {
        assert_eq!(
            StatusError::InvalidPathEncoding {
                path: PathBuf::from("src/foo"),
            }
            .to_string(),
            "path 'src/foo' is not valid UTF-8",
        );
    }

    #[test]
    fn short_format_surface_emits_all_seven_unmerged_xy_codes() {
        use crate::command::unmerged::{UnmergedEntry, UnmergedStage};

        let hash = ObjectHash::new(&[0u8; 20]);
        let mk = |name: &str, stages: [bool; 3]| {
            let stage = |present: bool| {
                present.then_some(UnmergedStage {
                    mode: 0o100644,
                    hash,
                })
            };
            UnmergedEntry::new(
                PathBuf::from(name),
                [stage(stages[0]), stage(stages[1]), stage(stages[2])],
            )
        };
        let unmerged = vec![
            mk("dd.txt", [true, false, false]),
            mk("au.txt", [false, true, false]),
            mk("ud.txt", [true, true, false]),
            mk("ua.txt", [false, false, true]),
            mk("du.txt", [true, false, true]),
            mk("aa.txt", [false, true, true]),
            mk("uu.txt", [true, true, true]),
        ];
        let empty = Changes::default();
        let rows = generate_short_format_status_with_unmerged(&empty, &empty, &unmerged);
        let by_path: std::collections::BTreeMap<_, _> = rows
            .into_iter()
            .map(|(path, x, y)| (path, (x, y)))
            .collect();
        assert_eq!(by_path.get(&PathBuf::from("dd.txt")), Some(&('D', 'D')));
        assert_eq!(by_path.get(&PathBuf::from("au.txt")), Some(&('A', 'U')));
        assert_eq!(by_path.get(&PathBuf::from("ud.txt")), Some(&('U', 'D')));
        assert_eq!(by_path.get(&PathBuf::from("ua.txt")), Some(&('U', 'A')));
        assert_eq!(by_path.get(&PathBuf::from("du.txt")), Some(&('D', 'U')));
        assert_eq!(by_path.get(&PathBuf::from("aa.txt")), Some(&('A', 'A')));
        assert_eq!(by_path.get(&PathBuf::from("uu.txt")), Some(&('U', 'U')));
    }

    #[test]
    fn list_workdir_files_prunes_ignored_directories() {
        let repo = tempdir().expect("failed to create temp repo");
        let workdir = repo.path().to_path_buf();
        std::fs::write(workdir.join(".libraignore"), "ignored-dir/\n")
            .expect("failed to write ignore file");
        std::fs::create_dir_all(workdir.join("ignored-dir/nested"))
            .expect("failed to create ignored directory");
        std::fs::write(workdir.join("ignored-dir/nested/file.txt"), "ignored")
            .expect("failed to write ignored file");
        std::fs::write(workdir.join("visible.txt"), "visible").expect("failed to write file");

        let (visible, ignored) =
            list_workdir_files_split_safe(&workdir).expect("failed to list workdir files");

        assert!(visible.contains(&PathBuf::from(".libraignore")));
        assert!(visible.contains(&PathBuf::from("visible.txt")));
        assert!(ignored.contains(&PathBuf::from("ignored-dir")));
        assert!(!visible.contains(&PathBuf::from("ignored-dir/nested/file.txt")));
        assert!(!ignored.contains(&PathBuf::from("ignored-dir/nested/file.txt")));
    }

    #[tokio::test]
    #[serial]
    async fn sequence_notice_surfaces_corrupt_sequence_kind() {
        let repo = tempdir().expect("failed to create temp repo");
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());
        let db_path = repo.path().join(".libra").join("libra.db");
        let db = get_db_conn_instance().await;
        db.execute(Statement::from_string(
            db.get_database_backend(),
            "INSERT INTO sequence_state \
             (worktree_id, kind, head_name, head_orig, current_oid, todo, payload) \
             VALUES ('', 'corrupt', 'main', 'a', 'b', '', '{}')",
        ))
        .await
        .expect("insert corrupt sequence row");

        let error = sequence_notice()
            .await
            .expect_err("corrupt sequence state must fail closed");
        assert_eq!(error.stable_code(), StableErrorCode::RepoStateInvalid);
        assert!(
            error
                .to_string()
                .contains("unknown sequence kind 'corrupt'")
        );

        reset_db_conn_instance_for_path(&db_path).await;
    }

    #[tokio::test]
    #[serial]
    async fn resolve_upstream_info_surfaces_branch_config_query_failures() {
        let repo = tempdir().expect("failed to create temp repo");
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());
        let db_path = repo.path().join(".libra").join("libra.db");

        let db = get_db_conn_instance().await;
        db.execute(Statement::from_string(
            db.get_database_backend(),
            "DROP TABLE config_kv",
        ))
        .await
        .expect("dropping config_kv table should succeed");

        let err = resolve_upstream_info(&Head::Branch("main".to_string()), None)
            .await
            .expect_err("missing config_kv table should surface as an error");

        assert_eq!(err.stable_code(), StableErrorCode::IoReadFailed);
        assert!(
            err.to_string()
                .contains("failed to read branch configuration for 'main'"),
            "unexpected error: {err}"
        );

        reset_db_conn_instance_for_path(&db_path).await;
    }
}
