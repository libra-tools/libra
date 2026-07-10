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
    merge, stash, status_untracked,
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

    /// Show ignored files
    #[clap(long = "ignored")]
    pub ignored: bool,

    /// Control untracked files display: `no`, `normal` (default), or `all`. As
    /// in Git, the short `-u`/long `--untracked-files` with no value means
    /// `all` (e.g. `-u`, `-uno`, `--untracked-files=all`).
    #[clap(
        short = 'u',
        long = "untracked-files",
        value_name = "MODE",
        num_args = 0..=1,
        default_value = "normal",
        default_missing_value = "all"
    )]
    pub untracked_files: UntrackedFiles,

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
}

impl StatusArgs {
    /// Whether ahead/behind counts should be shown in branch info.
    fn show_ahead_behind(&self) -> bool {
        !self.no_ahead_behind
    }
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
    porcelain_v2: Option<PorcelainV2Data>,
}

/// Human advisory for a non-merge sequence in progress (read-only detection).
async fn sequence_notice() -> Option<String> {
    use crate::internal::sequencer::{self, SequenceKind};
    match sequencer::detect_active().await.ok().flatten() {
        Some(SequenceKind::CherryPick) => Some(
            "cherry-pick in progress; run 'libra cherry-pick --continue' or '--abort'".to_string(),
        ),
        Some(SequenceKind::Revert) => {
            Some("revert in progress; run 'libra revert --continue' or '--abort'".to_string())
        }
        Some(SequenceKind::Rebase) => {
            Some("rebase in progress; run 'libra rebase --continue' or '--abort'".to_string())
        }
        // Merge has its own dedicated rendering below.
        Some(SequenceKind::Merge) | None => None,
    }
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
async fn collect_status_data(args: &StatusArgs) -> CliResult<StatusData> {
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
        args.untracked_files,
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

    // Resolve rename detection: `--no-renames` wins (off); otherwise `--renames`
    // (or `--find-renames`) enables it at the given threshold (default 50).
    let rename_threshold = if args.no_renames {
        None
    } else if args.renames || args.find_renames.is_some() {
        Some(args.find_renames.unwrap_or(50))
    } else {
        None
    };

    // Apply rename detection before collapsing untracked dirs / porcelain metadata.
    if let Some(threshold) = rename_threshold
        && threshold > 0
    {
        detect_renames_in_changes(&mut staged, threshold, head_oid.as_ref()).await?;
        detect_renames_in_changes(&mut unstaged, threshold, head_oid.as_ref()).await?;
    }

    let stash_count = if args.show_stash {
        Some(stash::get_stash_num().unwrap_or(0))
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
        Some(build_porcelain_v2_data(index, head_oid.as_ref()))
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
        sequence_notice: sequence_notice().await,
        sparse_view_active: crate::internal::sparse::SparseView::load()
            .await
            .is_active(),
        porcelain_v2,
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
    changes.renamed.retain(|(old, new)| {
        current_relative_matches(old, pathspecs) || current_relative_matches(new, pathspecs)
    });
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

/// Detect renames between deleted and new files in `changes`.
///
/// Matches are selected greedily by best similarity score. A file may only
/// participate in one rename pair. The threshold is a percentage (0-100).
async fn detect_renames_in_changes(
    changes: &mut Changes,
    threshold: u8,
    head_oid: Option<&ObjectHash>,
) -> CliResult<()> {
    if changes.deleted.is_empty() || changes.new.is_empty() {
        return Ok(());
    }

    let head_blobs = head_oid.map(load_head_tree_blobs).unwrap_or_default();

    // Pre-compute blob hashes for new files.
    let mut new_hashes: HashMap<usize, ObjectHash> = HashMap::new();
    for (idx, path) in changes.new.iter().enumerate() {
        let abs = util::workdir_to_absolute(path);
        if let Ok(hash) = calc_file_blob_hash(&abs) {
            new_hashes.insert(idx, hash);
        }
    }

    let mut used_new: HashSet<usize> = HashSet::new();
    let mut remaining_deleted: Vec<PathBuf> = Vec::new();
    let mut renamed: Vec<(PathBuf, PathBuf)> = Vec::new();

    for deleted in &changes.deleted {
        let deleted_name = file_name_lossy(deleted);
        let deleted_head_blob = head_blobs.get(deleted).cloned();

        let mut best: Option<(usize, u8)> = None;
        for (idx, new_path) in changes.new.iter().enumerate() {
            if used_new.contains(&idx) {
                continue;
            }
            let score = if deleted_head_blob
                .as_ref()
                .zip(new_hashes.get(&idx))
                .is_some_and(|(a, b)| a == b)
            {
                100
            } else {
                let new_name = file_name_lossy(new_path);
                filename_similarity(&deleted_name, &new_name)
            };
            if score >= threshold {
                match best {
                    None => best = Some((idx, score)),
                    Some((_, current)) if score > current => best = Some((idx, score)),
                    _ => {}
                }
            }
        }

        if let Some((idx, _)) = best {
            used_new.insert(idx);
            renamed.push((deleted.clone(), changes.new[idx].clone()));
        } else {
            remaining_deleted.push(deleted.clone());
        }
    }

    let mut remaining_new: Vec<PathBuf> = changes
        .new
        .iter()
        .enumerate()
        .filter(|(idx, _)| !used_new.contains(idx))
        .map(|(_, p)| p.clone())
        .collect();

    // Sort both remaining lists to preserve deterministic output.
    remaining_deleted.sort();
    remaining_new.sort();
    renamed.sort_by(|a, b| a.1.cmp(&b.1));

    changes.deleted = remaining_deleted;
    changes.new = remaining_new;
    changes.renamed.extend(renamed);
    Ok(())
}

fn load_head_tree_blobs(head_oid: &ObjectHash) -> HashMap<PathBuf, ObjectHash> {
    let commit = Commit::load(head_oid);
    let tree = Tree::load(&commit.tree_id);
    tree.get_plain_items_with_mode()
        .into_iter()
        .map(|(path, hash, _mode)| (path, hash))
        .collect()
}

fn file_name_lossy(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// Simple filename similarity based on longest common subsequence length.
fn filename_similarity(a: &str, b: &str) -> u8 {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut prev = vec![0u16; b.len() + 1];
    let mut curr = vec![0u16; b.len() + 1];
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            if a[i - 1] == b[j - 1] {
                curr[j] = prev[j - 1] + 1;
            } else {
                curr[j] = curr[j - 1].max(prev[j]);
            }
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    let lcs = prev[b.len()] as usize;
    let max_len = a.len().max(b.len());
    (lcs.saturating_mul(100) / max_len.max(1)).min(100) as u8
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

    let args = StatusArgs::default();
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

    let data = collect_status_data(&args).await?;
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
pub async fn execute_safe(args: StatusArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    // Dirty-set cache modes (lore.md 1.1). NOTE: only this CLI entry routes
    // them — the legacy `execute_to` writer entry ignores the flags (its
    // callers never set them).
    if args.scan {
        return run_status_scan(&args, output).await;
    }
    if args.cached || args.check_dirty {
        return run_status_cache_mode(&args, output).await;
    }

    let data = collect_status_data(&args).await?;

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
async fn run_status_scan(args: &StatusArgs, output: &OutputConfig) -> CliResult<()> {
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
    let result = run_status_scan_locked(args, output, &index_path).await;
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
    let data = collect_status_data(args).await?;

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
async fn run_status_cache_mode(args: &StatusArgs, output: &OutputConfig) -> CliResult<()> {
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
        let data = collect_status_data(args).await?;
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
        let data = collect_status_data(args).await?;
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
    let unstaged = unstaged.to_relative();
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
        stash_count: None,
        upstream,
        merge_state,
        sequence_notice: sequence_notice().await,
        sparse_view_active: crate::internal::sparse::SparseView::load()
            .await
            .is_active(),
        porcelain_v2: None,
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
pub async fn execute_to(args: StatusArgs, writer: &mut impl Write) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    let data = collect_status_data(&args).await?;
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
                data.porcelain_v2.as_ref(),
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
    let status_list = generate_short_format_status_with_unmerged(staged, unstaged, unmerged);
    let write_err = |e: io::Error| CliError::io(format!("failed to write status output: {e}"));
    for (file, staged_status, unstaged_status) in status_list {
        write!(
            writer,
            "{}{} {}",
            staged_status,
            unstaged_status,
            file.display()
        )
        .map_err(write_err)?;
        if null_terminated {
            writer.write_all(b"\0").map_err(write_err)?;
        } else {
            writer.write_all(b"\n").map_err(write_err)?;
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

/// Output porcelain v2 format using metadata collected during status computation.
fn output_porcelain_v2(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
    ignored: &[PathBuf],
    metadata: Option<&PorcelainV2Data>,
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

    let status_list = generate_short_format_status(staged, unstaged);
    for (file, staged_status, unstaged_status) in status_list {
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

fn generate_short_format_status_with_unmerged(
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

/// Short format output with color controlled by OutputConfig.
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

    let status_list = generate_short_format_status_with_unmerged(staged, unstaged, unmerged);

    for (file, staged_status, unstaged_status) in status_list {
        if use_colors {
            let colored_output = format_colored_status(staged_status, unstaged_status, &file);
            write!(writer, "{}", colored_output).map_err(write_err)?;
        } else {
            write!(
                writer,
                "{}{} {}",
                staged_status,
                unstaged_status,
                file.display()
            )
            .map_err(write_err)?;
        }
        if null_terminated {
            writer.write_all(b"\0").map_err(write_err)?;
        } else {
            writer.write_all(b"\n").map_err(write_err)?;
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
