//! Implements status reporting with ignore policy support, computing staged/unstaged/untracked sets and printing concise summaries.

use std::{
    collections::{HashMap, HashSet},
    io,
    io::Write,
    path::{Path, PathBuf},
};

use clap::{Parser, ValueEnum};
use git_internal::{
    errors::GitError,
    hash::ObjectHash,
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
        shallow::ShallowSet,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        ignore::IgnorePolicy,
        object_ext::{CommitExt, TreeExt},
        output::{OutputConfig, emit_json_data},
        path,
        pathspec::{PathspecError, PathspecSet},
        util,
    },
};

// StatusData stays here so scan, cache, and renderers share one result shape.
mod cache;
mod input;
mod output;
mod scan;

use cache::*;
pub use input::{InvocationWarningCtx, PorcelainVersion, StatusArgs, UntrackedFiles};
// Preserve crate-visible paths for existing callers and inline tests.
#[allow(unused_imports)]
pub(crate) use input::{RenameThresholdOccurrence, StatusFormatFlags};
pub(crate) use input::{
    ResolvedStatusArgs, StatusArgvResolution, normalize_status_argv, resolve_config_defaults,
    resolve_status_threshold,
};
use input::{StatusConfigExtras, apply_status_config_defaults};
pub(crate) use output::io_blocked_reason_and_code;
use output::render_status_to_writer;
// Output renderers moved to `output`; keep the crate-visible `status::` paths
// for existing callers, sibling modules that glob-import `super::*`, and tests.
pub use output::{
    PorcelainV2Data, ShortStatusEntry, StatusOutcome, build_porcelain_v2_data, build_status_json,
    cache_warning, current_to_workdir, deliver_warnings_stderr, generate_short_format_status,
    generate_short_status_entries, index_mode_to_tree_item_mode, is_gitlink_index_entry,
    output_porcelain, output_short_format, quote_pathname, quote_pathname_bytes, raw_path_base64,
    raw_path_sort_key,
};
#[allow(unused_imports)]
pub(crate) use output::{
    generate_short_format_status_with_unmerged, generate_short_status_entries_with_unmerged,
};
#[allow(unused_imports)]
pub(crate) use scan::head_object_unreadable;
use scan::*;
pub(crate) use scan::{load_head_commit_tree, load_status_index};

// ---------------------------------------------------------------------------
// Shared warnings and result data
// ---------------------------------------------------------------------------

/// Structured status degradation warning (§B.5; reused verbatim by the JSON
/// `data.warnings[]` array). Human/short/porcelain modes render these on
/// stderr; JSON never writes them to stderr.
#[derive(Clone, Debug, serde::Serialize)]
pub struct StatusWarning {
    pub code: StatusWarningCode,
    pub message: String,
    pub source: StatusWarningSource,
}

