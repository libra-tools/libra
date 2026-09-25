//! Implements status reporting with ignore policy support, computing staged/unstaged/untracked sets and printing concise summaries.

use std::{
    collections::{HashMap, HashSet},
    io,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};

use clap::{Parser, ValueEnum};
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
        shallow::ShallowSet,
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

// StatusData stays here so scan, cache, and renderers share one result shape.
mod cache;
mod input;
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

async fn render_status_to_writer(
    data: &StatusData,
    args: &StatusArgs,
    output: &OutputConfig,
    writer: &mut impl Write,
) -> CliResult<()> {
    fail_closed_on_io_blocked(data, output)?;
    let write_error =
        |err: io::Error| crate::utils::output::stdout_write_error("write status output", err);
    let mut buffer = Vec::new();

    // §B.6.4 machine-format path base: porcelain v1/v2 ALWAYS emit
    // repository-root-relative paths (Git parity), while the human
    // formats honor `status.relativePaths`. The collected data carries
    // the display base, so project it back for the porcelain renderers.
    let porcelain_data;
    let data = if args.porcelain.is_some() {
        porcelain_data = data.to_repo_relative();
        &porcelain_data
    } else {
        data
    };

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
                data.quote_path,
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
                data.quote_path,
                &mut buffer,
            )?;
            if args.ignored && !data.ignored_files.is_empty() {
                for file in &data.ignored_files {
                    if args.null_terminated {
                        write!(&mut buffer, "!! ").map_err(write_error)?;
                        write_raw_path(&mut buffer, file).map_err(write_error)?;
                        buffer.push(b'\0');
                    } else {
                        buffer.extend_from_slice(b"!! ");
                        buffer.extend_from_slice(&quote_pathname_bytes(file, data.quote_path));
                        buffer.push(b'\n');
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
            data.quote_path,
            &mut buffer,
        )
        .await?;
        if args.ignored {
            for file in &data.ignored_files {
                if args.null_terminated {
                    write!(&mut buffer, "!! ").map_err(write_error)?;
                    write_raw_path(&mut buffer, file).map_err(write_error)?;
                    buffer.push(b'\0');
                } else {
                    buffer.extend_from_slice(b"!! ");
                    buffer.extend_from_slice(&quote_pathname_bytes(file, data.quote_path));
                    buffer.push(b'\n');
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
        with_dir_marker(path, util::to_workdir_path(path))
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
    fn details(details: &RenameDetails) -> RenameDetails {
        details
            .iter()
            .map(|((from, to), value)| ((convert(from), convert(to)), *value))
            .collect()
    }
    let mut rooted = data.clone();
    rooted.staged = changes(&data.staged);
    rooted.unstaged = changes(&data.unstaged);
    // Keep the score/exactness lookup keys aligned with the converted rename
    // pairs, or JSON emission from a subdirectory would miss every detail.
    rooted.staged_rename_details = details(&data.staged_rename_details);
    rooted.unstaged_rename_details = details(&data.unstaged_rename_details);
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
        |err: io::Error| crate::utils::output::stdout_write_error("write status output", err);

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
            data.quote_path,
        );
        if args.column {
            render_columnated_labeled_entries(buffer, &entries, colored::Color::BrightGreen)?;
        } else {
            for (label, path) in entries {
                let mut line = format!("\t{label} ").into_bytes();
                line.extend_from_slice(&path);
                push_colored_line(buffer, &colored::Color::BrightGreen.to_fg_str(), &line);
            }
        }
    }

    // Unstaged changes (modified + deleted + renamed — a probe-paired
    // unstaged rename can be the section's ONLY content, §B.3.1)
    if !data.unstaged.deleted.is_empty()
        || !data.unstaged.modified.is_empty()
        || !data.unstaged.renamed.is_empty()
    {
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
            data.quote_path,
        );
        if args.column {
            render_columnated_labeled_entries(buffer, &entries, colored::Color::BrightRed)?;
        } else {
            for (label, path) in entries {
                let mut line = format!("\t{label} ").into_bytes();
                line.extend_from_slice(&path);
                push_colored_line(buffer, &colored::Color::BrightRed.to_fg_str(), &line);
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
                    quote_pathname_bytes(&entry.path, data.quote_path),
                )
            })
            .collect::<Vec<_>>();
        if args.column {
            render_columnated_labeled_entries(buffer, &entries, colored::Color::BrightRed)?;
        } else {
            for (label, path) in entries {
                let mut line = format!("\t{label} ").into_bytes();
                line.extend_from_slice(&path);
                push_colored_line(buffer, &colored::Color::BrightRed.to_fg_str(), &line);
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
            render_columnated_paths(buffer, &data.unstaged.new, data.quote_path)?;
        } else {
            for f in &data.unstaged.new {
                let mut line = b"\t".to_vec();
                line.extend_from_slice(&quote_pathname_bytes(f, data.quote_path));
                push_colored_line(buffer, &colored::Color::BrightRed.to_fg_str(), &line);
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
            render_columnated_paths(buffer, &data.ignored_files, data.quote_path)?;
        } else {
            for f in &data.ignored_files {
                let mut line = b"\t".to_vec();
                line.extend_from_slice(&quote_pathname_bytes(f, data.quote_path));
                push_colored_line(buffer, &colored::Color::BrightRed.to_fg_str(), &line);
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
    quote_path: bool,
) -> Vec<(&'a str, Vec<u8>)> {
    let mut entries = Vec::new();
    for f in deleted {
        entries.push((deleted_label, quote_pathname_bytes(f, quote_path)));
    }
    for f in modified {
        entries.push((modified_label, quote_pathname_bytes(f, quote_path)));
    }
    for (old, new) in renamed {
        let mut line = quote_pathname_bytes(old, quote_path);
        line.extend_from_slice(b" -> ");
        line.extend_from_slice(&quote_pathname_bytes(new, quote_path));
        entries.push((renamed_label, line));
    }
    for f in new_files {
        entries.push((new_label, quote_pathname_bytes(f, quote_path)));
    }
    entries
}

/// Render labeled entries in aligned columns.
fn render_columnated_labeled_entries(
    buffer: &mut Vec<u8>,
    entries: &[(&str, Vec<u8>)],
    color: colored::Color,
) -> CliResult<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let max_label_width = entries.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
    for (label, path) in entries {
        let mut line = format!("\t{label:max_label_width$} ").into_bytes();
        line.extend_from_slice(path);
        push_colored_line(buffer, &color.to_fg_str(), &line);
    }
    Ok(())
}

/// Wrap one content line in an ANSI fg color + reset and terminate it,
/// honoring the colored crate's own colorize gate (so piped/test output
/// stays plain, exactly like the `.bright_green()` call sites it
/// replaces). The colored crate's API is `String`-only, so byte-faithful
/// paths (raw non-UTF-8 bytes under `core.quotePath=false`) are colored
/// manually with the same codes the crate emits.
fn push_colored_line(buffer: &mut Vec<u8>, fg: &str, line: &[u8]) {
    let colorize = colored::control::SHOULD_COLORIZE.should_colorize();
    if colorize {
        buffer.extend_from_slice(b"\x1b[");
        buffer.extend_from_slice(fg.as_bytes());
        buffer.extend_from_slice(b"m");
    }
    buffer.extend_from_slice(line);
    if colorize {
        buffer.extend_from_slice(b"\x1b[0m");
    }
    buffer.push(b'\n');
}

/// Render plain paths in multiple columns like `ls`.
fn render_columnated_paths(
    buffer: &mut Vec<u8>,
    paths: &[PathBuf],
    quote_path: bool,
) -> CliResult<()> {
    let write_error =
        |err: io::Error| crate::utils::output::stdout_write_error("write status output", err);
    if paths.is_empty() {
        return Ok(());
    }

    let names: Vec<Vec<u8>> = paths
        .iter()
        .map(|p| quote_pathname_bytes(p, quote_path))
        .collect();
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
            buffer.extend_from_slice(name);
            if col + 1 < num_cols {
                for _ in name.len()..col_width {
                    buffer.push(b' ');
                }
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
        |err: io::Error| crate::utils::output::stdout_write_error("write status output", err);

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
        |err: io::Error| crate::utils::output::stdout_write_error("write status output", err);

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

/// Render collected warnings to stderr (`warning: …`) and mark the global
/// warning tracker — human/short/porcelain delivery only (§B.5 matrix).
/// JSON callers must NOT use this: their warnings ride in `data.warnings[]`.
fn deliver_warnings_stderr(warnings: &[StatusWarning]) {
    // In TEXT mode RepositoryPreflight entries were already printed live by
    // `emit_warning` when the preflight raised them (with structured output
    // inactive it prints immediately); they join the structured list only so
    // JSON payloads and §B.5 exit arbitration see them, and re-printing them
    // here doubled the stderr line. Under a structured envelope nothing was
    // printed live (`emit_warning` only buffers), so the JSON output-failure
    // fallback that calls this MUST deliver them or they vanish with the
    // envelope — the skip is therefore conditional on live delivery having
    // happened.
    let preflight_already_live = !crate::utils::output::structured_output_active();
    let mut delivered_any = false;
    for warning in warnings {
        if preflight_already_live && matches!(warning.code, StatusWarningCode::RepositoryPreflight)
        {
            continue;
        }
        eprintln!("warning: {}", warning.message);
        delivered_any = true;
    }
    if delivered_any {
        crate::utils::output::record_warning();
    }
}

/// Build a `source = cache` structured warning (§B.5 R0-8b).
fn cache_warning(code: StatusWarningCode, message: impl Into<String>) -> StatusWarning {
    StatusWarning {
        code,
        message: message.into(),
        source: code.source(),
    }
}

/// The single §B.5 exit arbitration point.
///
/// Every status path — full scan, `--scan`, both cache modes, and both
/// fallbacks — resolves its exit code here instead of repeating the
/// comparison. That matters because the ordering is subtle and one branch
/// getting it wrong is invisible in review: an early `silent_exit(1)` for a
/// dirty tree would preempt the warning exit 9 that is supposed to outrank
/// it. Priority: **fatal ≻ 9 (`--exit-code-on-warning`) ≻ 1 (dirty,
/// including a non-empty `io_blocked`) ≻ 0**. Fatal is raised earlier by
/// `fail_closed_on_io_blocked`, so this resolver covers 9 ≻ 1 ≻ 0.
struct StatusOutcome<'a> {
    data: &'a StatusData,
    args: &'a StatusArgs,
}

impl<'a> StatusOutcome<'a> {
    fn new(data: &'a StatusData, args: &'a StatusArgs) -> Self {
        Self { data, args }
    }

    fn resolve(&self, output: &OutputConfig) -> CliResult<()> {
        if let Some(exit) = warning_exit(output, &self.data.warnings) {
            return Err(exit);
        }
        // `--exit-code`: dirty → exit 1, silently (no error line).
        if self.args.exit_code && self.data.is_dirty() {
            return Err(CliError::silent_exit(1));
        }
        Ok(())
    }
}

/// §B.5 exit arbitration, rule 2: warnings + `--exit-code-on-warning` exit 9
/// and take precedence over the dirty exit 1. Local to each return point
/// because an early `silent_exit(1)` would otherwise preempt the top-level
/// exit-9 pass in `cli.rs` (and JSON never records globally).
fn warning_exit(output: &OutputConfig, warnings: &[StatusWarning]) -> Option<CliError> {
    // Decided from THIS invocation's structured list only. Consulting the
    // process-global tracker would let a warning emitted by an earlier
    // embedded call flip a later, clean one to exit 9 with an empty
    // `warnings[]` — and the reverse leak, where a status call marks the
    // tracker for whoever runs next. Preflight advisories are folded into
    // the list at collection time, so nothing is lost by dropping the
    // global read.
    (output.exit_code_on_warning && !warnings.is_empty()).then(|| CliError::silent_exit(9))
}

/// Sort key for `io_blocked[]`: the RAW path encoding, matching what
/// `raw_base64` serializes. `PathBuf`'s own ordering compares WTF-8 bytes on
/// Windows, which disagrees with UTF-16 code-unit order once supplementary
/// characters are involved — so a machine consumer relying on the documented
/// "sorted by raw path bytes" would see a different order than it computed.
pub(crate) fn raw_path_sort_key(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        // BIG-endian in the SORT KEY only. The documented order is by UTF-16
        // code unit, and a bytewise comparison reproduces that only when the
        // high byte comes first: little-endian would put U+0101 (`01 01`)
        // after U+0200 (`00 02`), inverting the required order. The
        // published `raw_base64` stays little-endian — the key exists to be
        // compared, not to be transmitted.
        let mut bytes = Vec::new();
        for unit in path.as_os_str().encode_wide() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        bytes
    }
    #[cfg(not(any(unix, windows)))]
    path.to_string_lossy().into_owned().into_bytes()
}

/// Reversible encoding of a path whose name is not valid UTF-8, for the
/// `io_blocked[].path.raw_base64` contract (§B.6.0.1). Returns `None` for a
/// valid-UTF-8 name, whose `display` form is already lossless.
///
/// Unix encodes the raw `OsStr` bytes. Windows encodes the UTF-16 code units
/// LITTLE-ENDIAN, because an unpaired surrogate has no UTF-8 form at all —
/// returning `None` there would break reversibility for exactly the names
/// that need it.
pub fn raw_path_base64(path: &Path) -> Option<String> {
    use base64::Engine as _;

    if path.to_str().is_some() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Some(base64::engine::general_purpose::STANDARD.encode(path.as_os_str().as_bytes()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let mut bytes = Vec::new();
        for unit in path.as_os_str().encode_wide() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        Some(base64::engine::general_purpose::STANDARD.encode(bytes))
    }
    #[cfg(not(any(unix, windows)))]
    None
}

/// A path as a JSON string, using the same escaping the docs promise for
/// display forms. Undecodable bytes become `\ooo` octal escapes rather than
/// `U+FFFD`, so distinct filenames stay distinct in the payload. The quoting
/// wrapper is stripped: JSON supplies its own quoting.
fn json_path_string(path: &Path, quote_path: bool) -> String {
    match path.to_str() {
        Some(text) => text.to_string(),
        None => {
            let quoted = quote_pathname(path, quote_path);
            quoted
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .map(str::to_string)
                .unwrap_or(quoted)
        }
    }
}

fn build_status_json(data: &StatusData, _args: &StatusArgs) -> serde_json::Value {
    // §B.5 delivery matrix: porcelain and JSON paths are ALWAYS
    // repository-root-relative, regardless of the invocation subdirectory.
    // Collection stays cwd-relative (pathspec filtering depends on it), so
    // the JSON payload converts here — identity when cwd is the repo root.
    let rooted = data_with_repo_root_paths(data);
    let data = &rooted;
    // Paths render through the SAME escaping the docs promise for display
    // forms. `Path::display()` replaces undecodable bytes with U+FFFD, so
    // two different real filenames could collapse to one JSON string —
    // silently merging distinct entries for every consumer.
    let quote = data.quote_path;
    let paths_to_json = move |paths: &[PathBuf]| -> Vec<serde_json::Value> {
        paths
            .iter()
            .map(|p| serde_json::Value::String(json_path_string(p, quote)))
            .collect()
    };

    let renamed_to_json = move |renamed: &[(PathBuf, PathBuf)]| -> Vec<serde_json::Value> {
        renamed
            .iter()
            .map(|(old, new)| {
                serde_json::json!({
                    "from": json_path_string(old, quote),
                    "to": json_path_string(new, quote),
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
                "from": json_path_string(old, quote),
                "to": json_path_string(new, quote),
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

    // §B.6.0.1 io_blocked[] public contract: escaped repo-relative display
    // (same quoting as non-`-z` porcelain), lossless raw bytes for
    // non-UTF-8 paths, the KNOWN staged component only, the reason
    // taxonomy, and the staged rename pair when one is known. Sorted by raw
    // path, deduplicated. Every entry also emits a worktree-family warning.
    // Warnings already carry the worktree family from collection time
    // (§B.5 single arbitration source); JSON only serializes them.
    let warnings_json = data.warnings.clone();
    let mut io_blocked_json: Vec<serde_json::Value> = Vec::new();
    for event in &data.io_blocked {
        let display = quote_pathname(&event.path, data.quote_path);
        let raw_base64: serde_json::Value = match raw_path_base64(&event.path) {
            Some(encoded) => serde_json::Value::String(encoded),
            None => serde_json::Value::Null,
        };
        // Compare on REPO-RELATIVE keys: `event.path` is repo-relative
        // while the change lists carry the display base, so from a
        // subdirectory a display-base conversion of the event would never
        // match (the historical `staged`/`rename` = null bug).
        // The change lists may carry repo-relative OR display-base paths
        // depending on the caller; accept either spelling of the same file
        // so the schema fields stay correct from a subdirectory.
        let matches_event = |candidate: &PathBuf| -> bool {
            candidate == &event.path || current_to_workdir(candidate) == event.path
        };
        let staged_component = if data.staged.modified.iter().any(matches_event) {
            serde_json::json!("M")
        } else if data.staged.new.iter().any(matches_event) {
            serde_json::json!("A")
        } else if data.staged.deleted.iter().any(matches_event) {
            serde_json::json!("D")
        } else if data
            .staged
            .renamed
            .iter()
            .any(|(_, new)| matches_event(new))
        {
            serde_json::json!("R")
        } else {
            serde_json::Value::Null
        };
        let rename = data
            .staged
            .renamed
            .iter()
            .find(|(_, new)| matches_event(new))
            .map(|pair| {
                let score = data
                    .staged_rename_details
                    .get(pair)
                    .map(|(pct, _)| *pct)
                    .unwrap_or(100);
                // Lossless like every other JSON path in the payload —
                // `display()` would U+FFFD-corrupt a non-UTF-8 pair
                // (2026-08-06 R0-6 review; latent, since staged pairs
                // currently require addable UTF-8 index paths).
                serde_json::json!({
                    "from": json_path_string(&pair.0, quote),
                    "to": json_path_string(&pair.1, quote),
                    "score": score,
                })
            })
            .unwrap_or(serde_json::Value::Null);
        let (reason, _warning_code) = io_blocked_reason_and_code(event.reason);
        io_blocked_json.push(serde_json::json!({
            "path": { "display": display, "raw_base64": raw_base64 },
            "staged": staged_component,
            "reason": reason,
            "rename": rename,
        }));
    }
    // §B.6.0.1: rename detection is complete only when nothing degraded it —
    // no probe truncation/blocks and no engine skip/limit/budget warnings.
    let rename_detection_complete = !data.rename_scan_blocked
        && !data.warnings.iter().any(|w| {
            matches!(
                w.code,
                StatusWarningCode::ProbeTruncated
                    | StatusWarningCode::RenameLimitProductSkipped
                    | StatusWarningCode::SimilarityBudgetExceeded
                    | StatusWarningCode::MetadataUnavailable
                    | StatusWarningCode::MetadataBudgetExceeded
                    | StatusWarningCode::WorktreeBudgetExceeded
                    | StatusWarningCode::RenamePathEncodingUnsupported
            )
        });

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
        "warnings": warnings_json,
        "renames": renames,
        "io_blocked": io_blocked_json,
        "base_scan_complete": !data.base_scan_blocked,
        "rename_detection_complete": rename_detection_complete,
        "complete": !data.base_scan_blocked && rename_detection_complete,
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
    output_porcelain_with_unmerged(staged, unstaged, &[], null_terminated, true, writer)
}

fn output_porcelain_with_unmerged(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
    null_terminated: bool,
    quote_path: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);

    // Renames render as a single `R  <old> -> <new>` record (Git porcelain v1
    // §B.6.3), never as two `R` endpoint rows. Under `-z` the record is
    // `XY SP <new> NUL <old> NUL` (raw path bytes, new before old, matching
    // Git); non-`-z` paths go through `quote_pathname` (§B.6.6).
    for entry in generate_short_status_entries_with_unmerged(staged, unstaged, unmerged) {
        match entry {
            ShortStatusEntry::Path {
                path,
                staged: x,
                unstaged: y,
            } => {
                if null_terminated {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    write_raw_path(writer, &path).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                } else {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&path, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b"\n").map_err(write_err)?;
                }
            }
            ShortStatusEntry::Rename {
                old,
                new,
                staged: x,
                unstaged: y,
            } => {
                if null_terminated {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    write_raw_path(writer, &new).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                    write_raw_path(writer, &old).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                } else {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&old, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b" -> ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&new, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b"\n").map_err(write_err)?;
                }
            }
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

/// Tri-state worktree mode. "Gone" and "unreadable" are different answers:
/// the first is representable (`000000`), the second must never be guessed.
enum WorktreeMode {
    Mode(u32),
    Gone,
    Unreadable,
}

/// Mode of an already repository-root-relative path. The porcelain v2
/// payload is projected to repo-root paths before rendering, so it must NOT
/// go through `current_to_workdir` a second time.
fn get_worktree_mode_result_for_workdir(workdir_path: &std::path::Path) -> WorktreeMode {
    // Debug-only seam: the RENDER-time unreadable branch is otherwise only
    // reachable by winning a race between collection and rendering, so tests
    // name the path that must report as unreadable.
    #[cfg(debug_assertions)]
    if std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_some()
        && let Ok(target) = std::env::var("LIBRA_TEST_UNREADABLE_MODE_PATH")
        && !target.is_empty()
        && workdir_path == std::path::Path::new(&target)
    {
        return WorktreeMode::Unreadable;
    }
    let abs_path = util::workdir_to_absolute(workdir_path);
    match std::fs::symlink_metadata(&abs_path) {
        Ok(metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                WorktreeMode::Mode(if metadata.file_type().is_symlink() {
                    0o120000
                } else if metadata.permissions().mode() & 0o111 != 0 {
                    0o100755
                } else {
                    0o100644
                })
            }
            #[cfg(not(unix))]
            {
                let _ = metadata;
                WorktreeMode::Mode(0o100644)
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => WorktreeMode::Gone,
        Err(_) => WorktreeMode::Unreadable,
    }
}

/// Worktree mode of an already repository-root-relative path, with the
/// pre-R0 lenient fallback. The porcelain v2 payload is projected once at
/// the render entry (`to_repo_relative`), so this must NOT re-project via
/// `current_to_workdir` — from a subdirectory that second projection made
/// every lookup miss and fabricated `100644` (2026-08-06 R0-5 review).
/// The fallback (vs the rename arm's hard error) is justified by the
/// collection phase failing closed on unreadable tracked paths first.
fn get_worktree_mode(workdir_path: &std::path::Path) -> u32 {
    match get_worktree_mode_result_for_workdir(workdir_path) {
        WorktreeMode::Mode(mode) => mode,
        _ => 0o100644,
    }
}

/// Whether `path` is recorded at stage 0 of `index` as a `160000` gitlink.
fn is_gitlink_index_entry(index: &Index, path: &str) -> bool {
    index
        .get(path, 0)
        .is_some_and(|entry| is_submodule_mode(entry.mode))
}

fn is_submodule_mode(mode: u32) -> bool {
    mode == 0o160000
}

fn get_submodule_status(_file_path: &std::path::Path) -> String {
    "S...".to_string()
}

fn build_porcelain_v2_data(
    index: Index,
    head_oid: Option<&ObjectHash>,
) -> CliResult<PorcelainV2Data> {
    let head_tree_items = if let Some(commit_hash) = head_oid {
        let (_, tree) = load_head_commit_tree(commit_hash)?;
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

    Ok(PorcelainV2Data {
        index,
        head_tree_items,
    })
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
    quote_path: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);
    // The porcelain payload was ALREADY projected to repository-root paths
    // (`to_repo_relative`), so these keys are used as-is. Converting again
    // would double the prefix when `status` runs from a subdirectory —
    // `sub/a.txt` became `sub/sub/a.txt`, the HEAD/index lookups missed, and
    // the record fail-closed instead of reporting its real mode and hash.
    let old_workdir = old.to_path_buf();
    let new_workdir = new.to_path_buf();

    // Staged rename: HEAD side is the OLD path, index side is the NEW path.
    // Unstaged-only rename (`.R`): there is no staged component, so Git copies
    // the index fields into the HEAD fields.
    let staged_rename = x == 'R';
    // §B.6.4 forbids fabricated all-zero hashes / default modes in a
    // rename record: a script that trusts `2 R…` must be able to trust
    // its mode+hash columns. Missing metadata is an internal
    // inconsistency, so fail closed with the offending path instead.
    let missing = |field: &str, path: &Path| -> CliError {
        CliError::fatal(format!(
            "cannot render the porcelain v2 rename record for '{}': {field} metadata is missing",
            path.display()
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
        .with_hint("re-run 'libra status' after 'libra add'/'libra reset' settles the index")
    };
    // The HEAD side comes from the HEAD TREE whenever the record has a real
    // staged component — `R.` (staged rename) and `MR`/`AR` (an unstaged
    // rename whose SOURCE also changed in the index) alike. Only a pure
    // `.R`, where HEAD and index agree by construction, copies the index
    // fields; doing that for `MR` would claim HEAD matches an index the user
    // just changed.
    let head_from_tree = staged_rename || x != '.';
    let (mode_head, hash_head) = if staged_rename {
        metadata
            .head_tree_items
            .get(&old_workdir)
            .map(|info| (info.mode, info.hash.clone()))
            .ok_or_else(|| missing("HEAD tree", &old_workdir))?
    } else if head_from_tree {
        // `MR`/`AR`: the source is the HEAD path. An `A` source has no HEAD
        // entry at all, which is exactly what `A` means, so the zero hash is
        // the honest answer there rather than a fabrication.
        match metadata.head_tree_items.get(&old_workdir) {
            Some(info) => (info.mode, info.hash.clone()),
            None if x == 'A' => (0, zero_hash.to_string()),
            None => return Err(missing("HEAD tree", &old_workdir)),
        }
    } else {
        // `.R`: filled from the index below (fixup).
        (0, zero_hash.to_string())
    };
    let index_key = if staged_rename {
        &new_workdir
    } else {
        &old_workdir
    };
    let index_str = index_key
        .to_str()
        .ok_or_else(|| missing("index (non-UTF-8 path)", index_key))?;
    let (mode_index, hash_index) = metadata
        .index
        .get(index_str, 0)
        .map(|entry| (entry.mode, entry.hash.to_string()))
        .ok_or_else(|| missing("index", index_key))?;
    let (mode_head, hash_head) = if staged_rename || head_from_tree {
        (mode_head, hash_head)
    } else {
        (mode_index, hash_index.clone())
    };
    // A worktree-deleted destination (`RD`) has no worktree entry: mW must
    // be 000000 like an ordinary v2 deleted row, not a fabricated 100644.
    let mode_worktree = if y == 'D' {
        0
    } else {
        // A mode read that FAILS is not `100644`. Between the scan and this
        // render the destination can be deleted or made unreadable; emitting
        // a fabricated regular-file mode would hand a script a value the
        // filesystem never reported. Fail closed instead — the same rule the
        // hash fields already follow.
        match get_worktree_mode_result_for_workdir(&new_workdir) {
            WorktreeMode::Mode(mode) => mode,
            // Genuinely absent (a chained rename already moved it on):
            // `000000`, the v2 spelling for "no worktree entry".
            WorktreeMode::Gone => 0,
            // Present but UNREADABLE: a fabricated `100644` would hand a
            // script a mode the filesystem never reported.
            WorktreeMode::Unreadable => {
                return Err(CliError::fatal(format!(
                    "cannot read the worktree mode of '{}' while rendering its rename record",
                    quote_pathname(new, true)
                ))
                .with_stable_code(StableErrorCode::IoReadFailed)
                .with_hint("re-run 'libra status' once the path is readable again"));
            }
        }
    };
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
        write_raw_path(writer, new).map_err(write_err)?;
        writer.write_all(b"\0").map_err(write_err)?;
        write_raw_path(writer, old).map_err(write_err)?;
        writer.write_all(b"\0").map_err(write_err)?;
    } else {
        writer
            .write_all(&quote_pathname_bytes(new, quote_path))
            .map_err(write_err)?;
        writer.write_all(b"\t").map_err(write_err)?;
        writer
            .write_all(&quote_pathname_bytes(old, quote_path))
            .map_err(write_err)?;
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
    quote_path: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let metadata =
        metadata.ok_or_else(|| CliError::internal("missing porcelain v2 metadata for status"))?;
    let zero_hash = zero_hash_str();
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);

    for entry in unmerged {
        write_unmerged_porcelain_v2(entry, &zero_hash, null_terminated, quote_path, writer)?;
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
        // A missing score is NOT 100: `R100` is the documented spelling of an
        // exact rename, so defaulting to it would publish an inexact pair as
        // byte-identical. The mode and hash fields already fail closed on
        // missing metadata; the score column gets the same treatment.
        let score = staged_rename_details
            .get(&(old.clone(), new.clone()))
            .map(|(pct, _)| *pct)
            .ok_or_else(|| {
                CliError::fatal(format!(
                    "cannot render the porcelain v2 rename record for '{}': score metadata is missing",
                    quote_pathname(new, quote_path)
                ))
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("re-run 'libra status' after 'libra add'/'libra reset' settles the index")
            })?;
        write_rename_porcelain_v2(
            old,
            new,
            'R',
            unstaged_char,
            score,
            metadata,
            &zero_hash,
            null_terminated,
            quote_path,
            writer,
        )?;
    }
    for (old, new) in &unstaged.renamed {
        endpoints.insert(old.clone());
        endpoints.insert(new.clone());
        let score = unstaged_rename_details
            .get(&(old.clone(), new.clone()))
            .map(|(pct, _)| *pct)
            .ok_or_else(|| {
                CliError::fatal(format!(
                    "cannot render the porcelain v2 rename record for '{}': score metadata is missing",
                    quote_pathname(new, quote_path)
                ))
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("re-run 'libra status' after 'libra add'/'libra reset' settles the index")
            })?;
        // The SOURCE of an unstaged rename may also carry a staged change
        // (edit `a`, `add a`, then move it to `b`). Git reports that as
        // `MR`, keeping the real HEAD and index sides distinct. Hardcoding
        // `.R` both lost the staged component — the endpoint row is
        // suppressed, so it vanished entirely — and made the record copy the
        // index hash into `hH`, claiming HEAD and index agree when they do
        // not.
        let staged_char = if staged.modified.contains(old) {
            'M'
        } else if staged.new.contains(old) {
            'A'
        } else {
            '.'
        };
        write_rename_porcelain_v2(
            old,
            new,
            staged_char,
            'R',
            score,
            metadata,
            &zero_hash,
            null_terminated,
            quote_path,
            writer,
        )?;
    }

    // An unresolved conflict lives ONLY in its `u` record: the
    // stage-0-less index also classifies the path as a staged deletion,
    // and without this exclusion the ordinary loop would emit a bogus
    // duplicate `1 D.` row for it (2026-08-06 R0-5 review).
    let unmerged_paths: std::collections::HashSet<&std::path::Path> =
        unmerged.iter().map(|entry| entry.path.as_path()).collect();
    let status_list = generate_short_format_status(staged, unstaged);
    for (file, staged_status, unstaged_status) in status_list {
        if endpoints.contains(&file) {
            continue;
        }
        if unmerged_paths.contains(file.as_path()) {
            continue;
        }
        if staged_status == '?' && unstaged_status == '?' {
            if null_terminated {
                write!(writer, "? ").map_err(write_err)?;
                write_raw_path(writer, &file).map_err(write_err)?;
            } else {
                write!(writer, "? ").map_err(write_err)?;
                writer
                    .write_all(&quote_pathname_bytes(&file, quote_path))
                    .map_err(write_err)?;
            }
            if null_terminated {
                writer.write_all(b"\0").map_err(write_err)?;
            } else {
                writer.write_all(b"\n").map_err(write_err)?;
            }
            continue;
        }

        // The porcelain payload is ALREADY repository-root-relative
        // (`to_repo_relative` at the render entry). Re-projecting through
        // `current_to_workdir` here made every index/HEAD lookup miss from
        // a subdirectory and fall back to fabricated `100644`/zero-hash
        // metadata (2026-08-06 R0-5 review).
        let workdir_path = file.clone();
        let file_str = workdir_path.to_str().unwrap_or_default();

        let (mode_index, hash_index) = if let Some(entry) = metadata.index.get(file_str, 0) {
            (entry.mode, entry.hash.to_string())
        } else if staged_status == 'D' {
            // Semantically absent: the deletion is staged, so stage 0 has
            // no entry — Git v2 spells that `000000` plus the zero hash.
            (0, zero_hash.clone())
        } else {
            // Every other `1` row REQUIRES a stage-0 entry; fabricating
            // `100644` here would forge metadata for a lookup that must
            // not miss (2026-08-06 R0-5 review).
            return Err(CliError::fatal(format!(
                "missing index entry for '{file_str}' while rendering porcelain v2"
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid));
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
            match get_worktree_mode_result_for_workdir(&workdir_path) {
                WorktreeMode::Mode(mode) => mode,
                // A path absent from the worktree (e.g. a staged deletion's
                // `D.` row) is semantically gone: `000000`, like Git.
                WorktreeMode::Gone => 0,
                // Unreadable is a different answer from gone and must never
                // be spelled `100644` (2026-08-06 R0-5 review, mirroring
                // the rename-record arm).
                WorktreeMode::Unreadable => {
                    return Err(CliError::fatal(format!(
                        "cannot read the worktree mode of '{file_str}' while rendering \
                         porcelain v2"
                    ))
                    .with_stable_code(StableErrorCode::IoReadFailed));
                }
            }
        };

        let sub = if is_submodule_mode(mode_index) || is_submodule_mode(mode_head) {
            get_submodule_status(&file)
        } else {
            "N...".to_string()
        };

        // Git porcelain v2 spells an unmodified side as `.`, never the
        // v1-style space — `1  M` instead of `1 .M` breaks fixed-column
        // consumers (2026-08-06 R0-5 review).
        let dot = |status: char| if status == ' ' { '.' } else { status };
        write!(
            writer,
            "1 {}{} {} {} {} {} {} {} ",
            dot(staged_status),
            dot(unstaged_status),
            sub,
            format_mode(mode_head),
            format_mode(mode_index),
            format_mode(mode_worktree),
            hash_head,
            hash_index,
        )
        .map_err(write_err)?;
        if null_terminated {
            // `1` rows always carry UTF-8 index paths today, but the `-z`
            // wire format is raw bytes by contract.
            write_raw_path(writer, &file).map_err(write_err)?;
            writer.write_all(b"\0").map_err(write_err)?;
        } else {
            writer
                .write_all(&quote_pathname_bytes(&file, quote_path))
                .map_err(write_err)?;
            writer.write_all(b"\n").map_err(write_err)?;
        }
    }

    for file in ignored {
        if null_terminated {
            write!(writer, "! ").map_err(write_err)?;
            write_raw_path(writer, file).map_err(write_err)?;
        } else {
            write!(writer, "! ").map_err(write_err)?;
            writer
                .write_all(&quote_pathname_bytes(file, quote_path))
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

fn zero_hash_str() -> String {
    ObjectHash::zero_str(get_hash_kind())
}

fn write_unmerged_porcelain_v2(
    entry: &UnmergedEntry,
    zero_hash: &str,
    null_terminated: bool,
    quote_path: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);
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
        "u {}{} N... {} {} {} {} {} {} {} ",
        staged_status,
        unstaged_status,
        mode(1),
        mode(2),
        mode(3),
        format_mode(get_unmerged_worktree_mode(&entry.path)),
        hash(1),
        hash(2),
        hash(3)
    )
    .map_err(write_err)?;
    // §B.6.6: `-z` carries RAW path bytes (a non-UTF-8 name must survive
    // byte-for-byte), every other mode carries the C-style-escaped form.
    // `display()` did neither — it lossily replaced undecodable bytes and
    // left control characters unescaped, which can break the line format.
    if null_terminated {
        write_raw_path(writer, &entry.path).map_err(write_err)?;
        writer.write_all(b"\0").map_err(write_err)?;
    } else {
        writer
            .write_all(&quote_pathname_bytes(&entry.path, quote_path))
            .map_err(write_err)?;
        writer.write_all(b"\n").map_err(write_err)?;
    }
    Ok(())
}

/// `u`-row worktree mode; the unmerged payload is repository-root-relative
/// like the rest of the projected porcelain data (2026-08-06 R0-5 review:
/// same double-projection fix as `get_worktree_mode`).
fn get_unmerged_worktree_mode(workdir_path: &std::path::Path) -> u32 {
    let abs_path = util::workdir_to_absolute(workdir_path);
    if std::fs::symlink_metadata(&abs_path).is_ok() {
        get_worktree_mode(workdir_path)
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Short format
// ---------------------------------------------------------------------------

/// Core logic for generating short format status without color (for testing).
///
/// LEGACY tuple API (pre-R0 shape, §B.6.0.1): renames are DECOMPOSED into
/// their endpoint states — a staged rename contributes `old = D ` / `new =
/// A `, an unstaged rename contributes an unstaged `D` on its source (merged
/// with any staged state via the ordinary rules) and `??` on its
/// destination — so a chain `a→b` staged + `b→c` unstaged yields `a = D `,
/// `b = AD`, `c = ??` with no duplicate rows. Rename-aware consumers use
/// [`generate_short_status_entries`] instead.
pub fn generate_short_format_status(
    staged: &Changes,
    unstaged: &Changes,
) -> Vec<(std::path::PathBuf, char, char)> {
    generate_short_format_status_with_unmerged(staged, unstaged, &[])
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

/// Shared base XY map: every non-rename change plus unmerged entries. Rename
/// pairs are handled by the caller (decomposed by the legacy API, first-class
/// in [`generate_short_status_entries`]).
fn short_xy_base(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
) -> HashMap<PathBuf, (char, char)> {
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

    process_unstaged_changes(&unstaged.modified, &mut file_status, 'M');
    process_unstaged_changes(&unstaged.deleted, &mut file_status, 'D');

    for file in &unstaged.new {
        file_status.insert(file.clone(), ('?', '?'));
    }
    for entry in unmerged {
        file_status.insert(entry.path.clone(), entry.xy());
    }
    file_status
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
    // Pre-R0 decomposition: a staged rename is a delete of the old path plus
    // an add of the new path in this legacy tuple view.
    for (old, new) in &staged.renamed {
        file_status.insert(old.clone(), ('D', ' '));
        file_status.insert(new.clone(), ('A', ' '));
    }

    process_unstaged_changes(&unstaged.modified, &mut file_status, 'M');
    process_unstaged_changes(&unstaged.deleted, &mut file_status, 'D');
    // Pre-R0 decomposition: an unstaged rename is an unstaged delete of its
    // source (merged with any staged state) …
    for (old, _new) in &unstaged.renamed {
        process_unstaged_changes(std::slice::from_ref(old), &mut file_status, 'D');
    }

    for file in &unstaged.new {
        file_status.insert(file.clone(), ('?', '?'));
    }
    // … and an untracked destination.
    for (_old, new) in &unstaged.renamed {
        file_status.insert(new.clone(), ('?', '?'));
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

/// One short-format / porcelain-v1 render entry (§B.6.1 public API): either a
/// plain per-path change or a first-class rename pair (rendered with Git's
/// `old -> new` arrow instead of two endpoint rows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShortStatusEntry {
    Path {
        path: PathBuf,
        staged: char,
        unstaged: char,
    },
    Rename {
        old: PathBuf,
        new: PathBuf,
        staged: char,
        unstaged: char,
    },
}

impl ShortStatusEntry {
    /// Sort key — Git orders renames by their destination path.
    fn sort_key(&self) -> &Path {
        match self {
            ShortStatusEntry::Path { path, .. } => path,
            ShortStatusEntry::Rename { new, .. } => new,
        }
    }
}

/// Build the shared short-format / porcelain-v1 entry list (§B.6.1): rename
/// pairs stay first-class — the unstaged column of a staged rename carries
/// the DESTINATION's worktree state (`RM`/`RD`) — and every non-endpoint
/// path renders as an XY tuple. Entries sort by path, renames by their
/// destination.
pub fn generate_short_status_entries(
    staged: &Changes,
    unstaged: &Changes,
) -> Vec<ShortStatusEntry> {
    generate_short_status_entries_with_unmerged(staged, unstaged, &[])
}

pub(crate) fn generate_short_status_entries_with_unmerged(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
) -> Vec<ShortStatusEntry> {
    let mut entries: Vec<ShortStatusEntry> = Vec::new();
    let mut endpoints: HashSet<PathBuf> = HashSet::new();
    for (old, new) in &staged.renamed {
        endpoints.insert(old.clone());
        endpoints.insert(new.clone());
        // The endpoint rows are suppressed, so this column is the only
        // signal for the destination's worktree state.
        let unstaged_char = if unstaged.modified.contains(new) {
            'M'
        } else if unstaged.deleted.contains(new) {
            'D'
        } else {
            ' '
        };
        entries.push(ShortStatusEntry::Rename {
            old: old.clone(),
            new: new.clone(),
            staged: 'R',
            unstaged: unstaged_char,
        });
    }
    for (old, new) in &unstaged.renamed {
        endpoints.insert(old.clone());
        endpoints.insert(new.clone());
        // The suppressed source row is the only carrier of the SOURCE's
        // staged state: a staged-modify→worktree-rename is `MR`, a
        // staged-add→worktree-rename `AR` — hard-coding a space here
        // erased that component while porcelain v2 derived it correctly
        // (2026-08-06 R0-6 review, mirroring the v2 derivation).
        let staged_char = if staged.modified.contains(old) {
            'M'
        } else if staged.new.contains(old) {
            'A'
        } else {
            ' '
        };
        entries.push(ShortStatusEntry::Rename {
            old: old.clone(),
            new: new.clone(),
            staged: staged_char,
            unstaged: 'R',
        });
    }

    let mut base: Vec<_> = short_xy_base(staged, unstaged, unmerged)
        .into_iter()
        .collect();
    base.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, (staged_char, unstaged_char)) in base {
        if endpoints.contains(&path) {
            continue;
        }
        entries.push(ShortStatusEntry::Path {
            path,
            staged: staged_char,
            unstaged: unstaged_char,
        });
    }
    entries.sort_by(|a, b| a.sort_key().cmp(b.sort_key()));
    entries
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
        true,
        writer,
    )
    .await
}

/// C-style path quoting for human-short and non-`-z` porcelain output
/// (§B.6.6). Control characters, `"` and `\` are ALWAYS escaped; bytes above
/// 0x7F are additionally escaped as octal `\ooo` only under
/// `core.quotePath=true` (the default, matching Git). A path needing any
/// escape is wrapped in double quotes; `-z` output never calls this.
/// §B.6.0.1 reason taxonomy → JSON reason string + warning code.
pub(crate) fn io_blocked_reason_and_code(
    reason: crate::command::status_probe::IoBlockedReason,
) -> (&'static str, StatusWarningCode) {
    use crate::command::status_probe::IoBlockedReason;

    match reason {
        IoBlockedReason::PermissionDenied => (
            "permission_denied",
            StatusWarningCode::WorktreePermissionDenied,
        ),
        IoBlockedReason::IoError => ("io_error", StatusWarningCode::WorktreeReadFailed),
        IoBlockedReason::IoTimeout => ("io_timeout", StatusWarningCode::WorktreeIoTimeout),
    }
}

/// Write a path under `-z` as RAW OS bytes (Git parity: `-z` never quotes,
/// and on Unix a non-UTF-8 name keeps its exact bytes on the wire). Non-Unix
/// platforms fall back to the platform's stable `display()` encoding.
fn write_raw_path(writer: &mut impl Write, path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        writer.write_all(path.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        write!(writer, "{}", path.display())
    }
}

/// Public so the wave-0 suite can pin the §B.6.6 escape matrix on paths
/// (LF/CR) that cannot be created as files on every filesystem.
pub fn quote_pathname(path: &Path, quote_path: bool) -> String {
    let bytes = quote_pathname_bytes(path, quote_path);
    match String::from_utf8(bytes) {
        Ok(text) => text,
        // String-typed callers can never hold raw non-UTF-8 bytes, so they
        // keep the lossless octal-escaped form (every byte maps 1:1).
        // Byte-oriented writers call `quote_pathname_bytes` and stay raw
        // when `core.quotePath` is off (Git parity).
        Err(_) => {
            String::from_utf8(quote_pathname_bytes(path, true)).unwrap_or_else(|error| {
                // INVARIANT: the quote_path=true form octal-escapes every
                // byte >= 0x80, so it is pure ASCII and always valid UTF-8.
                String::from_utf8_lossy(error.as_bytes()).into_owned()
            })
        }
    }
}

/// Byte-faithful variant of [`quote_pathname`] for byte-oriented writers
/// (§B.6.6): TAB/LF/CR/`"`/`\` are always escaped; bytes >= 0x80 are
/// octal-escaped only while `quote_path` holds — with `core.quotePath=false`
/// they are written RAW, including when the path is not valid UTF-8 (Git
/// parity). `-z` surfaces never call this: they are raw end to end.
pub fn quote_pathname_bytes(path: &Path, quote_path: bool) -> Vec<u8> {
    // Escape the RAW OS path bytes (Unix), not a lossy `display()` copy, so
    // a non-UTF-8 name renders its true bytes (`\377`) instead of U+FFFD
    // replacement bytes. On non-Unix `display()` is the platform's stable
    // encoding.
    #[cfg(unix)]
    let bytes: &[u8] = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes()
    };
    #[cfg(not(unix))]
    let display = path.display().to_string();
    #[cfg(not(unix))]
    let bytes: &[u8] = display.as_bytes();
    let needs_escape =
        |b: u8| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || (quote_path && b >= 0x80);
    if !bytes.iter().copied().any(needs_escape) {
        return bytes.to_vec();
    }
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 8);
    out.push(b'"');
    for &b in bytes {
        match b {
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            _ if b < 0x20 || b == 0x7f || (quote_path && b >= 0x80) => {
                out.extend_from_slice(format!("\\{b:03o}").as_bytes());
            }
            _ => out.push(b),
        }
    }
    out.push(b'"');
    out
}

/// Short format output with color controlled by OutputConfig.
///
/// Renames are rendered as a single `R  <old> -> <new>` line (Git's short
/// rename form), not as two separate `R` rows (§B.6.1). Under `-z` the record
/// is `XY SP <new> NUL <old> NUL` (new before old, matching Git) with RAW
/// unquoted paths; non-`-z` paths go through [`quote_pathname`] (§B.6.6).
async fn output_short_format_with_config(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
    output: &OutputConfig,
    null_terminated: bool,
    quote_path: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let use_colors = should_use_colors(output).await;
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);

    for entry in generate_short_status_entries_with_unmerged(staged, unstaged, unmerged) {
        match entry {
            ShortStatusEntry::Path {
                path,
                staged: x,
                unstaged: y,
            } => {
                if null_terminated {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    write_raw_path(writer, &path).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                } else if use_colors {
                    // Only the XY letters are colored; the path is appended
                    // byte-faithfully so `core.quotePath=false` keeps raw
                    // high bytes even under forced color.
                    let head = format_colored_status(x, y, "");
                    writer.write_all(head.as_bytes()).map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&path, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b"\n").map_err(write_err)?;
                } else {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&path, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b"\n").map_err(write_err)?;
                }
            }
            ShortStatusEntry::Rename {
                old,
                new,
                staged: x,
                unstaged: y,
            } => {
                if null_terminated {
                    // `XY SP <new> NUL <old> NUL` (§B.6.1), raw path bytes.
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    write_raw_path(writer, &new).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                    write_raw_path(writer, &old).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                } else if use_colors {
                    // Same byte-faithful shape as the Path arm: colored XY,
                    // then raw quoted bytes, then the arrow and the new path.
                    let head = format_colored_status(x, y, "");
                    writer.write_all(head.as_bytes()).map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&old, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b" -> ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&new, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b"\n").map_err(write_err)?;
                } else {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&old, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b" -> ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&new, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b"\n").map_err(write_err)?;
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

fn format_colored_status(staged_status: char, unstaged_status: char, file: &str) -> String {
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

    format!("{colored_staged}{colored_unstaged} {file}")
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
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);
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
                    match (u.ahead, u.behind) {
                        (Some(ahead), Some(behind)) if ahead > 0 && behind > 0 => {
                            format!("## {tracking} [ahead {ahead}, behind {behind}]")
                        }
                        (Some(ahead), Some(_)) if ahead > 0 => {
                            format!("## {tracking} [ahead {ahead}]")
                        }
                        (Some(_), Some(behind)) if behind > 0 => {
                            format!("## {tracking} [behind {behind}]")
                        }
                        // Up to date, or no counts (unborn branch, or counts
                        // that could not be computed).
                        _ => format!("## {tracking}"),
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
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);
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
        // Like Git, `# branch.ab` is only written when the counts exist: an
        // unborn branch or uncountable history omits the line rather than
        // claiming `+0 -0`.
        if !u.gone
            && show_ahead_behind
            && let (Some(ahead), Some(behind)) = (u.ahead, u.behind)
        {
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
    let tracked_files = index.tracked_files();

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