/// Declare a warning enum together with its `ALL` registry from ONE
/// variant list: a variant cannot exist without appearing in `ALL`, so
/// the schema-snapshot and doc-closeout guards cannot be left green by a
/// forgotten registry entry (2026-08-06 R0-7 review; the old hand-written
/// `ALL` was a silently-driftable duplicate).
macro_rules! declare_status_warning_enum {
    (
        $(#[$enum_meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident $(= $discriminant:literal)?
            ),+ $(,)?
        }
    ) => {
        $(#[$enum_meta])*
        $vis enum $name {
            $(
                $(#[$variant_meta])*
                $variant $(= $discriminant)?
            ),+
        }

        impl $name {
            /// Every variant, generated from the single declaration —
            /// exhaustive by construction.
            $vis const ALL: &'static [$name] = &[$($name::$variant),+];
        }
    };
}

declare_status_warning_enum! {
    /// Stable warning codes (§B.5), declared in the order the user-facing
    /// warning table lists them (`compat_r0_9_doc_closeout` walks `ALL` to
    /// assert the docs stay in sync, so a code added here without a doc
    /// row fails the build rather than shipping undocumented).
    /// Serialization names are pinned by `json_warnings_schema_snapshot`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
    #[serde(rename_all = "snake_case")]
    #[non_exhaustive]
    // Explicit discriminants preserve the ORIGINAL pre-macro declaration's
    // numeric values: this is a public fieldless enum embedders may cast,
    // and the table-ordered redeclaration must not silently renumber it
    // (2026-08-06 R0-7 review).
    pub enum StatusWarningCode {
        RenameLimitProductSkipped = 1,
        SimilarityBudgetExceeded = 0,
        /// §B.3.2: the rename-destination probe tripped a budget; partial
        /// destinations still pair, but detection may be incomplete.
        ProbeTruncated = 2,
        /// §B.6.1: a non-UTF-8 path was skipped as a rename candidate (its
        /// base D/A/`??` rows are unaffected).
        RenamePathEncodingUnsupported = 9,
        /// §B.3.4: repository-object reads for inexact scoring were skipped
        /// (missing/corrupt/unavailable objects); affected candidates
        /// dropped.
        MetadataUnavailable = 3,
        /// §B.3.4: an OBJECT read budget (per-object size cap, byte total,
        /// slot count, or deadline) was hit; affected candidates dropped,
        /// detection may be incomplete. The worktree-side equivalent is
        /// [`StatusWarningCode::WorktreeBudgetExceeded`] — the two are
        /// separate because `source` distinguishes `metadata` from
        /// `worktree`.
        MetadataBudgetExceeded = 4,
        /// §B.3.4: a WORKTREE read budget (per-file size cap, byte total,
        /// or task count) was hit during optional rename content reads;
        /// affected candidates dropped, detection may be incomplete.
        WorktreeBudgetExceeded = 5,
        /// §B.3.3: a worktree read failed (I/O) during optional rename
        /// content reads; the affected candidate was dropped.
        WorktreeReadFailed = 6,
        /// §B.6.0.1 reason taxonomy (R0-8 io_blocked contract).
        WorktreePermissionDenied = 7,
        /// §B.6.0.1 reason taxonomy (R0-8 io_blocked contract).
        WorktreeIoTimeout = 8,
        DirtyCacheLockStolen = 10,
        DirtyCacheStaleFallback = 11,
        DirtyCacheConcurrentInvalidate = 12,
        /// A path could not be encoded for the dirty cache (a non-UTF-8
        /// name), so its row was omitted from the snapshot. The base
        /// status is unaffected; `--cached` simply will not list that
        /// path.
        DirtyCachePathUnencodable = 13,
        /// A repository-level PREFLIGHT advisory raised before the command
        /// ran (e.g. a pending durable object-index repair). Carried in
        /// `warnings[]` so `--exit-code-on-warning` can never return 9
        /// with an empty structured list — §B.5 forbids a stderr-only
        /// channel.
        RepositoryPreflight = 14,
        /// #486: the upstream ahead/behind counts could not be computed (a
        /// commit in either history, or the shallow boundary list, could not
        /// be read). The counts are omitted — never guessed — and the rest of
        /// status is unaffected.
        UpstreamCountsUnavailable = 15,
    }
}

impl StatusWarningCode {
    /// The subsystem a code is ALWAYS emitted under (§B.5 table). Every
    /// emit site derives its `source` from here instead of repeating the
    /// pairing, so the code→source mapping has exactly one definition and
    /// the published table can be checked against it.
    pub fn source(self) -> StatusWarningSource {
        match self {
            StatusWarningCode::ProbeTruncated => StatusWarningSource::Probe,
            StatusWarningCode::SimilarityBudgetExceeded
            | StatusWarningCode::RenameLimitProductSkipped
            | StatusWarningCode::RenamePathEncodingUnsupported => StatusWarningSource::RenameDetect,
            StatusWarningCode::MetadataUnavailable
            | StatusWarningCode::MetadataBudgetExceeded
            | StatusWarningCode::UpstreamCountsUnavailable => StatusWarningSource::Metadata,
            StatusWarningCode::WorktreeBudgetExceeded
            | StatusWarningCode::WorktreeReadFailed
            | StatusWarningCode::WorktreePermissionDenied
            | StatusWarningCode::WorktreeIoTimeout => StatusWarningSource::Worktree,
            StatusWarningCode::DirtyCacheLockStolen
            | StatusWarningCode::DirtyCacheStaleFallback
            | StatusWarningCode::DirtyCacheConcurrentInvalidate
            | StatusWarningCode::DirtyCachePathUnencodable => StatusWarningSource::Cache,
            StatusWarningCode::RepositoryPreflight => StatusWarningSource::Config,
        }
    }
}

declare_status_warning_enum! {
    /// Which subsystem produced a warning (§B.5).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
    #[serde(rename_all = "snake_case")]
    #[non_exhaustive]
    pub enum StatusWarningSource {
        /// Repository-level advisories not tied to a scan — currently
        /// `repository_preflight`. Config RESOLUTION itself never warns: an
        /// invalid value fails closed instead of degrading.
        Config,
        /// The bounded rename-destination worktree probe (§B.3.2). Distinct
        /// from `RenameDetect` on purpose: a truncated probe means "we may
        /// not have SEEN every candidate", while a `RenameDetect` warning
        /// means "we saw them but could not score them". Consumers act
        /// differently on the two (re-run narrower vs. accept the pairing).
        Probe,
        RenameDetect,
        Cache,
        /// Repository-object reads (§B.3.4 metadata side).
        Metadata,
        /// Worktree reads (§B.3.3/§B.3.4 worktree side).
        Worktree,
    }
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
    #[error("cannot read the HEAD {what} '{oid}': the object is missing or corrupt")]
    HeadObjectUnreadable { what: &'static str, oid: String },
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
            StatusError::HeadObjectUnreadable { .. } => CliError::fatal(msg)
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("run 'libra fsck' or restore the object, then retry"),
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
    /// Commits ahead of upstream (None when gone, on an unborn branch, or when
    /// the counts cannot be computed)
    pub ahead: Option<usize>,
    /// Commits behind upstream (None in the same cases as `ahead`)
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
pub struct StatusData {
    head: Head,
    head_oid: Option<ObjectHash>,
    has_commits: bool,
    staged: Changes,
    unstaged: Changes,
    /// WT-06 (ADR-WT-05): index stage-0 entries carrying the v3
    /// `intent_to_add` extended flag whose working-tree file exists. Git
    /// renders these as the unstaged worktree side of a new file (` A` /
    /// `new file:`), never as a staged `A` row: the index holds only the empty
    /// blob placeholder. Paths are in the same display base as `unstaged`.
    intent_to_add: Vec<PathBuf>,
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
    /// Structured degradation warnings collected during data assembly
    /// (§B.5): rename-engine budget/limit downgrades. Rendered per the
    /// delivery matrix by the callers.
    warnings: Vec<StatusWarning>,
    /// `core.quotePath` (§B.6.6): escape non-ASCII bytes in human-short and
    /// non-`-z` porcelain paths (default true, Git parity).
    quote_path: bool,
    /// §B.3.3/§B.6.0.1: paths the base scan or the rename probe could not
    /// inspect (workdir-relative, sorted, deduplicated). Text formats fail
    /// closed on any entry; JSON reports the partial result plus
    /// `data.io_blocked[]` with `is_clean = false`.
    io_blocked: Vec<crate::command::status_probe::IoBlockedEvent>,
    /// WT-06 follow-up (t7508): the current directory relative to the
    /// repository root. Display paths in `staged`/`unstaged`/`intent_to_add`
    /// are cwd-relative, but Git orders short/porcelain rows by the
    /// REPOSITORY-ROOT path. Renderers join this base before sorting (empty
    /// at the root, where display order and root order already agree).
    sort_base: PathBuf,
    /// ADR-FM-06 (#470 FM-05): legacy-materialization repair candidates —
    /// index 100755, working tree not executable, content identical. Drives
    /// the one human long-format hint only (short/porcelain/JSON unchanged).
    mode_fix_candidates: Vec<PathBuf>,
    /// Whether the RENAME side (probe or candidate reads) was blocked, as
    /// opposed to the base scan. `rename_detection_complete` keys off this
    /// so a base-scan-only block does not also claim the rename pairing
    /// degraded.
    rename_scan_blocked: bool,
    /// Whether the BASE scan (tracked dirty + untracked enumeration) hit an
    /// I/O block — `data.base_scan_complete` is its negation; probe blocks
    /// only affect `rename_detection_complete`.
    base_scan_blocked: bool,
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Collect repository status and render it inside the same `{ok, command,
/// data}` envelope that `libra status --json` prints, so `/api/repo/status`
/// stays byte-compatible with the CLI output.
///
/// Internally re-uses `collect_status_data` + [`build_status_json`] with a
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

    // Serialize concurrent API collections: interleaved collections against
    // the shared repository connection have produced transient read errors
    // and — worse — silently inconsistent snapshots (a staged-deletion side
    // observed empty mid-interleave). The API serves one same-cwd repository,
    // so a process-wide mutex is the correct, cheap consistency guarantee.
    static API_STATUS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _serialized = API_STATUS_LOCK.lock().await;

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
    // An API request inherits NOTHING from the process: a long-running
    // server's preflight buffer is not this request's warning list.
    let data = collect_status_data(&args, extras, &InvocationWarningCtx::empty()).await?;
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
    execute_safe_with_resolution(args, output, None).await
}

/// CLI entry with the pre-clap argv resolution (§B.4.3): the occurrence list
/// overrides the legacy percent field so all three rename spellings obey
/// true last-one-wins.
pub(crate) async fn execute_safe_with_resolution(
    mut args: StatusArgs,
    output: &OutputConfig,
    resolution: Option<&StatusArgvResolution>,
) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    // Fail closed on invalid `status.*` config before any mode runs or any
    // output is produced; CLI flags keep precedence inside the resolver.
    let mut extras = apply_status_config_defaults(&mut args).await?;
    // ADR-FM-04 K6: an invalid `core.fileMode` fails status closed (no output,
    // zero writes) with the commit.verbose mapping.
    let _ = crate::internal::config::core_file_mode().await?;
    crate::command::status::warn_sparse_checkout_unsupported_once().await;

    if let Some(resolution) = resolution {
        extras.rename_threshold = resolve_status_threshold(&args, Some(resolution))?;
        // The argv scan and clap must agree about which format flags were
        // GIVEN. The scan interprets short clusters from the subcommand's own
        // arity table (so `-uno` is `-u=no`, and its letters are not flags);
        // if it ever disagreed with clap, the disagreement would show up as
        // silently wrong output — NUL separators that were never asked for,
        // or a porcelain format the user did not select. It fails closed
        // instead. The implication is one-directional on purpose: config can
        // set `short` with nothing in argv, but argv can never set something
        // clap did not see.
        resolution.format.ensure_agrees_with(&args)?;
    }
    let args = args;

    // Dirty-set cache modes (lore.md 1.1). NOTE: only this CLI entry routes
    // them — the legacy `execute_to` writer entry ignores the flags (its
    // callers never set them).
    //
    // Part C W1 (§C.4.1.1): the dirty cache is worktree-scoped, so the
    // cache-semantic modes run in any worktree against their own rows.
    if args.scan {
        return run_status_scan(&args, extras, output).await;
    }
    if args.cached || args.check_dirty {
        return run_status_cache_mode(&args, extras, output).await;
    }

    let data = collect_status_data(
        &args,
        extras,
        &InvocationWarningCtx::from_process_preflight(),
    )
    .await?;

    if output.is_json() {
        let json_data = build_status_json(&data, &args);
        // A non-EPIPE stdout failure must not swallow the collected
        // warnings: JSON is their only channel, so they fall back to stderr
        // rather than vanishing with the envelope.
        if let Err(error) = emit_json_data("status", &json_data, output) {
            if !error.is_silent() {
                deliver_warnings_stderr(&data.warnings);
            }
            return Err(error);
        }
    } else {
        // §B.5 delivery matrix: stderr warnings even under `--quiet`
        // (quiet suppresses the body, never diagnostics).
        deliver_warnings_stderr(&data.warnings);
        // Fail closed BEFORE the quiet check: suppressing the body must not
        // suppress the "could not inspect" verdict.
        fail_closed_on_io_blocked(&data, output)?;
        if !output.quiet {
            let mut stdout = std::io::stdout();
            render_status_to_writer(&data, &args, output, &mut stdout).await?;
        }
    }

    // §B.5 exit arbitration: warnings + --exit-code-on-warning (9) beats
    // the --exit-code dirty exit (1); JSON gets the same silent 9 without a
    // second stderr envelope.
    StatusOutcome::new(&data, &args).resolve(output)?;

    Ok(())
}

// ─── Dirty-set cache modes (lore.md §1.1) ───────────────────────────────────

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
    // This writer entry renders a plain full status. It does NOT implement
    // the dirty-cache modes or the exit arbitration (it returns `()`, not an
    // exit code), so silently ignoring those options would hand an embedder
    // an ordinary status while it believed it had asked for a cache read or
    // a dirty exit code. Refuse instead — a caller that needs them must go
    // through `execute_safe_with_resolution`.
    let unsupported: &[(&str, bool)] = &[
        ("--scan", args.scan),
        ("--cached", args.cached),
        ("--check-dirty", args.check_dirty),
        ("--exit-code", args.exit_code),
    ];
    if let Some((flag, _)) = unsupported.iter().find(|(_, set)| *set) {
        return Err(CliError::command_usage(format!(
            "'{flag}' is not supported by the status writer entry point"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint("run the status command itself; this entry renders a plain full status"));
    }
    let data = collect_status_data(
        &args,
        extras,
        &InvocationWarningCtx::from_process_preflight(),
    )
    .await?;
    deliver_warnings_stderr(&data.warnings);
    let output = OutputConfig::default();
    fail_closed_on_io_blocked(&data, &output)?;
    render_status_to_writer(&data, &args, &output, writer).await
}

// ---------------------------------------------------------------------------
// Rendering dispatcher
// ---------------------------------------------------------------------------

/// §B.3.3/§B.6.0.1 delivery matrix: human/short/porcelain fail CLOSED on any
/// I/O-blocked path — no partial dirty/porcelain body is ever printed. Only
/// JSON/API may report the partial result (`io_blocked[]`).
///
/// This lives OUTSIDE the renderer because `--quiet` skips rendering: routing
/// the guard through the writer would let `libra --quiet status` exit 0 on a
/// repository it could not fully inspect, which is precisely the silent
/// "looks clean" answer the contract exists to prevent.
/// Collapse a SORTED `io_blocked[]` to one entry per path.
///
/// A path survives as `absorbed` only if EVERY report of it was absorbed.
/// The same unreadable directory can be compensated for by one consumer and
/// not another — an untracked scan emits its `?? dir/` marker regardless,
/// while rename detection genuinely could not see what was inside it. A
/// plain dedup keeps whichever event happened to be recorded first, so an
/// absorbed report could silently downgrade a real omission and stop the
/// command failing closed.
fn collapse_io_blocked_by_path(events: &mut Vec<crate::command::status_probe::IoBlockedEvent>) {
    let mut collapsed: Vec<crate::command::status_probe::IoBlockedEvent> =
        Vec::with_capacity(events.len());
    for event in events.drain(..) {
        match collapsed.last_mut() {
            Some(last) if last.path == event.path => last.absorbed &= event.absorbed,
            _ => collapsed.push(event),
        }
    }
    *events = collapsed;
}

fn fail_closed_on_io_blocked(data: &StatusData, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return Ok(());
    }
    // §B.6.0.1: in text modes ANY blocked path is fatal — including an
    // absorbed one. An unreadable untracked directory gets its `?? dir/`
    // marker emitted (over-reporting, the safe direction), but a marker is
    // not an inspection result, so the command still fails closed rather
    // than claim a complete status.
    let Some(first) = data.io_blocked.first() else {
        return Ok(());
    };
    let count = data.io_blocked.len();
    Err(CliError::fatal(format!(
        "cannot inspect '{}' ({count} path(s) blocked); status output would be incomplete",
        quote_pathname(&first.path, data.quote_path),
    ))
    .with_stable_code(StableErrorCode::IoReadFailed)
    .with_hint("fix the unreadable path permissions and retry")
    .with_hint("use --json to inspect the partial result with data.io_blocked[]"))
}

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

/// When the ahead/behind counts cannot be computed, the reason is pushed onto
/// `warnings` as an `upstream_counts_unavailable` warning — this invocation's
/// structured list, never the process-wide warning tracker (§B.4.3).
async fn resolve_upstream_info(
    head: &Head,
    local_commit: Option<&ObjectHash>,
    warnings: &mut Vec<StatusWarning>,
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
    let remote_ref_display = if remote == "." {
        merge_branch.clone()
    } else {
        format!("{remote}/{merge_branch}")
    };

    // Tracking refs are stored under their fully-qualified
    // `refs/remotes/<remote>/<branch>` name (clone/fetch/push writers), so the
    // lookup must use that name — a short-name query never matches and made
    // every fresh clone report "upstream is gone" (#464). The short-name probe
    // is kept as a fallback for repositories written before the
    // fully-qualified convention.
    let tracking_branch = if remote == "." {
        Branch::find_branch_result(merge_branch, None)
            .await
            .map_err(|error| status_branch_store_error("resolve upstream branch", error))?
    } else {
        let tracking_full_ref = format!("refs/remotes/{remote}/{merge_branch}");
        let tracking_branch = Branch::find_branch_result(&tracking_full_ref, Some(remote))
            .await
            .map_err(|error| status_branch_store_error("resolve upstream branch", error))?;
        match tracking_branch {
            Some(branch) => Some(branch),
            None => Branch::find_branch_result(merge_branch, Some(remote))
                .await
                .map_err(|error| status_branch_store_error("resolve upstream branch", error))?,
        }
    };

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

    let (ahead, behind) = match upstream_ahead_behind(local_commit, &tracking_commit) {
        Ok((ahead, behind)) => (Some(ahead), Some(behind)),
        Err(reason) => {
            let code = StatusWarningCode::UpstreamCountsUnavailable;
            warnings.push(StatusWarning {
                code,
                message: format!(
                    "cannot count commits ahead/behind '{remote_ref_display}': {reason}"
                ),
                source: code.source(),
            });
            (None, None)
        }
    };

    Ok(Some(UpstreamInfo {
        remote_ref: remote_ref_display,
        ahead,
        behind,
        gone: false,
    }))
}

/// Count how far `local` and `upstream` have diverged for the tracking segment
/// of `status` and `branch -vv`, as `(ahead, behind)`.
///
/// Delegates to [`crate::internal::merge_base::ahead_behind`], the painting
/// shared with merge-base, with the repository's shallow boundaries treated as
/// roots. `Err` carries a human-readable reason when the counts cannot be known
/// (unreadable shallow metadata, or a commit in either history that cannot be
/// loaded); callers then show no counts instead of a guessed number.
pub(crate) fn upstream_ahead_behind(
    local: &ObjectHash,
    upstream: &ObjectHash,
) -> Result<(usize, usize), String> {
    if local == upstream {
        return Ok((0, 0));
    }
    let shallow = ShallowSet::load().map_err(|error| error.to_string())?;
    crate::internal::merge_base::ahead_behind(local, upstream, shallow.oids())
        .map(|counts| (counts.ahead, counts.behind))
        .map_err(|error| error.to_string())
}

// ---------------------------------------------------------------------------
// Bare repository detection
// ---------------------------------------------------------------------------

/// Shared-parser `core.bare` read (all git boolean spellings). FAILS CLOSED:
/// an unparseable value or a config read failure refuses status rather than
/// silently proceeding into worktree-status collection on a bare repository.
async fn is_bare_repository() -> CliResult<bool> {
    match ConfigKv::get("core.bare").await {
        Ok(Some(entry)) => crate::internal::config::parse_git_bool(&entry.value).ok_or_else(|| {
            CliError::fatal(format!(
                "invalid core.bare value '{}': expected true/false/yes/no/on/off/1/0",
                entry.value
            ))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
        }),
        Ok(None) => Ok(false),
        Err(error) => Err(CliError::fatal(format!(
            "cannot read core.bare to classify this repository: {error}"
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)),
    }
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
        // Component-wise prefix check, never a `display()` string: a
        // non-UTF-8 directory name must not be flattened (U+FFFD would
        // break the comparison AND corrupt the marker).
        let has_tracked_files = index.tracked_files().iter().any(|f| f.starts_with(&dir));

        if has_tracked_files {
            for file in files {
                result.insert(file);
            }
        } else {
            // The marker is `<dir>/` built on the RAW name (see
            // `status_untracked_paths::directory_marker`).
            let mut marker = dir.into_os_string();
            marker.push("/");
            result.insert(PathBuf::from(marker));
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
    // WT-06/WT-07 (ADR-WT-05): an intent-to-add entry is an empty-blob
    // PLACEHOLDER for a new file, not staged content. Git never reports it as
    // `A ` and never writes it into a tree; excluding it here is the single
    // staged-side rule every consumer (status, commit, rm) shares.
    let intent_to_add: HashSet<PathBuf> = index
        .tracked_entries(0)
        .into_iter()
        .filter(|entry| entry.flags.intent_to_add)
        .map(|entry| PathBuf::from(&entry.name))
        .collect();
    let tracked_files: Vec<PathBuf> = index
        .tracked_files()
        .into_iter()
        .filter(|path| !intent_to_add.contains(path))
        .collect();

    if head_commit.is_none() {
        changes.new = tracked_files;
        return Ok(changes);
    }

    let head_commit = match head_commit {
        Some(head_commit) => head_commit,
        None => return Ok(changes),
    };
    let commit =
        Commit::try_load(&head_commit).ok_or_else(|| StatusError::HeadObjectUnreadable {
            what: "commit",
            oid: head_commit.to_string(),
        })?;
    let tree =
        Tree::try_load(&commit.tree_id).ok_or_else(|| StatusError::HeadObjectUnreadable {
            what: "tree",
            oid: commit.tree_id.to_string(),
        })?;
    let tree_files = tree.get_plain_items_with_mode();

    for (item_path, item_hash, item_mode) in tree_files.iter() {
        // §B.6.1: a tree path that is not valid UTF-8 cannot match an index
        // key, so it is skipped rather than made fatal.
        let Some(item_str) = item_path.to_str() else {
            continue;
        };
        // WT-06 (t2203:18): Git treats an intent-to-add placeholder as ABSENT
        // on the index side. If HEAD has the path, that is a staged DELETION
        // (`D` in column 1) even though stage 0 holds the empty blob; the
        // worktree side then supplies the `A` (`DA` when the file exists).
        if intent_to_add.contains(item_path) {
            changes.deleted.push(item_path.clone());
        } else if index.tracked(item_str, 0) {
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
    let (mut visible, ignored) = changes_to_be_staged_split_with_index(
        &workdir,
        &index,
        ignore_case,
        default_core_file_mode(),
    )?;
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
    changes_to_be_staged_split_safe_with_ignore_case_and_file_mode(
        ignore_case,
        default_core_file_mode(),
    )
}

/// [`changes_to_be_staged_split_safe_with_ignore_case`] with an explicit
/// `core.fileMode` value (FM-04): the callers already resolved the config.
pub(crate) fn changes_to_be_staged_split_safe_with_ignore_case_and_file_mode(
    ignore_case: bool,
    file_mode: bool,
) -> Result<(Changes, Changes), StatusError> {
    let workdir = util::try_working_dir().map_err(|source| StatusError::Workdir { source })?;
    let index_path = path::try_index().map_err(|source| StatusError::Workdir { source })?;
    let index = Index::load(&index_path).map_err(|source| StatusError::IndexLoad {
        path: index_path.clone(),
        source,
    })?;
    changes_to_be_staged_split_with_index(&workdir, &index, ignore_case, file_mode)
}

/// `changes_to_be_staged` with an explicit `core.fileMode` value (FM-04).
pub fn changes_to_be_staged_with_file_mode(file_mode: bool) -> Result<Changes, StatusError> {
    let workdir = util::try_working_dir().map_err(|source| StatusError::Workdir { source })?;
    let ignore_case = effective_ignore_case_for_workdir(&workdir)?;
    let index_path = path::try_index().map_err(|source| StatusError::Workdir { source })?;
    let index = Index::load(&index_path).map_err(|source| StatusError::IndexLoad {
        path: index_path.clone(),
        source,
    })?;
    let (visible, _) =
        changes_to_be_staged_split_with_index(&workdir, &index, ignore_case, file_mode)?;
    Ok(visible)
}

/// Platform default for `core.fileMode` when the config value is not resolved
/// by a command entry: Unix enables mode comparison, other platforms do not.
fn default_core_file_mode() -> bool {
    cfg!(unix)
}

/// Owner-execute bit of a worktree file (false on platforms without POSIX
/// permission bits).
fn worktree_exec_bit(metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

/// Emit the one-time `core.sparseCheckout=true` unsupported warning.
///
/// Libra honors the skip-worktree index bit but does not evaluate sparse
/// patterns, so a repository that turns sparse checkout on gets one warning
/// per process (ADR-SW-01 item 3).
pub(crate) async fn warn_sparse_checkout_unsupported_once() {
    use std::sync::OnceLock;
    static WARNED: OnceLock<()> = OnceLock::new();
    if WARNED.get().is_some() {
        return;
    }
    let Ok(Some(entry)) = ConfigKv::get("core.sparseCheckout").await else {
        return;
    };
    if crate::internal::config::parse_git_bool(&entry.value) == Some(true) {
        let _ = WARNED.set(());
        eprintln!(
            "warning: core.sparseCheckout=true is not supported; Libra honors the \
             skip-worktree index bit only"
        );
    }
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
    changes_to_be_staged_split_force_with_ignore_case_and_file_mode(
        ignore_case,
        default_core_file_mode(),
    )
}

/// [`changes_to_be_staged_split_force_with_ignore_case`] with an explicit
/// `core.fileMode` value (FM-04).
pub(crate) fn changes_to_be_staged_split_force_with_ignore_case_and_file_mode(
    ignore_case: bool,
    file_mode: bool,
) -> Result<(Changes, Changes), StatusError> {
    let workdir = util::try_working_dir().map_err(|source| StatusError::Workdir { source })?;
    let index_path = path::try_index().map_err(|source| StatusError::Workdir { source })?;
    let index = Index::load(&index_path).map_err(|source| StatusError::IndexLoad {
        path: index_path.clone(),
        source,
    })?;
    changes_to_be_staged_split_force_with_index(&workdir, &index, ignore_case, file_mode)
}

fn changes_to_be_staged_split_force_with_index(
    workdir: &PathBuf,
    index: &Index,
    ignore_case: bool,
    file_mode: bool,
) -> Result<(Changes, Changes), StatusError> {
    let mut visible = Changes::default();
    let mut ignored = Changes::default();
    let tracked_files = index.tracked_files();
    let tracked_fold = tracked_files_by_fold(&tracked_files, ignore_case);
    for file in tracked_files.iter() {
        // §B.6.1: skip the keyed comparisons for an undecodable name rather
        // than failing the whole status (see `collect_tracked_worktree_changes`).
        let Some(file_str) = file.to_str() else {
            continue;
        };
        // Gitlinks have no working-tree blob to compare (see
        // `changes_to_be_staged_split_with_index`).
        if is_gitlink_index_entry(index, file_str) {
            match workdir.join(file).symlink_metadata() {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                    continue;
                }
                // Absent is the normal shape: Libra never checks a submodule out.
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) =>
                {
                    continue;
                }
                // A plain file or symlink is neither shape, so it stays visible.
                Ok(_) => {
                    visible.modified.push(file.clone());
                    continue;
                }
                // Anything else is a real worktree read failure: fall through to
                // the ordinary handling below rather than reporting clean.
                Err(_) => {}
            }
        }
        let file_abs = workdir.join(file);
        match file_abs.symlink_metadata() {
            Err(_) => visible.deleted.push(file.clone()),
            Ok(metadata) => {
                // ADR-FM-05: with core.fileMode=true a regular file whose owner
                // execute bit differs from the index is a mode-only change.
                let mode_only_change = file_mode
                    && metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && index.get(file_str, 0).is_some_and(|entry| {
                        entry.mode & 0o100000 == 0o100000
                            && (entry.mode & 0o111 != 0) != worktree_exec_bit(&metadata)
                    });
                if mode_only_change {
                    visible.modified.push(file.clone());
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
        }
    }
    let (files, ignored_files) = list_workdir_files_split_force(workdir).map_err(|source| {
        StatusError::ListWorkdirFiles {
            path: workdir.clone(),
            source,
        }
    })?;
    // A non-UTF-8 name is NOT a status failure (§B.6.1): the base `??` row
    // survives everywhere else, and failing the whole command here would
    // make `--scan` the one mode a repository containing such a file could
    // never run. It cannot be a tracked-lookup key, so it is simply treated
    // as untracked — which is what it is.
    for file in files {
        let untracked = match file.to_str() {
            Some(file_str) => !index.tracked(file_str, 0),
            None => true,
        };
        if untracked && !is_same_file_tracked_alias(workdir, &file, &tracked_fold) {
            visible.new.push(file);
        }
    }
    for file in ignored_files {
        let untracked = match file.to_str() {
            Some(file_str) => !index.tracked(file_str, 0),
            None => true,
        };
        if untracked && !is_same_file_tracked_alias(workdir, &file, &tracked_fold) {
            ignored.new.push(file);
        }
    }
    Ok((visible, ignored))
}

fn changes_to_be_staged_split_with_index(
    workdir: &PathBuf,
    index: &Index,
    ignore_case: bool,
    file_mode: bool,
) -> Result<(Changes, Changes), StatusError> {
    let mut visible = Changes::default();
    let mut ignored = Changes::default();
    let tracked_files = index.tracked_files();
    let tracked_fold = tracked_files_by_fold(&tracked_files, ignore_case);
    for file in tracked_files.iter() {
        // §B.6.1: skip the keyed comparisons for an undecodable name rather
        // than failing the whole status (see `collect_tracked_worktree_changes`).
        let Some(file_str) = file.to_str() else {
            continue;
        };
        // ADR-SW-04 item 1: a skip-worktree entry is a sparse-checkout path —
        // its worktree copy may legitimately be absent or stale, so neither
        // shape is a change. add -u/-A and commit -a share this computation.
        if index
            .get(file_str, 0)
            .is_some_and(|entry| entry.flags.skip_worktree)
        {
            continue;
        }
        // A `160000` gitlink names a SUBMODULE commit, not a blob of this
        // repository. Comparing one against the working tree as a file reports
        // every submodule as deleted — or, when the directory exists, fails the
        // whole status trying to hash a directory — and that verdict feeds
        // `ensure_clean_status`, so it would block `switch`/`merge`/`rebase` in
        // any repository that merely CONTAINS a submodule. Submodule content is
        // out of scope (ADR-MG-01), so the two shapes Libra expects — absent
        // (never materialized) and a directory (checked out by the user) — are
        // left alone. A plain file or symlink there is neither, and stays
        // visible as a modification rather than being silently ignored.
        if is_gitlink_index_entry(index, file_str) {
            match workdir.join(file).symlink_metadata() {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                    continue;
                }
                // Absent is the normal shape: Libra never checks a submodule out.
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) =>
                {
                    continue;
                }
                // A plain file or symlink is neither shape, so it stays visible.
                Ok(_) => {
                    visible.modified.push(file.clone());
                    continue;
                }
                // Anything else is a real worktree read failure: fall through to
                // the ordinary handling below rather than reporting clean.
                Err(_) => {}
            }
        }
        let file_abs = workdir.join(file);
        match file_abs.symlink_metadata() {
            Err(_) => visible.deleted.push(file.clone()),
            Ok(metadata) => {
                // ADR-FM-05: with core.fileMode=true a regular file whose owner
                // execute bit differs from the index is a mode-only change.
                let mode_only_change = file_mode
                    && metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && index.get(file_str, 0).is_some_and(|entry| {
                        entry.mode & 0o100000 == 0o100000
                            && (entry.mode & 0o111 != 0) != worktree_exec_bit(&metadata)
                    });
                if mode_only_change {
                    visible.modified.push(file.clone());
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
        }
    }
    let (files, ignored_files) =
        list_workdir_files_split_safe(workdir).map_err(|source| StatusError::ListWorkdirFiles {
            path: workdir.clone(),
            source,
        })?;
    // §B.6.1: an undecodable name is untracked, not a fatal error — it can
    // never be an index key, so the lookup is simply skipped.
    for file in files {
        let untracked = file.to_str().is_none_or(|name| !index.tracked(name, 0));
        if untracked && !is_same_file_tracked_alias(workdir, &file, &tracked_fold) {
            visible.new.push(file);
        }
    }
    for file in ignored_files {
        let untracked = file.to_str().is_none_or(|name| !index.tracked(name, 0));
        if untracked && !is_same_file_tracked_alias(workdir, &file, &tracked_fold) {
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
    // ONE snapshot for the whole walk (§C.4.1.1): capturing per path would
    // re-read process-global state thousands of times, and a concurrent re-pin
    // between two paths would switch which worktree's exclusions this walk is
    // applying halfway through.
    let layers = crate::internal::layer::ExclusionSnapshot::for_request();

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
                if util::check_gitignore_with_layers(workdir, &path, &layers) {
                    ignored.push(relative);
                } else {
                    pending_dirs.push(path);
                }
            } else if file_type.is_file() || file_type.is_symlink() {
                if util::check_gitignore_with_layers(workdir, &path, &layers) {
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
mod argv_normalization_test {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<std::ffi::OsString> {
        parts.iter().map(std::ffi::OsString::from).collect()
    }

    fn normalize(parts: &[&str]) -> StatusArgvResolution {
        normalize_status_argv(
            argv(parts),
            &<crate::cli::Cli as clap::CommandFactory>::command(),
        )
    }

    /// §B.4.3: the argv scan itself records the format flags, from the
    /// SUBCOMMAND's arity table.
    ///
    /// This is the level the cluster logic has to be tested at. An
    /// end-to-end assertion about `-bz` output cannot fail if the scan is
    /// deleted, because clap parses the cluster too — so it proves clap
    /// works, not that the scan does.
    #[test]
    fn clusters_are_scanned_from_the_subcommand_arity_table() {
        for cluster in ["-bz", "-zb"] {
            let resolution = normalize(&["libra", "status", cluster]);
            assert!(
                resolution.format.z_explicit,
                "`{cluster}` contains -z: {:?}",
                resolution.format
            );
            assert!(
                !resolution.format.short_explicit,
                "`{cluster}` contains no -s: {:?}",
                resolution.format
            );
        }
        for cluster in ["-sz", "-zs"] {
            let resolution = normalize(&["libra", "status", cluster]);
            assert!(
                resolution.format.z_explicit && resolution.format.short_explicit,
                "`{cluster}` contains both -s and -z: {:?}",
                resolution.format
            );
        }
    }

    /// A cluster STOPS at the first value-taking option: everything after it
    /// is that option's value, and its letters are not flags. `-uzs` is
    /// `-u=zs`, so neither `z` nor `s` may be recorded.
    #[test]
    fn a_cluster_value_is_never_read_as_flags() {
        let resolution = normalize(&["libra", "status", "-uzs"]);
        assert_eq!(
            resolution.format,
            StatusFormatFlags::default(),
            "`zs` is -u's VALUE, not two flags: {:?}",
            resolution.format
        );
        // And the same letters BEFORE the value option are flags.
        let resolution = normalize(&["libra", "status", "-zuno"]);
        assert!(
            resolution.format.z_explicit,
            "the -z before -u is a flag: {:?}",
            resolution.format
        );
        assert!(
            !resolution.format.short_explicit,
            "`no` is -u's value: {:?}",
            resolution.format
        );
    }

    /// A global AFTER the subcommand is a global, not a cluster.
    ///
    /// clap accepts `libra status -J=ndjson`, and the scan has to as well:
    /// reading `ndjson` as a cluster made its `s` look like `--short`, and
    /// the agreement check then refused a perfectly ordinary command line.
    #[test]
    fn a_global_after_the_subcommand_is_not_a_cluster() {
        let resolution = normalize(&["libra", "status", "-J=ndjson"]);
        assert_eq!(
            resolution.format,
            StatusFormatFlags::default(),
            "`ndjson` is -J's value: {:?}",
            resolution.format
        );
        // The same for a global taking a SEPARATE value.
        let resolution = normalize(&["libra", "status", "--color", "never"]);
        assert_eq!(resolution.format, StatusFormatFlags::default());
    }

    /// A valued GLOBAL with an attached value does not shift subcommand
    /// location, and the value's letters are not flags.
    #[test]
    fn a_global_attached_value_does_not_shift_the_subcommand() {
        let resolution = normalize(&["libra", "-J=ndjson", "status", "--find-renames=505"]);
        assert_eq!(
            resolution.rename_occurrences.len(),
            1,
            "the status slice was found after a valued global"
        );
        assert_eq!(
            resolution.format,
            StatusFormatFlags::default(),
            "`ndjson` is a value, not flags: {:?}",
            resolution.format
        );
        // The raw value survives for the resolver; argv carries a placeholder.
        assert_eq!(
            resolution.argv[3],
            std::ffi::OsString::from("--find-renames=50")
        );
    }

    /// Everything after `--` is copied verbatim: not scanned for flags, not
    /// collected as an occurrence, not rewritten.
    #[test]
    fn tokens_after_the_separator_are_untouched() {
        let resolution = normalize(&["libra", "status", "--", "--find-renames=505", "-z"]);
        assert!(
            resolution.rename_occurrences.is_empty(),
            "a pathspec is not an occurrence"
        );
        assert_eq!(
            resolution.format,
            StatusFormatFlags::default(),
            "a pathspec is not a flag: {:?}",
            resolution.format
        );
        assert_eq!(
            resolution.argv[3],
            std::ffi::OsString::from("--find-renames=505"),
            "and it is not rewritten"
        );
    }

    /// A non-status subcommand is never rewritten, even when its own
    /// arguments spell `status`.
    #[test]
    fn a_non_status_subcommand_is_left_alone() {
        let resolution = normalize(&["libra", "diff", "status", "--find-renames=505"]);
        assert!(resolution.rename_occurrences.is_empty());
        assert_eq!(
            resolution.argv[3],
            std::ffi::OsString::from("--find-renames=505")
        );
    }

    /// WT-02: `-M[<raw>]` is scanned as another `--find-renames` spelling —
    /// standalone and inside a short cluster — recording the raw value for
    /// the resolver while argv carries the clap-safe placeholder.
    #[test]
    fn short_m_rename_spellings_are_scanned() {
        let resolution = normalize(&["libra", "status", "--porcelain", "-M"]);
        assert_eq!(resolution.rename_occurrences.len(), 1, "bare -M");
        assert_eq!(
            resolution.argv[3],
            std::ffi::OsString::from("-M50"),
            "bare -M is rewritten so clap cannot eat the next token"
        );

        let resolution = normalize(&["libra", "status", "-M90%"]);
        assert_eq!(resolution.rename_occurrences.len(), 1, "glued -M90%");
        assert_eq!(resolution.argv[2], std::ffi::OsString::from("-M50"));

        let resolution = normalize(&["libra", "status", "-sM90"]);
        assert_eq!(resolution.rename_occurrences.len(), 1, "clustered -sM90");
        assert_eq!(
            resolution.argv[2],
            std::ffi::OsString::from("-sM50"),
            "the preceding flags survive the rewrite"
        );
        assert!(
            resolution.format.short_explicit,
            "-s is still a format flag"
        );
    }

    /// WT-02/M4: `-M` takes part in the LAST-occurrence-wins ordering across
    /// all rename spellings, and only the winning raw value is interpreted.
    #[test]
    fn short_m_takes_part_in_last_wins_ordering() {
        for (argv, expected) in [
            (&["libra", "status", "-M", "--no-renames"][..], None),
            (&["libra", "status", "--no-renames", "-M"][..], Some(30000)),
            (&["libra", "status", "-M90", "--renames"][..], Some(30000)),
            (&["libra", "status", "--renames", "-M90"][..], Some(54000)),
            (&["libra", "status", "-M90%"][..], Some(54000)),
        ] {
            let resolution = normalize(argv);
            let threshold = resolve_status_threshold(&StatusArgs::default(), Some(&resolution))
                .unwrap_or_else(|error| panic!("{argv:?}: {error}"));
            assert_eq!(threshold, expected, "{argv:?}");
        }

        // An invalid `-M` that a later spelling overrides is never parsed...
        let resolution = normalize(&["libra", "status", "-Mabc", "--no-renames"]);
        assert_eq!(
            resolve_status_threshold(&StatusArgs::default(), Some(&resolution)).expect("resolve"),
            None
        );
        // ...while an invalid WINNER fails closed with LBR-CLI-002 (M5).
        let resolution = normalize(&["libra", "status", "-Mabc"]);
        let error = resolve_status_threshold(&StatusArgs::default(), Some(&resolution))
            .expect_err("invalid winner");
        assert_eq!(error.stable_code(), StableErrorCode::CliInvalidArguments);
    }

    /// §B.4.3: the API percent field accepts ONLY 0..=100 — a struct-literal
    /// caller passing 101..=255 fails closed with LBR-CLI-002 instead of a
    /// silent clamp to exact-only, and the clap parser path refuses the
    /// value outright (2026-08-05 R0-4 review).
    #[test]
    #[serial_test::serial(cwd)]
    fn api_percent_above_100_fails_closed() {
        use clap::Parser as _;

        let args = StatusArgs {
            find_renames: Some(101),
            ..Default::default()
        };
        let err = resolve_status_threshold(&args, None).expect_err("101% is out of range");
        assert_eq!(err.stable_code(), StableErrorCode::CliInvalidArguments);

        assert!(
            StatusArgs::try_parse_from(["status", "--find-renames=101"]).is_err(),
            "clap's range guard refuses 101 on the parser path"
        );

        let full = resolve_status_threshold(
            &StatusArgs {
                find_renames: Some(100),
                ..Default::default()
            },
            None,
        )
        .expect("100% stays valid");
        assert_eq!(full, Some(60000), "100% means exact-only, not an error");
    }
}

#[cfg(test)]
mod test {
    use sea_orm::{ConnectionTrait, Statement};
    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        internal::db::reset_db_conn_instance_for_path,
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

        let hash = ObjectHash::new_for_kind(git_internal::hash::get_hash_kind(), &[0u8; 20]);
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
        let rows = generate_short_format_status_with_unmerged(&empty, &empty, &unmerged, &[]);
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
    #[serial(cwd, env)]
    async fn sequence_notice_surfaces_corrupt_sequence_kind() {
        let repo = tempdir().expect("failed to create temp repo");
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());
        let db_path = repo.path().join(".libra").join("libra.db");
        let db = crate::internal::sequencer::request_db_checked()
            .await
            .expect("test fixture database");
        db.execute_raw(Statement::from_string(
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
    #[serial(cwd, env)]
    async fn resolve_upstream_info_surfaces_branch_config_query_failures() {
        let repo = tempdir().expect("failed to create temp repo");
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());
        let db_path = repo.path().join(".libra").join("libra.db");

        let db = crate::internal::sequencer::request_db_checked()
            .await
            .expect("test fixture database");
        db.execute_raw(Statement::from_string(
            db.get_database_backend(),
            "DROP TABLE config_kv",
        ))
        .await
        .expect("dropping config_kv table should succeed");

        let err = resolve_upstream_info(&Head::Branch("main".to_string()), None, &mut Vec::new())
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

    /// §B.5 seam: engine degradation stats map onto both structured warning
    /// codes (the end-to-end similarity path is covered by
    /// `similarity_budget_warning`; this pins the mapping in isolation).
    #[test]
    fn rename_stats_map_to_structured_warnings() {
        let stats = rename_detect::RenameDetectStats {
            skipped_by_limit: true,
            exhaustive_discarded: true,
            ..Default::default()
        };
        let mut warnings = Vec::new();
        warnings_from_rename_stats(&stats, &mut warnings);
        assert_eq!(warnings.len(), 2);
        assert!(matches!(
            warnings[0].code,
            StatusWarningCode::RenameLimitProductSkipped
        ));
        assert!(matches!(
            warnings[1].code,
            StatusWarningCode::SimilarityBudgetExceeded
        ));
        // Full-text pins: both degradation messages must name their
        // SURVIVORS — the engine keeps exact and already-scored
        // unique-basename pairs and discards only the exhaustive stage, and
        // a rewording that claims the whole inexact pass was lost would
        // misreport the output the user is looking at.
        assert_eq!(
            warnings[0].message,
            "rename detection skipped the exhaustive inexact pass: too many candidates on one side (renameLimit); exact and unique-basename matches were kept"
        );
        assert_eq!(
            warnings[1].message,
            "rename detection discarded the exhaustive inexact pass: similarity comparison budget exceeded; exact and already-scored unique-basename matches were kept"
        );
        assert!(
            warnings
                .iter()
                .all(|w| matches!(w.source, StatusWarningSource::RenameDetect))
        );
    }

    /// §B.5: the frozen `source` enum means `metadata` for the object store
    /// and `worktree` for the working tree. Both sides can hit a size cap,
    /// exhaust a budget, and fail to read, so this asserts BOTH directions:
    /// no worktree problem is published as `metadata`, and no object problem
    /// is published as `worktree`.
    #[test]
    #[serial_test::serial(cwd)]
    fn content_skips_map_to_metadata_and_worktree_warnings() {
        use rename_detect::SkipReason;
        let mut stats = rename_detect::RenameDetectStats::default();
        stats.content_skips.insert(SkipReason::ObjectMissing, 2);
        stats.content_skips.insert(SkipReason::ObjectCorrupt, 1);
        stats.content_skips.insert(SkipReason::ObjectIoFailed, 6);
        stats.content_skips.insert(SkipReason::ObjectTooLarge, 3);
        stats
            .content_skips
            .insert(SkipReason::ObjectBudgetExceeded, 4);
        stats.content_skips.insert(SkipReason::WorktreeTooLarge, 8);
        stats
            .content_skips
            .insert(SkipReason::WorktreeBudgetExceeded, 9);
        stats.content_skips.insert(SkipReason::WorktreeIoFailed, 5);
        let mut warnings = Vec::new();
        warnings_from_rename_stats(&stats, &mut warnings);
        assert_eq!(warnings.len(), 4, "{warnings:?}");

        let find = |code: StatusWarningCode| {
            warnings
                .iter()
                .find(|w| w.code == code)
                .unwrap_or_else(|| panic!("{code:?} missing from {warnings:?}"))
        };

        // Object side: missing + corrupt + object I/O failure, all `metadata`.
        let unavailable = find(StatusWarningCode::MetadataUnavailable);
        assert_eq!(unavailable.source, StatusWarningSource::Metadata);
        assert!(
            unavailable.message.contains("9 candidate(s)"),
            "{unavailable:?}"
        );
        // Object side caps, still `metadata` — and NOT inflated by the
        // worktree caps below.
        let object_budget = find(StatusWarningCode::MetadataBudgetExceeded);
        assert_eq!(object_budget.source, StatusWarningSource::Metadata);
        assert!(
            object_budget.message.contains("7 candidate(s)"),
            "{object_budget:?}"
        );
        // Worktree caps get their OWN code under `worktree`; before the
        // split these were reported as a repository-object budget.
        let worktree_budget = find(StatusWarningCode::WorktreeBudgetExceeded);
        assert_eq!(worktree_budget.source, StatusWarningSource::Worktree);
        assert!(
            worktree_budget.message.contains("17 candidate(s)"),
            "{worktree_budget:?}"
        );
        // And a worktree read failure stays `worktree` — the object-side
        // I/O failure above must not have leaked into this count.
        let worktree_failed = find(StatusWarningCode::WorktreeReadFailed);
        assert_eq!(worktree_failed.source, StatusWarningSource::Worktree);
        assert!(
            worktree_failed.message.contains("5 candidate(s)"),
            "{worktree_failed:?}"
        );
    }
}

#[cfg(all(test, unix))]
mod mode_fix_hint_test {
    use std::os::unix::fs::PermissionsExt;

    use git_internal::internal::index::IndexEntry;
    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::utils::test::ChangeDirGuard;

    /// FM-05 (M-HINT H1/H3/H5): candidate detection needs index 100755,
    /// worktree non-exec, and identical content; reverse direction and
    /// `core.fileMode=false` stay empty.
    #[test]
    #[serial(cwd, env)]
    fn mode_fix_candidates_track_legacy_shape_only() {
        let repo = tempdir().expect("temp repo");
        std::fs::create_dir_all(repo.path().join(".libra/objects")).expect("objects dir");
        std::fs::write(repo.path().join(".libra/libra.db"), b"").expect("db marker");
        let _guard = ChangeDirGuard::new(repo.path());

        let run_content = b"x\n";
        let plain_content = b"y\n";
        std::fs::write(repo.path().join("run.sh"), run_content).expect("write run");
        std::fs::write(repo.path().join("plain.txt"), plain_content).expect("write plain");

        let mut index = Index::new();
        for (name, mode) in [("run.sh", 0o100755u32), ("plain.txt", 0o100644)] {
            let path = repo.path().join(name);
            let mut entry = IndexEntry::new_from_blob(
                name.to_string(),
                calc_file_blob_hash(&path).expect("hash"),
                0,
            );
            entry.mode = mode;
            index.add(entry);
        }
        let index_path = repo.path().join(".libra/index");
        index.save(&index_path).expect("save index");

        // Legacy shape: run.sh lost exec, plain.txt gained it.
        std::fs::set_permissions(
            repo.path().join("run.sh"),
            std::fs::Permissions::from_mode(0o644),
        )
        .expect("chmod run");
        std::fs::set_permissions(
            repo.path().join("plain.txt"),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("chmod plain");

        let scan = status_untracked::collect_status_worktree_changes(
            UntrackedFiles::No,
            false,
            false,
            true,
        )
        .expect("scan");
        assert_eq!(
            scan.mode_fix_candidates,
            vec![PathBuf::from("run.sh")],
            "only the executable-bit-lost direction is a candidate"
        );

        let scan = status_untracked::collect_status_worktree_changes(
            UntrackedFiles::No,
            false,
            false,
            false,
        )
        .expect("scan fileMode=false");
        assert!(
            scan.mode_fix_candidates.is_empty(),
            "fileMode=false yields no candidates"
        );
    }
}

#[cfg(test)]
mod rename_destination_budget_test {
    use super::*;
    use crate::utils::test::ChangeDirGuard;

    /// The destination (untracked-side) detector must hand its drawn-down
    /// budgets back to the run-level `RenameBudgets` — remaining bytes and
    /// tasks, the shared OID cache, and the spent comparisons. Without the
    /// restore a detection pass added after it would restart with fresh
    /// budgets, silently doubling the call-level caps (§B.3.4).
    #[test]
    #[serial_test::serial(cwd, env)]
    fn destination_detector_restores_budgets_and_records_comparisons() {
        let repo = tempfile::tempdir().expect("temp repo");
        // Minimal bare-layout markers so path discovery treats the temp dir
        // as a repository and object lookups fail as genuine misses.
        std::fs::create_dir_all(repo.path().join("objects")).expect("objects dir");
        std::fs::write(repo.path().join("libra.db"), b"").expect("db marker");
        let _guard = ChangeDirGuard::new(repo.path());

        let mut details: RenameDetails = HashMap::new();
        let mut stats = rename_detect::RenameDetectStats::default();
        let mut budgets = RenameBudgets::new();
        let (worktree_before, tasks_before) = (budgets.worktree_total, budgets.worktree_tasks);
        let (objects_before, slots_before) = (budgets.objects_total, budgets.objects_slots);
        let config = rename_detect::RenameDetectConfig {
            threshold: 30000,
            rename_limit: 1000,
            comparison_budget: Some(500_000),
        };

        // A scorable inexact pair (both sides worktree this call) spends
        // worktree budget on hashing/reads and records real comparisons.
        // The fixture is large enough that spanhash sees many shared spans.
        let old_payload = b"alpha beta gamma delta\n".repeat(200);
        let mut new_payload = old_payload.clone();
        let mid = new_payload.len() / 2;
        new_payload[mid] = b'X';
        std::fs::write(repo.path().join("gone.txt"), &old_payload).expect("old");
        std::fs::write(repo.path().join("came.txt"), &new_payload).expect("new");
        let mut changes = Changes {
            new: vec![],
            modified: vec![],
            deleted: vec![PathBuf::from("gone.txt")],
            renamed: vec![],
        };
        let consumed = detect_renames_with_destinations(
            &mut changes,
            &config,
            RenameBlobSide::Worktree,
            &[PathBuf::from("came.txt")],
            &mut details,
            &mut stats,
            &mut budgets,
        );
        assert!(
            consumed.contains(&PathBuf::from("came.txt")),
            "the similar pair should match inexact and consume the destination"
        );
        assert!(
            budgets.worktree_total < worktree_before || budgets.worktree_tasks < tasks_before,
            "worktree bytes/tasks spent by the destination pass must be restored \
             to the shared budget (before={worktree_before}/{tasks_before} \
             after={}/{})",
            budgets.worktree_total,
            budgets.worktree_tasks
        );
        assert!(
            budgets.comparisons_spent > 0,
            "inexact scoring comparisons must be recorded against the shared cap"
        );
        // The NEXT consumer sees the depleted remainder, not a fresh cap.
        let narrowed = budgets.narrowed(&config);
        assert_eq!(
            narrowed.comparison_budget,
            Some(500_000u64.saturating_sub(budgets.comparisons_spent)),
            "a later pass must inherit the spent comparisons"
        );

        // A HEAD/index-side candidate whose object is missing consumes an
        // object slot and lands in the SHARED OID cache; both must come back
        // with the restored object budget.
        let missing_oid = git_internal::internal::object::blob::Blob::from_content_bytes(
            b"never stored in this repository".to_vec(),
        )
        .id;
        std::fs::create_dir_all(repo.path().join("d")).expect("d dir");
        std::fs::create_dir_all(repo.path().join("u")).expect("u dir");
        std::fs::write(repo.path().join("u/same.txt"), b"payload\n").expect("dest");
        let known: HashMap<PathBuf, (ObjectHash, u32)> =
            [(PathBuf::from("d/same.txt"), (missing_oid, 0o100644))]
                .into_iter()
                .collect();
        let mut changes2 = Changes {
            new: vec![],
            modified: vec![],
            deleted: vec![PathBuf::from("d/same.txt")],
            renamed: vec![],
        };
        detect_renames_with_destinations(
            &mut changes2,
            &config,
            RenameBlobSide::Known(&known),
            &[PathBuf::from("u/same.txt")],
            &mut details,
            &mut stats,
            &mut budgets,
        );
        assert!(
            !budgets.object_cache.is_empty(),
            "the shared OID cache must return with the restored object budget"
        );
        assert!(
            budgets.objects_slots < slots_before,
            "the missing-object lookup consumed a slot that must be restored \
             (before={slots_before} after={})",
            budgets.objects_slots
        );
        assert!(
            budgets.objects_total <= objects_before,
            "object byte budget must never grow across a pass"
        );
    }
}

#[cfg(test)]
mod seam_gate_test {
    use super::*;

    /// `LIBRA_TEST_STATUS_COMPARISON_BUDGET` must be honored only under the
    /// test harness: without `LIBRA_TEST` the production cap stays in
    /// effect; with the gate the override bites.
    #[test]
    #[serial_test::serial(env)]
    fn comparison_budget_override_requires_the_harness_gate() {
        // SAFETY: serialized test body; every variable is removed again
        // before the test returns.
        unsafe {
            std::env::set_var("LIBRA_TEST_STATUS_COMPARISON_BUDGET", "1");
            std::env::remove_var(crate::utils::pager::LIBRA_TEST_ENV);
            assert_eq!(
                status_comparison_budget(),
                rename_detect::STATUS_MAX_SIMILARITY_COMPARISONS,
                "without LIBRA_TEST the budget override must be ignored"
            );
            std::env::set_var(crate::utils::pager::LIBRA_TEST_ENV, "1");
            assert_eq!(
                status_comparison_budget(),
                1,
                "with the gate the budget override applies"
            );
            // Tighten-only: even under the gate, a value above the
            // production cap clamps back to it — the seam can shrink the
            // budget to force exhaustion, never raise it.
            std::env::set_var(
                "LIBRA_TEST_STATUS_COMPARISON_BUDGET",
                (rename_detect::STATUS_MAX_SIMILARITY_COMPARISONS + 1).to_string(),
            );
            assert_eq!(
                status_comparison_budget(),
                rename_detect::STATUS_MAX_SIMILARITY_COMPARISONS,
                "a gated override above the production cap is clamped to the cap"
            );
            std::env::remove_var("LIBRA_TEST_STATUS_COMPARISON_BUDGET");
            std::env::remove_var(crate::utils::pager::LIBRA_TEST_ENV);
        }
    }
}
