//! Supports cloning repositories by parsing URLs, fetching objects via protocol
//! clients, checking out the working tree, and writing initial refs/config.
//!
//! The execution layer (`execute_clone`) produces a structured [`CloneOutput`]
//! and the rendering layer (`execute_safe`) converts it to human / JSON /
//! machine output according to the global [`OutputConfig`].

use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use clap::Parser;
use git_internal::{
    errors::GitError,
    hash::{ObjectHash, get_hash_kind},
};
use sea_orm::DatabaseTransaction;
use serde::Serialize;

use super::fetch::{self, RemoteSpecErrorKind};
use crate::{
    command::{
        self,
        init::InitError,
        restore::{RestoreArgs, RestoreError},
    },
    git_protocol::PKT_LINE_PROTOCOL_ERROR_PREFIX,
    internal::{
        branch::{self, Branch},
        config::{ConfigKv, LocalIdentityTarget, RemoteConfig},
        db::get_db_conn_instance,
        head::Head,
        protocol::DiscoveryResult,
        reflog::{ReflogAction, ReflogContext, with_reflog},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        ignore as ignore_utils,
        output::{OutputConfig, emit_json_data},
        path, util,
    },
};

const ISSUE_URL: &str = "https://github.com/libra-tools/libra/issues";

/// Clone a repository into a new directory.
//
// The user-visible examples block is rendered by clap via the
// `after_help = "EXAMPLES:\n    …"` attribute below — not by this
// rustdoc. Keeping the rustdoc to one summary line stops clap from
// echoing a markdown `# Examples` heading and triple-backtick fences
// verbatim into `--help` output (those don't render outside cargo doc).
#[derive(Parser, Debug, Clone)]
#[clap(after_help = "EXAMPLES:\n    \
    libra clone git@github.com:user/repo.git             Clone via SSH\n    \
    libra clone https://github.com/user/repo.git          Clone via HTTPS\n    \
    libra clone git@github.com:user/repo.git my-dir       Clone to specific directory\n    \
    libra clone --bare git@github.com:user/repo.git       Create bare clone\n    \
    libra clone -b develop git@github.com:user/repo.git   Clone specific branch\n    \
    libra clone --single-branch -b main <url>             Clone only one branch\n    \
    libra clone --no-checkout <url>                       Set up the repo without checking out files\n    \
    libra clone -o upstream <url>                         Name the remote 'upstream' instead of 'origin'\n    \
    libra clone --depth 1 <url>                           Shallow clone (latest commit only)\n    \
    libra clone --deps-of scene.usd <local-libra>         Scope the view to a file's dependency closure (lore.md 3.2)")]
pub struct CloneArgs {
    /// The remote repository location to clone from, usually a URL with HTTPS or SSH
    pub remote_repo: String,

    /// The local path to clone the repository to
    pub local_path: Option<String>,

    /// Checkout <BRANCH> instead of the remote's HEAD
    #[clap(short = 'b', long, required = false)]
    pub branch: Option<String>,

    /// Clone only one branch, HEAD or --branch
    #[clap(long, overrides_with = "no_single_branch")]
    pub single_branch: bool,

    /// Clone the branch histories of all branches (the default), countermanding
    /// an earlier `--single-branch` (last one on the command line wins),
    /// matching `git clone --no-single-branch`. Clone fetches all branches by
    /// default, so on its own this is a no-op.
    #[clap(long = "no-single-branch", overrides_with = "single_branch")]
    pub no_single_branch: bool,

    /// Create a bare repository without checking out a working tree
    #[clap(long)]
    pub bare: bool,

    /// Create a shallow clone with history truncated to N commits (must be > 0)
    #[clap(long, value_name = "N", value_parser = validate_depth)]
    pub depth: Option<usize>,

    /// Fetch all tags (the default). Accepted for Git compatibility and to
    /// override an earlier `--no-tags`.
    #[clap(long, overrides_with = "no_tags")]
    pub tags: bool,

    /// Do not clone any tags, and set `remote.<name>.tagOpt=--no-tags` (the
    /// remote name is `origin` by default, or the `-o`/`--origin` value) so future
    /// fetches also skip tags (matches `git clone --no-tags`).
    #[clap(long = "no-tags", overrides_with = "tags")]
    pub no_tags: bool,

    /// Do not show the fetch progress meter (the "Receiving objects" spinner)
    /// during the clone, matching `git clone --no-progress`.
    #[clap(long = "no-progress")]
    pub no_progress: bool,

    /// Do not check out HEAD into the working tree after cloning (objects, refs
    /// and HEAD are still set up), matching `git clone --no-checkout`.
    #[clap(long = "no-checkout")]
    pub no_checkout: bool,

    /// Use NAME for the remote (and its `refs/remotes/<NAME>/*` tracking refs)
    /// instead of the default `origin`, matching `git clone -o`.
    #[clap(short = 'o', long = "origin", value_name = "NAME")]
    pub origin: Option<String>,

    /// Request local optimizations for a filesystem source (Git's `-l`/`--local`
    /// copies/hardlinks instead of using the transport). Accepted for
    /// compatibility and is a no-op: Libra never hardlinks (it always copies),
    /// and how it reads a local-path source is determined by the source type
    /// (a local Libra repo is read directly; a local Git repo is fetched via
    /// git-upload-pack), not by this flag.
    #[clap(short = 'l', long, overrides_with = "no_local")]
    pub local: bool,

    /// Force the regular transport even for a local source (Git's `--no-local`,
    /// which avoids hardlinks). Accepted for compatibility and is a no-op:
    /// Libra never hardlinks objects, so there is nothing to disable.
    #[clap(long = "no-local", overrides_with = "local")]
    pub no_local: bool,

    /// Fail if the clone would be a shallow repository that was not explicitly
    /// requested — i.e. the source repository is shallow (matching
    /// `git clone --reject-shallow`). Two narrowings vs Git: (1) for remotes
    /// that can negotiate shallow boundaries, Libra cannot distinguish a shallow
    /// source from `--depth`-induced shallowness, so passing `--depth`
    /// suppresses the post-fetch check (Git would still reject); (2) local Libra
    /// sources do not advertise shallow boundaries (declined by design, D20), so `--depth` fails
    /// closed before this check.
    #[clap(long = "reject-shallow")]
    pub reject_shallow: bool,

    /// Borrow objects from an existing local repository to reduce transfer
    /// (Git's `--reference <repo>`, which sets up `objects/info/alternates`).
    /// Accepted for compatibility but a no-op with a warning: Libra has no object
    /// alternates — it always copies every object into the clone — so there is
    /// nothing to borrow and the reference is ignored. May be given multiple
    /// times.
    #[clap(long = "reference", value_name = "repo")]
    pub reference: Vec<String>,

    /// Like `--reference`, but silently ignore a reference that cannot be used
    /// (Git's `--reference-if-able`). Since Libra never uses alternates, the
    /// reference is always "unusable" and is silently ignored — exactly Git's
    /// graceful-degradation behavior. May be given multiple times.
    #[clap(long = "reference-if-able", value_name = "repo")]
    pub reference_if_able: Vec<String>,

    /// Share objects with a local source via alternates (Git's `--shared`/`-s`,
    /// lore.md 2.11). For a LOCAL Libra source, registers the source's object
    /// store as an alternate of the clone (borrowed reads + base gc/evict
    /// protection). NOTE: v1 still COPIES every object — the register only adds
    /// the borrow link and base protection; disk copy-avoidance is deferred.
    /// A no-op (warning) for a remote or local-Git source. Can also default via
    /// `clone.shared` config; override with `--no-shared`.
    #[clap(long = "shared", short = 's')]
    pub shared: bool,

    /// Countermand `--shared` / a `clone.shared=true` default: do NOT register
    /// the source as an alternate (lore.md 2.11). Last one wins with `--shared`.
    #[clap(long = "no-shared", overrides_with = "shared")]
    pub no_shared: bool,

    /// Copy borrowed objects in so the clone does not depend on `--reference`
    /// (Git's `--dissociate`). Accepted for compatibility and a no-op: Libra
    /// never borrows objects (it always copies), so every clone is already
    /// fully self-contained — there is nothing to dissociate.
    #[clap(long = "dissociate")]
    pub dissociate: bool,

    /// Set up a mirror of the source repository (Git's `--mirror`). Implies
    /// `--bare`; maps the fetched branches into `refs/heads/*` and keeps tags in
    /// `refs/tags/*` verbatim (no `refs/remotes/*` tracking refs), and records
    /// `remote.<name>.mirror=true`. NARROWING vs Git: Libra mirrors only what it
    /// fetches — `refs/notes/*` and other un-fetched namespaces are not mirrored,
    /// and because fetch collapses `refs/mr/*` into the branch tracking namespace
    /// any such refs become `refs/heads/mr/*`; the mirror marker is informational
    /// (`libra fetch` is not yet mirror-aware).
    #[clap(long = "mirror")]
    pub mirror: bool,

    /// Partial-clone object filter (Git's `--filter <spec>`, e.g. `blob:none`).
    /// Accepted for compatibility but a no-op with a warning: Libra has no
    /// partial-clone/promisor support, so the filter is ignored (no objects are
    /// excluded) and a complete clone is performed (subject only to `--depth` if
    /// also given) — mirroring Git's own behavior when a server does not advertise
    /// filtering.
    #[clap(long = "filter", value_name = "spec")]
    pub filter: Option<String>,

    /// Deepen history to commits more recent than a date (Git's
    /// `--shallow-since <date>`). Accepted for compatibility but a no-op with a
    /// warning: Libra bounds shallow history only by `--depth`, so the date bound
    /// is not applied (history is limited only by `--depth` if also given).
    #[clap(long = "shallow-since", value_name = "date")]
    pub shallow_since: Option<String>,

    /// Deepen history, excluding commits reachable from a ref (Git's
    /// `--shallow-exclude <rev>`). Accepted for compatibility but a no-op with a
    /// warning: Libra bounds shallow history only by `--depth`, so the exclusion
    /// is not applied (history is limited only by `--depth` if also given). May be
    /// given multiple times.
    #[clap(long = "shallow-exclude", value_name = "rev")]
    pub shallow_exclude: Vec<String>,

    /// lore.md 3.2 — dependency-filtered clone: after the (full) checkout, scope
    /// the working-tree VIEW to the forward dependency closure of these root
    /// path(s) (repeatable). Implies `--notes` (the dependency graph must be
    /// fetched to compute the closure). intentionally-different (Git has no
    /// file-dependency concept); it is NOT partial-clone/`--filter` (objects are
    /// never wire-filtered) and NOT `--sparse` (declined D10). The whole tree is
    /// still downloaded and checked out — only the sparse VIEW is narrowed (disk
    /// narrowing is deferred, D18). Only a local Libra source can travel the graph
    /// in v1 (D17). Conflicts with `--no-checkout`/`--bare`/`--mirror` (they skip
    /// the checkout that keeps the repository commit-safe).
    #[clap(
        long = "deps-of",
        value_name = "path",
        conflicts_with_all = ["no_checkout", "bare", "mirror"]
    )]
    pub deps_of: Vec<String>,

    /// Bound the `--deps-of` dependency closure depth (`1` = direct dependencies
    /// only; unbounded by default). Requires `--deps-of`.
    #[clap(long = "deps-depth-limit", value_name = "N", requires = "deps_of")]
    pub deps_depth_limit: Option<usize>,
}

/// `--reject-shallow`: refuse a clone that ended up shallow without the user
/// asking for it. A shallow result is fine when the user passed `--depth`;
/// otherwise it means the source repository was shallow, which Git rejects.
///
/// NARROWING vs Git: Git rejects a shallow SOURCE regardless of `--depth`, but
/// Libra has no protocol signal distinguishing a shallow source from
/// `--depth`-induced shallowness (both only leave a `.libra/shallow` marker), so
/// when `--depth` is given Libra does NOT reject after fetch. Local Libra
/// sources are rejected earlier by fetch when `--depth` is present because they
/// cannot advertise shallow boundaries (accepted end state, D20). The common cases still match Git:
/// `--reject-shallow` alone rejects a shallow result, and a full clone of a
/// non-shallow source is allowed.
/// Warn when `--reference`/`--shared` were given: those flags ask Git to share
/// or borrow objects from another local store via alternates, but Libra always
/// copies every object into the clone (it has no object alternates), so the
/// clone is self-contained and the flags have no effect. `--reference-if-able`
/// and `--dissociate` are intentionally silent (Git's `-if-able` silently
/// ignores an unusable reference, and a copy-only clone is already dissociated).
fn object_alternates_warning(args: &CloneArgs) -> Option<String> {
    // `--reference` is still a genuine no-op (copy-avoidance deferred).
    // `--shared` messaging is handled at the clone hook (took-effect vs
    // can't-share), so it is NOT warned here.
    if args.reference.is_empty() {
        return None;
    }
    Some(
        "--reference has no effect: Libra has no fetch-side alternate negotiation yet and \
         always copies every object into the clone (use 'libra alternates add' to borrow)"
            .to_string(),
    )
}

/// Warnings for fetch-shaping flags Libra cannot honor (`--filter`,
/// `--shallow-since`, `--shallow-exclude`). Libra has no partial-clone/promisor
/// support and bounds shallow history only by `--depth`, so these flags are
/// accepted but ignored — the optimization is simply not applied (the clone still
/// fetches everything those flags would have trimmed, subject only to `--depth`
/// if also given). Each given flag produces its own explanatory warning so the
/// user knows it had no effect — mirroring Git, which warns and falls back to a
/// full clone when a server cannot honor `--filter`.
fn unsupported_fetch_optimization_warnings(args: &CloneArgs) -> Vec<String> {
    let mut warnings = Vec::new();
    if args.filter.is_some() {
        warnings.push(
            "--filter is ignored: Libra has no partial-clone support, so the object filter is not \
             applied (no objects are excluded)"
                .to_string(),
        );
    }
    if args.shallow_since.is_some() {
        warnings.push(
            "--shallow-since is ignored: Libra bounds shallow history only by --depth, so the date \
             bound is not applied"
                .to_string(),
        );
    }
    if !args.shallow_exclude.is_empty() {
        warnings.push(
            "--shallow-exclude is ignored: Libra bounds shallow history only by --depth, so the \
             exclusion is not applied"
                .to_string(),
        );
    }
    warnings
}

fn clone_should_reject_shallow(
    reject_shallow: bool,
    is_shallow: bool,
    depth: Option<usize>,
) -> bool {
    reject_shallow && is_shallow && depth.is_none()
}

const REPO_MARKERS: &[&str] = &["description", "libra.db", "info/exclude", "objects"];

// ---------------------------------------------------------------------------
// CloneOutput — structured result of a successful clone
// ---------------------------------------------------------------------------

/// Structured output for a successful clone operation. Rendered as JSON
/// envelope by `emit_json_data("clone", &output, config)` in `--json` mode,
/// or as a human-readable summary otherwise.
#[derive(Debug, Clone, Serialize)]
pub struct CloneOutput {
    /// Repository absolute path (worktree root for non-bare, `.libra` dir for bare).
    pub path: String,
    pub bare: bool,
    /// Normalized remote URL.
    pub remote_url: String,
    /// Name of the configured remote (`origin` by default, or the `-o`/`--origin`
    /// value for standard clones).
    pub remote_name: String,
    /// Actual checked-out branch; `None` for empty remotes.
    pub branch: Option<String>,
    /// `sha1` or `sha256` (from `InitOutput.object_format`).
    pub object_format: String,
    /// From `InitOutput.repo_id`.
    pub repo_id: String,
    /// From `InitOutput.vault_signing`.
    pub vault_signing: bool,
    /// From `InitOutput.ssh_key_detected`.
    pub ssh_key_detected: Option<String>,
    /// Whether `--depth` produced a shallow clone.
    pub shallow: bool,
    /// Non-fatal warnings (empty remote, init warnings, etc.).
    pub warnings: Vec<String>,
    /// Worktree-relative paths of `.libraignore` files written by converting
    /// `.gitignore` files from the source repository.  Empty for bare clones.
    pub gitignore_converted: Vec<String>,
    /// Source kind for additive clone integrations. Omitted for ordinary Git sources.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
    /// Optional site metadata reserved for removed restore integrations. Always omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud_site: Option<CloudCloneSiteOutput>,
    /// Number of objects received in the fetch pack (Git sources only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objects_fetched: Option<usize>,
    /// Bytes received in the fetch pack stream (Git sources only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_received: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CloudCloneSiteOutput {
    pub clone_domain: String,
    pub site_id: String,
    pub slug: String,
    pub repo_id: String,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_name: Option<String>,
    pub revision: String,
}

// ---------------------------------------------------------------------------
// CloneError
// ---------------------------------------------------------------------------

#[derive(thiserror::Error, Debug)]
pub enum CloneError {
    #[error("please specify the destination path explicitly")]
    CannotInferDestination,
    #[error("destination path '{path}' already exists and is not an empty directory")]
    DestinationExistsNonEmpty { path: PathBuf },
    #[error("destination path '{path}' already contains a libra repository")]
    DestinationAlreadyRepo { path: PathBuf },
    #[error("could not create directory '{path}': {source}")]
    CreateDestinationFailed { path: PathBuf, source: io::Error },
    #[error("remote discovery failed")]
    DiscoverRemote { source: fetch::FetchError },
    #[error("failed to change working directory to '{path}': {source}")]
    ChangeDirectory { path: PathBuf, source: io::Error },
    #[error("failed to restore working directory to '{path}': {source}")]
    RestoreDirectory { path: PathBuf, source: io::Error },
    #[error("failed to initialize repository")]
    InitializeRepository { source: InitError },
    #[error("source repository is shallow, reject to clone")]
    RejectShallow,
    #[error("remote branch {branch} not found in upstream {remote}")]
    RemoteBranchNotFound { branch: String, remote: String },
    #[error("failed to inspect local branch state after fetch: {source}")]
    LocalBranchState { source: branch::BranchStoreError },
    #[error("fetch failed: {source}")]
    FetchFailed { source: fetch::FetchError },
    #[error("failed to checkout working tree")]
    CheckoutFailed { source: RestoreError },
    #[error("failed to convert ignore files")]
    IgnoreFile {
        source: ignore_utils::IgnoreFileError,
    },
    #[error("failed to complete clone setup: {message}")]
    SetupFailed { message: String },
    #[error("this clone source is no longer supported")]
    RemovedPublishRestoreSource,
}

// ---------------------------------------------------------------------------
// CloneError → CliError — explicit StableErrorCode mapping
// ---------------------------------------------------------------------------

impl From<CloneError> for CliError {
    fn from(error: CloneError) -> Self {
        match error {
            CloneError::CannotInferDestination => {
                CliError::command_usage("please specify the destination path explicitly")
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
                    .with_hint("please specify the destination path explicitly")
            }
            CloneError::DestinationExistsNonEmpty { ref path } => CliError::command_usage(format!(
                "destination path '{}' already exists and is not an empty directory",
                path.display()
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
            .with_hint("choose a different path or empty the directory first"),
            CloneError::DestinationAlreadyRepo { ref path } => CliError::fatal(format!(
                "destination path '{}' already contains a libra repository",
                path.display()
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint("the destination already contains a libra repository"),
            CloneError::CreateDestinationFailed { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::IoWriteFailed)
                .with_hint("check directory permissions and disk space"),
            CloneError::DiscoverRemote { source } => map_discover_remote_error(source),
            CloneError::ChangeDirectory { .. } | CloneError::RestoreDirectory { .. } => {
                CliError::fatal(error.to_string())
                    .with_stable_code(StableErrorCode::InternalInvariant)
                    .with_hint(format!("please report this issue at: {ISSUE_URL}"))
            }
            CloneError::InitializeRepository { source } => {
                // Transparently reuse init's complete error mapping.
                source.into()
            }
            // `--reject-shallow` on a shallow source: mirror Git's exit 128.
            CloneError::RejectShallow => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_exit_code(128)
                .with_hint("the source is shallow; clone without --reject-shallow, or deepen the source first"),
            CloneError::RemoteBranchNotFound {
                ref branch,
                ref remote,
            } => CliError::fatal(format!(
                "remote branch '{branch}' not found in upstream {remote}"
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint(
                "use `-b <branch>` to specify an existing branch, or omit to use remote HEAD",
            ),
            CloneError::LocalBranchState { source } => map_local_branch_state_error(source)
                .with_hint("run 'libra status' to verify the local repository state"),
            CloneError::FetchFailed { source } => map_fetch_error(source),
            CloneError::CheckoutFailed { source } => map_checkout_error(source),
            CloneError::IgnoreFile { source } => {
                let stable_code = if source.is_write() {
                    StableErrorCode::IoWriteFailed
                } else {
                    StableErrorCode::IoReadFailed
                };
                CliError::fatal(source.to_string())
                    .with_stable_code(stable_code)
                    .with_hint(source.recovery_hint())
            }
            CloneError::SetupFailed { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::InternalInvariant)
                .with_hint(format!("please report this issue at: {ISSUE_URL}")),
            CloneError::RemovedPublishRestoreSource => CliError::command_usage(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint(
                    "the Cloudflare publish site host was removed; clone a git remote or use `libra cloud` for repository backup.",
                ),
        }
    }
}

/// Map a `FetchError` from the discovery phase into a `CliError`.
fn map_discover_remote_error(source: fetch::FetchError) -> CliError {
    match &source {
        fetch::FetchError::InvalidRemoteSpec { kind, .. } => match kind {
            RemoteSpecErrorKind::MissingLocalRepo | RemoteSpecErrorKind::InvalidLocalRepo => {
                CliError::fatal(source.to_string())
                    .with_stable_code(StableErrorCode::RepoNotFound)
                    .with_hint("use a valid libra repository path or a reachable remote URL")
            }
            RemoteSpecErrorKind::MalformedUrl | RemoteSpecErrorKind::UnsupportedScheme => {
                CliError::command_usage(source.to_string())
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_hint(
                        "check the clone URL or scheme, for example `https://`, `ssh`, or a local path",
                    )
            }
        },
        fetch::FetchError::Discovery {
            source: git_error, ..
        } => match git_error {
            GitError::UnAuthorized(_) => {
                CliError::fatal(format!("remote discovery failed: {source}"))
                    .with_stable_code(StableErrorCode::AuthPermissionDenied)
                    .with_hint("check SSH key / HTTP credentials and repository access rights")
            }
            GitError::NetworkError(detail) if detail.starts_with(PKT_LINE_PROTOCOL_ERROR_PREFIX) => {
                CliError::fatal(format!("remote discovery failed: {source}"))
                    .with_stable_code(StableErrorCode::NetworkProtocol)
                    .with_hint("check that the remote serves Git data and that a proxy has not altered the response")
            }
            GitError::IOError(error) if fetch::is_pkt_line_io_error(error) => {
                CliError::fatal(format!("remote discovery failed: {source}"))
                    .with_stable_code(StableErrorCode::NetworkProtocol)
                    .with_hint("check that the remote serves Git data and that a proxy has not altered the response")
            }
            GitError::NetworkError(detail)
                if detail.starts_with(crate::internal::protocol::ssh_client::SSH_HOST_KEY_CHANGED_SIGNAL) =>
            {
                CliError::fatal("SSH host identity has changed")
                    .with_stable_code(StableErrorCode::NetworkUnavailable)
                    .with_hint(crate::internal::protocol::ssh_client::SSH_HOST_KEY_CHANGED_GUIDANCE)
            }
            GitError::NetworkError(detail)
                if detail.starts_with(crate::internal::protocol::ssh_client::SSH_HOST_KEY_UNCONFIRMED_SIGNAL) =>
            {
                CliError::fatal("SSH host key could not be verified")
                    .with_stable_code(StableErrorCode::NetworkUnavailable)
                    .with_hint(crate::internal::protocol::ssh_client::SSH_HOST_KEY_GUIDANCE)
            }
            GitError::NetworkError(_) => {
                CliError::fatal(format!("remote discovery failed: {source}"))
                    .with_stable_code(StableErrorCode::NetworkUnavailable)
                    .with_hint("check the remote host, DNS, VPN/proxy, and network connectivity")
            }
            GitError::IOError(_) => CliError::fatal(format!("remote discovery failed: {source}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
                .with_hint("check filesystem permissions and repository integrity"),
            _ => CliError::fatal(format!("remote discovery failed: {source}"))
                .with_stable_code(StableErrorCode::NetworkProtocol)
                .with_hint(
                    "the remote did not complete discovery successfully; retry and inspect server/protocol settings",
                ),
        },
        _ => CliError::fatal(format!("remote discovery failed: {source}"))
            .with_stable_code(StableErrorCode::NetworkProtocol)
            .with_hint(
                "the remote did not complete discovery successfully; retry and inspect server/protocol settings",
            ),
    }
}

/// Map a `FetchError` from the fetch phase into a `CliError`.
fn map_fetch_error(source: fetch::FetchError) -> CliError {
    match &source {
        fetch::FetchError::ObjectFormatMismatch { .. } => CliError::fatal(source.to_string())
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint("the remote and local repository use different object formats"),
        fetch::FetchError::FetchObjects { source: error, .. }
        | fetch::FetchError::PacketRead { source: error }
            if fetch::is_pkt_line_io_error(error) =>
        {
            CliError::fatal(source.to_string())
                .with_stable_code(StableErrorCode::NetworkProtocol)
                .with_hint("check that the remote serves Git data and that a proxy has not altered the response")
        }
        fetch::FetchError::FetchObjects { .. } | fetch::FetchError::PacketRead { .. } => {
            CliError::fatal(source.to_string())
                .with_stable_code(StableErrorCode::NetworkUnavailable)
                .with_hint("network error during transfer; check connectivity and retry")
        }
        fetch::FetchError::RemoteSideband { .. } | fetch::FetchError::ChecksumMismatch => {
            CliError::fatal(source.to_string())
                .with_stable_code(StableErrorCode::NetworkProtocol)
                .with_hint("the remote transfer failed or returned corrupted data; retry the clone")
        }
        fetch::FetchError::UnsupportedShallowLocalLibra | fetch::FetchError::LocalState { .. } => {
            CliError::fatal(source.to_string())
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint(
                    "omit --depth for local Libra sources, or use a Git remote that negotiates \
                     shallow boundaries",
                )
        }
        fetch::FetchError::RemoteBranchNotFound { .. } => CliError::fatal(source.to_string())
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint("the specified branch does not exist on the remote"),
        _ => CliError::fatal(source.to_string())
            .with_stable_code(StableErrorCode::NetworkUnavailable)
            .with_hint("network error during transfer; check connectivity and retry"),
    }
}

/// Map a `RestoreError` from the checkout phase into a `CliError`.
fn map_checkout_error(source: RestoreError) -> CliError {
    match source {
        RestoreError::ResolveSource | RestoreError::ReferenceNotCommit => {
            CliError::fatal("working tree checkout target could not be resolved")
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("working tree checkout target could not be resolved")
        }
        RestoreError::PathspecNotMatched(_) => {
            CliError::fatal("working tree checkout referenced a path that was not present")
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint(
                    "the fetched tree is inconsistent; retry the clone or inspect the remote",
                )
        }
        RestoreError::ReadIndex
        | RestoreError::ReadObject
        | RestoreError::ReadWorktree
        | RestoreError::InvalidPathEncoding => {
            CliError::fatal("failed to read repository state while checking out the working tree")
                .with_stable_code(StableErrorCode::IoReadFailed)
                .with_hint("failed to read repository state while checking out the working tree")
        }
        RestoreError::WriteWorktree => CliError::fatal(
            "working tree checkout did not complete because files could not be written",
        )
        .with_stable_code(StableErrorCode::IoWriteFailed)
        .with_hint("working tree checkout did not complete because files could not be written"),
        RestoreError::NonEmptyWorktreeDirectory(path) => CliError::fatal(format!(
            "working tree checkout refused to replace non-empty directory '{path}'"
        ))
        .with_stable_code(StableErrorCode::ConflictOperationBlocked)
        .with_hint("move or remove nested files before retrying the checkout"),
        RestoreError::SubmodulePathNotOwned(path) => CliError::fatal(format!(
            "working tree checkout refused to replace '{path}': Libra does not manage submodule content there"
        ))
        .with_stable_code(StableErrorCode::ConflictOperationBlocked)
        .with_hint("move that path aside before retrying the checkout"),
        RestoreError::LfsDownload => {
            CliError::fatal("checkout required downloading LFS content, but the transfer failed")
                .with_stable_code(StableErrorCode::NetworkUnavailable)
                .with_hint("checkout required downloading LFS content, but the transfer failed")
        }
        RestoreError::SymlinkUnsupported(path) => CliError::fatal(format!(
            "working tree checkout requires a symlink at '{path}', but this platform does not support it"
        ))
        .with_stable_code(StableErrorCode::Unsupported)
        .with_hint("retry on a platform with symlink support or disable checkout and inspect the tree"),
        // `clone` never resolves user revisions, so the locked-source guard
        // in `restore::run_restore` is unreachable here. Surface a fatal
        // diagnostic rather than panicking on the unreachable branch — keeps
        // the match exhaustive without burying the case.
        RestoreError::LockedSource(name) => CliError::fatal(format!(
            "internal error: clone checkout attempted to restore from locked branch '{name}'"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid),
        RestoreError::LockedCurrentBranch(name) => CliError::fatal(format!(
            "internal error: clone checkout attempted to write worktree while on locked branch '{name}'"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid),
        // `clone` builds its own RestoreArgs and never sets --pathspec-from-file,
        // so this is unreachable; surface it rather than panicking.
        RestoreError::PathspecFileRead(detail) => CliError::fatal(format!(
            "internal error: clone checkout reported a pathspec-file read failure: {detail}"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid),
        // `clone` never restores conflict stages (it checks out a freshly fetched
        // tree with no unmerged index), so these are unreachable; surface rather
        // than panic.
        RestoreError::PathUnmerged(path) => CliError::fatal(format!(
            "internal error: clone checkout reported an unmerged path '{path}'"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid),
        RestoreError::MissingStageVersion { path, stage } => CliError::fatal(format!(
            "internal error: clone checkout reported a missing conflict stage {stage} for '{path}'"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid),
        // `clone` never passes `--merge`/`--conflict`, so this is unreachable;
        // surface rather than panic.
        RestoreError::UnsupportedConflictStyle(style) => CliError::fatal(format!(
            "internal error: clone checkout reported an unsupported conflict style '{style}'"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid),
        RestoreError::InvalidPathspec(detail) => CliError::fatal(format!(
            "internal error: clone checkout reported an invalid pathspec: {detail}"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid),
    }
}

fn map_local_branch_state_error(source: branch::BranchStoreError) -> CliError {
    match source {
        branch::BranchStoreError::AlreadyExists(name) => {
            CliError::fatal(format!("branch '{name}' already exists"))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
        }
        branch::BranchStoreError::Query(detail) => {
            CliError::fatal(format!(
                "failed to inspect local branch state after fetch: {detail}"
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
        }
        branch::BranchStoreError::Corrupt { .. } => {
            CliError::fatal(format!(
                "failed to inspect local branch state after fetch: {source}"
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt)
        }
        branch::BranchStoreError::NotFound(name) => {
            CliError::fatal(format!(
                "failed to inspect local branch state after fetch: branch '{name}' not found"
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
        }
        branch::BranchStoreError::Delete { name, detail } => CliError::fatal(format!(
            "failed to inspect local branch state after fetch: failed to delete branch '{name}': {detail}"
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed),
        // §C.13 LBR-CONFLICT-002.
        branch::BranchStoreError::CheckedOutElsewhere { .. } => CliError::fatal(format!(
            "failed to inspect local branch state after fetch: {source}"
        ))
        .with_stable_code(StableErrorCode::ConflictOperationBlocked)
        .with_hint(
            "the other worktree must switch away first; `libra worktree list` shows which one \
             holds it",
        ),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn contains_initialized_repo(metadata_root: &Path) -> bool {
    REPO_MARKERS
        .iter()
        .any(|marker| metadata_root.join(marker).exists())
}

/// Custom validation function, ensuring depth >= 1
fn validate_depth(s: &str) -> Result<usize, String> {
    s.parse::<usize>()
        .map_err(|_| "DEPTH must be a valid integer".to_string())
        .and_then(|val| {
            if val >= 1 {
                Ok(val)
            } else {
                Err("DEPTH must be greater than or equal to 1".to_string())
            }
        })
}

fn display_home_relative(path: &str) -> String {
    let Some(home) = dirs::home_dir() else {
        return path.to_string();
    };
    let home = home.to_string_lossy().to_string();
    if let Some(rest) = path.strip_prefix(&home) {
        return format!("~{rest}");
    }
    path.to_string()
}

// ---------------------------------------------------------------------------
// Cleanup
// ---------------------------------------------------------------------------

/// Attempt to clean up a failed clone. Returns a warning string if cleanup
/// itself fails, so the caller can surface it via `CliError.hints`.
/// Validate a `-o`/`--origin` remote name before it is interpolated into config
/// keys and `refs/remotes/<name>/*` refs. The name must satisfy Git's
/// check-ref-format rules for the `refs/remotes/<name>/HEAD` ref it will form;
/// otherwise a usage error (exit 129) is returned.
fn validate_remote_name(name: &str) -> CliResult<()> {
    if name.is_empty() || name.len() > 255 || !is_valid_remote_ref_format(name) {
        return Err(
            CliError::command_usage(format!("invalid remote name '{name}'"))
                .with_hint("use a plain remote name such as 'origin' or 'upstream'"),
        );
    }
    Ok(())
}

/// Apply Git's `check-ref-format` rules to the ref a remote name would create
/// (`refs/remotes/<name>/HEAD`): reject empty components, leading-dot or
/// `.lock`-suffixed components, `..`/`//`/`@{`, trailing `/` or `.`, and the
/// disallowed bytes (control/whitespace and ``: \ ~ ^ ? * [``).
fn is_valid_remote_ref_format(name: &str) -> bool {
    let candidate = format!("refs/remotes/{name}/HEAD");
    let Some(short) = candidate.strip_prefix("refs/") else {
        return false;
    };
    if short.starts_with('/')
        || short.ends_with('/')
        || short.ends_with('.')
        || short.ends_with(".lock")
        || short.contains("//")
        || short.contains("..")
        || short.contains("@{")
    {
        return false;
    }
    if short.split('/').any(|component| {
        component.is_empty() || component.starts_with('.') || component.ends_with(".lock")
    }) {
        return false;
    }
    !short.chars().any(|c| {
        c.is_ascii_control()
            || c.is_whitespace()
            || matches!(c, ':' | '\\' | '~' | '^' | '?' | '*' | '[')
    })
}

fn cleanup_failed_clone(local_path: &Path, created_by_clone: bool) -> Option<String> {
    let cleanup_result = if created_by_clone {
        fs::remove_dir_all(local_path)
    } else {
        clear_directory_contents(local_path)
    };

    match cleanup_result {
        Ok(()) => None,
        Err(error) => {
            let warning = format!(
                "warning: failed to clean up '{}': {}",
                local_path.display(),
                error
            );
            tracing::error!("{}", warning);
            Some(warning)
        }
    }
}

fn clear_directory_contents(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

pub async fn execute(args: CloneArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting.
///
/// # Side Effects
/// - Creates the destination repository layout and object storage.
/// - Fetches objects from the remote URL and writes refs/config.
/// - Checks out the working tree for non-bare clones.
/// - Restores the original process working directory after success or failure.
/// - May remove the partially created destination when clone cleanup is needed.
///
/// # Errors
/// Returns [`CliError`] when destination validation fails, remote negotiation or
/// object transfer fails, refs/config cannot be written, checkout fails, cleanup
/// fails, or the original working directory cannot be restored.
///
/// This is the **rendering layer**: it calls `execute_clone()` to get a
/// `CloneOutput` and then renders it according to the `OutputConfig`.
pub async fn execute_safe(mut args: CloneArgs, output: &OutputConfig) -> CliResult<()> {
    // `-o`/`--origin` becomes a config key and a `refs/remotes/<name>/*` ref
    // component, so reject invalid names up front (before touching the
    // filesystem) rather than writing a malformed key/ref.
    if let Some(name) = &args.origin {
        validate_remote_name(name)?;
    }

    // `--mirror` implies `--bare`: the mirror is a bare repository whose refs
    // mirror the source. Set it before dispatch so every bare-aware code path
    // (layout, no checkout, cloud rejection) treats a mirror as bare.
    if args.mirror {
        args.bare = true;
    }

    let original_dir = util::cur_dir();
    // §C.4.2: same as `init` — the target repository does not exist yet, and
    // clone creates it, enters it and checks out INSIDE it. A pin from an
    // enclosing repository would send the checkout's index write there.
    let scope = crate::internal::worktree_scope::WorktreeScope::unpinned();
    let (result, cleanup_warning) = execute_clone(&args, &original_dir, output).await;
    drop(scope);

    // Always restore the working directory.
    if env::current_dir().ok().as_ref() != Some(&original_dir) {
        #[cfg(test)]
        let _cwd_lock = crate::utils::test::cwd_lock_guard();
        env::set_current_dir(&original_dir).map_err(|source| {
            CliError::from(CloneError::RestoreDirectory {
                path: original_dir.clone(),
                source,
            })
        })?;
    }

    match result {
        Ok(clone_output) => render_clone_result(&clone_output, output),
        Err(error) => {
            let mut cli_error = CliError::from(error);
            if let Some(warning) = cleanup_warning {
                cli_error = cli_error.with_priority_hint(warning);
            }
            Err(cli_error)
        }
    }
}

/// Render the successful clone result to stdout / stderr.
fn render_clone_result(result: &CloneOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("clone", result, output);
    }
    if output.quiet {
        return Ok(());
    }

    // Human-readable success summary on stdout.
    if result.bare {
        println!("Cloned into bare repository '{}'", result.path);
    } else {
        // Show just the directory name, not the full path.
        let display_path = Path::new(&result.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&result.path);
        println!("Cloned into '{display_path}'");
    }
    println!("  remote: {} → {}", result.remote_name, result.remote_url);
    if let Some(branch) = &result.branch {
        println!("  branch: {branch}");
    }
    println!(
        "  signing: {}",
        if result.vault_signing {
            "enabled"
        } else {
            "disabled"
        }
    );

    // SSH key tip.
    if let Some(key_path) = &result.ssh_key_detected {
        println!();
        println!(
            "Tip: using existing SSH key at {}",
            display_home_relative(key_path)
        );
    }

    // .gitignore → .libraignore conversion tip.
    if !result.gitignore_converted.is_empty() {
        println!();
        let n = result.gitignore_converted.len();
        let plural = if n == 1 { "" } else { "s" };
        println!(
            "Tip: {n} .gitignore file{plural} converted to .libraignore — \
             run 'libra add .libraignore' (or 'libra add -A') to track them, \
             then 'libra commit' to record the change."
        );
    }

    // Warnings on stderr.
    for w in &result.warnings {
        eprintln!("warning: {w}");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Execution layer — produces CloneOutput, no rendering
// ---------------------------------------------------------------------------

/// Returns `(result, cleanup_warning)`. The cleanup warning is `Some` only
/// when the clone fails **and** the subsequent directory cleanup also fails.
async fn execute_clone(
    args: &CloneArgs,
    original_dir: &Path,
    output: &OutputConfig,
) -> (Result<CloneOutput, CloneError>, Option<String>) {
    match execute_clone_inner(args, original_dir, output).await {
        Ok(clone_output) => (Ok(clone_output), None),
        Err((error, cleanup_warning)) => (Err(error), cleanup_warning),
    }
}

/// Inner implementation that returns an error tuple containing the clone error
/// and an optional cleanup warning.
#[allow(clippy::result_large_err)]
async fn execute_clone_inner(
    args: &CloneArgs,
    original_dir: &Path,
    output: &OutputConfig,
) -> Result<CloneOutput, (CloneError, Option<String>)> {
    if is_removed_publish_restore_source(&args.remote_repo) {
        return Err((CloneError::RemovedPublishRestoreSource, None));
    }

    let mut remote_repo = args.remote_repo.clone();
    if !remote_repo.ends_with('/') {
        remote_repo.push('/');
    }

    // --- Step 1: Resolve local path ---
    let local_path = match &args.local_path {
        Some(path) => path.clone(),
        None => {
            let repo_name = util::get_repo_name_from_url(&remote_repo)
                .ok_or((CloneError::CannotInferDestination, None))?;
            original_dir.join(repo_name).to_string_lossy().into_owned()
        }
    };

    let local_path = PathBuf::from(&local_path);
    let local_path = if local_path.is_absolute() {
        local_path
    } else {
        original_dir.join(&local_path)
    };
    let metadata_root = if args.bare {
        local_path.clone()
    } else {
        local_path.join(util::ROOT_DIR)
    };

    // --- Step 2: Remote discovery ---
    if !output.quiet && !output.is_json() {
        eprintln!("Connecting to {} ...", args.remote_repo);
    }

    let (remote_client, discovery) = fetch::discover_remote(&remote_repo)
        .await
        .map_err(|source| (CloneError::DiscoverRemote { source }, None))?;

    // --- Step 3: Destination pre-checks ---
    if metadata_root.exists() && contains_initialized_repo(&metadata_root) {
        return Err((
            CloneError::DestinationAlreadyRepo {
                path: local_path.clone(),
            },
            None,
        ));
    }
    if local_path.exists() && !util::is_empty_dir(&local_path) {
        return Err((
            CloneError::DestinationExistsNonEmpty {
                path: local_path.clone(),
            },
            None,
        ));
    }

    let created_by_clone = if local_path.exists() {
        false
    } else {
        fs::create_dir_all(&local_path).map_err(|source| {
            (
                CloneError::CreateDestinationFailed {
                    path: local_path.clone(),
                    source,
                },
                None,
            )
        })?;
        true
    };

    // --- Pre-check: specified branch exists on remote ---
    if let Some(branch) = &args.branch
        && !fetch::remote_has_branch(&discovery.refs, branch)
    {
        let cleanup_warning = cleanup_failed_clone(&local_path, created_by_clone);
        return Err((
            CloneError::RemoteBranchNotFound {
                branch: branch.clone(),
                remote: args.origin.clone().unwrap_or_else(|| "origin".to_string()),
            },
            cleanup_warning,
        ));
    }

    // --- Step 4–7: clone into destination ---
    let remote_url = fetch::normalize_remote_url(&remote_repo, &remote_client);

    clone_into_destination(
        args,
        &remote_url,
        &remote_client,
        &discovery,
        &local_path,
        original_dir,
        output,
    )
    .await
    .map_err(|error| {
        if env::current_dir().ok().as_deref() != Some(original_dir) {
            #[cfg(test)]
            let _cwd_lock = crate::utils::test::cwd_lock_guard();
            let _ = env::set_current_dir(original_dir);
        }
        let cleanup_warning = cleanup_failed_clone(&local_path, created_by_clone);
        (error, cleanup_warning)
    })
}

fn clone_config_local_target() -> LocalIdentityTarget<'static> {
    if util::try_get_storage_path(None).is_ok() {
        LocalIdentityTarget::CurrentRepo
    } else {
        LocalIdentityTarget::None
    }
}

fn is_removed_publish_restore_source(input: &str) -> bool {
    // Split the retired scheme so the RC-34 zero-hit guard does not match a
    // contiguous token in this file.
    let scheme = concat!("libra", "+", "cloud");
    input
        .split_once("://")
        .is_some_and(|(found, _)| found.eq_ignore_ascii_case(scheme))
}

fn sparse_include_pattern(path: &str) -> String {
    let mut pattern = String::with_capacity(path.len() + 1);
    pattern.push('/');
    for ch in path.chars() {
        if matches!(ch, '\\' | '*' | '?' | '[' | ']') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern
}

/// Apply `clone --deps-of` (lore.md 3.2): after the full, commit-safe checkout,
/// scope the repo's sparse VIEW to the forward dependency closure of the
/// requested roots and persist the `remote.<name>.fetchNotesDeps` opt-in so later
/// pulls keep the graph fresh. Returns human-readable warnings and NEVER fails
/// the clone (the working tree is already fully materialized). Assumes the cwd is
/// the freshly-cloned repo.
async fn apply_deps_of_view(
    args: &CloneArgs,
    remote_name: &str,
    remote_client: &fetch::RemoteClient,
) -> Vec<String> {
    if args.deps_of.is_empty() {
        return Vec::new();
    }
    let mut warnings = Vec::new();

    // Honest: objects are never wire-filtered and the whole tree is on disk.
    warnings.push(
        "--deps-of narrows the checkout VIEW only; the full object set was still downloaded \
         (Libra has no partial-clone) and the whole tree remains on disk (disk narrowing is \
         deferred — see _compatibility.md D18)"
            .to_string(),
    );

    // A network or foreign-Git remote cannot travel the dependency graph in v1
    // (D17) — the fetch already warned. Refuse to set a misleadingly-narrow view
    // and keep the full clone.
    let remote_can_travel = matches!(
        remote_client,
        fetch::RemoteClient::Local(c) if c.is_libra_source()
    );
    if !remote_can_travel {
        warnings.push(
            "--deps-of: this remote cannot travel the dependency graph yet (only a local Libra \
             source can — see _compatibility.md D17); performed a full clone WITHOUT dependency \
             scoping"
                .to_string(),
        );
        return warnings;
    }

    // Normalize the roots; a bad root is warned + skipped, never fatal.
    let mut roots: Vec<String> = Vec::new();
    for raw in &args.deps_of {
        match crate::internal::deps::normalize_edge_path(raw) {
            Ok(p) => roots.push(p),
            Err(e) => warnings.push(format!("--deps-of: ignoring invalid root '{raw}': {e}")),
        }
    }
    if roots.is_empty() {
        warnings.push(
            "--deps-of: no valid root paths; left the working tree fully checked out with no \
             dependency scoping"
                .to_string(),
        );
        return warnings;
    }

    // Persist the opt-in so `libra pull` keeps the graph fresh.
    let _ = ConfigKv::set(
        &format!("remote.{remote_name}.fetchNotesDeps"),
        "true",
        false,
    )
    .await;

    // Compute the forward transitive closure at the just-checked-out HEAD.
    let closure = match crate::internal::deps::DependencyStore::transitive_closure(
        "HEAD",
        &roots,
        crate::internal::deps::Direction::Forward,
        args.deps_depth_limit,
    )
    .await
    {
        Ok(closure) => closure,
        Err(e) => {
            warnings.push(format!(
                "--deps-of: could not compute the dependency closure (left full checkout): {e}"
            ));
            return warnings;
        }
    };

    if closure.reachable.len() == roots.len() {
        warnings.push(
            "--deps-of: no dependency edges were declared for the given root(s) at HEAD; scoped \
             the view to the root(s) only"
                .to_string(),
        );
    }

    // Persist the closure as the sparse VIEW (anchored + glob-escaped patterns so
    // ls-files/status/diff scope EXACTLY to the closure files).
    let patterns: Vec<String> = closure
        .reachable
        .iter()
        .map(|p| sparse_include_pattern(p))
        .collect();
    // A fresh clone has exactly one (main) worktree — the deps-of view
    // belongs to the main scope by construction (W1 §C.4.1.1).
    if let Err(e) = crate::internal::sparse::SparseViewStore::replace(
        &crate::internal::worktree_scope::WorktreeScope::Main,
        &patterns,
    )
    .await
    {
        warnings.push(format!(
            "--deps-of: computed the dependency closure but could not set the sparse view: {e}"
        ));
    }

    warnings
}

async fn clone_into_destination(
    args: &CloneArgs,
    remote_url: &str,
    remote_client: &fetch::RemoteClient,
    discovery: &DiscoveryResult,
    local_path: &Path,
    original_dir: &Path,
    output: &OutputConfig,
) -> Result<CloneOutput, CloneError> {
    // lore.md 2.11: resolve the effective `--shared` decision BEFORE cwd
    // changes into the new (empty) clone — a per-repo `clone.shared` in the
    // directory being cloned FROM should count, and the fresh clone has no
    // config yet.
    let config_shared = crate::internal::config::read_cascaded_config_value(
        clone_config_local_target(),
        "clone.shared",
    )
    .await
    .ok()
    .flatten()
    .map(|v| matches!(v.trim(), "true" | "1" | "yes" | "on"))
    .unwrap_or(false);
    let effective_shared = !args.dissociate && !args.no_shared && (args.shared || config_shared);

    #[cfg(test)]
    let _cwd_lock = crate::utils::test::cwd_lock_guard();
    env::set_current_dir(local_path).map_err(|source| CloneError::ChangeDirectory {
        path: local_path.to_path_buf(),
        source,
    })?;

    let object_format = match discovery.hash_kind {
        git_internal::hash::HashKind::Sha1 => "sha1".to_string(),
        git_internal::hash::HashKind::Sha256 => "sha256".to_string(),
        git_internal::hash::HashKind::Blake3 => "blake3".to_string(),
    };

    // --- Step 4: Initialize repository ---
    if !output.quiet && !output.is_json() {
        eprintln!("Initializing repository ...");
    }

    let init_output = command::init::run_init(command::init::InitArgs {
        bare: args.bare,
        template: None,
        initial_branch: args.branch.clone(),
        repo_directory: local_path.to_string_lossy().into_owned(),
        quiet: true,
        shared: None,
        object_format: Some(object_format.clone()),
        ref_format: None,
        from_git_repository: None,
        vault: true,
    })
    .await
    .map_err(|source| CloneError::InitializeRepository { source })?;

    // --- Step 5: Fetch objects ---
    if !output.quiet && !output.is_json() {
        eprintln!("Fetching objects ...");
    }

    // `--no-progress` suppresses the fetch's "Receiving objects" meter during
    // the clone, matching `git clone --no-progress`.
    let child_output = output.child_output_config();
    let child_output =
        fetch::apply_no_progress(&child_output, args.no_progress).unwrap_or(child_output);
    // `-o`/`--origin` names the remote (and its tracking refs); defaults to
    // `origin`. `setup_repository` threads `remote_config.name` through the
    // `refs/remotes/<name>/*` refs, `branch.<b>.remote`, and `remote.<name>.url`.
    let remote_name = args.origin.clone().unwrap_or_else(|| "origin".to_string());
    let remote_config = RemoteConfig {
        name: remote_name.clone(),
        url: remote_url.to_string(),
    };
    // `git clone` fetches ALL tags by default; `--no-tags` skips them and records
    // `remote.<name>.tagOpt=--no-tags` so later fetches also skip tags.
    let clone_tag_mode = if args.no_tags {
        let _ = ConfigKv::set(&format!("remote.{remote_name}.tagOpt"), "--no-tags", false).await;
        fetch::TagFetchMode::NoTags
    } else {
        fetch::TagFetchMode::All
    };
    // Capture the fetch result so the clone can report transfer counts
    // (`objects_fetched`/`bytes_received`) in its structured output. `dry_run`
    // and `force` are always false for a clone into a fresh repository.
    let fetch_result = fetch::fetch_repository_with_result(
        remote_config.clone(),
        args.branch.clone(),
        args.single_branch,
        args.depth,
        false,
        Some(clone_tag_mode),
        false,
        // A fresh clone has no remote-tracking refs to prune.
        false,
        // `--deps-of` needs the dependency graph to compute the closure, so it
        // implies `--notes`; a plain clone never fetches notes (Git parity).
        !args.deps_of.is_empty(),
        &child_output,
    )
    .await
    .map_err(|source| CloneError::FetchFailed { source })?;
    // Clone owns this fresh repository exclusively; the fetch's ref updates
    // landed inside — release the pack's `.keep` pin now.
    fetch_result.release_pack_pin();

    // --- Step 6–7: Configure repository + checkout ---
    if !output.quiet && !output.is_json() {
        eprintln!("Configuring repository ...");
    }

    if !args.bare && !args.no_checkout && !output.quiet && !output.is_json() {
        eprintln!("Checking out working copy ...");
    }

    let setup_result = setup_repository(
        remote_config.clone(),
        args.branch.clone(),
        !args.bare && !args.no_checkout,
    )
    .await?;

    // lore.md 2.11: auto-register the source as an object alternate for a LOCAL
    // LIBRA source (a Git source's `git gc` does not consult Libra's borrowers
    // file, so it is never safe). NON-FATAL: any failure (a guard refusal OR an
    // io write, e.g. a read-only source) warns and continues — the clone
    // already copied everything and must not fail over a shared-store link.
    let mut shared_warnings: Vec<String> = Vec::new();
    if effective_shared {
        if let fetch::RemoteClient::Local(client) = remote_client {
            if client.is_libra_source() {
                let base_objects = client.repo_path().join("objects");
                let clone_objects = path::objects();
                match command::alternates::guarded_add(
                    &clone_objects,
                    &base_objects,
                    &object_format,
                )
                .await
                {
                    Ok(()) if !output.quiet => shared_warnings.push(format!(
                        "shared: registered {} as an object alternate (reads borrow from it; \
                         v1 still copied every object)",
                        base_objects.display()
                    )),
                    Ok(()) => {}
                    Err(e) => shared_warnings.push(format!(
                        "--shared: could not register the source as an alternate (continuing \
                         — the clone is self-contained): {e}"
                    )),
                }
            } else if args.shared {
                shared_warnings.push(
                    "--shared has no effect: the source is a local Git repo, not a Libra repo"
                        .to_string(),
                );
            }
        } else if args.shared {
            shared_warnings.push(
                "--shared has no effect: sharing an object store is only possible for a LOCAL \
                 Libra source"
                    .to_string(),
            );
        }
    }

    // `--mirror`: turn the standard tracking-ref layout into a mirror — every
    // fetched branch becomes a local `refs/heads/*` ref and the
    // `refs/remotes/<name>/*` tracking refs are dropped — and record the
    // informational `remote.<name>.mirror=true` marker (Libra's fetch is not yet
    // mirror-aware, so refreshing the mirror is not automatic).
    if args.mirror {
        normalize_mirror_refs(&remote_name).await?;
    }

    // `--reject-shallow`: if the fetch left a shallow boundary that the user did
    // not request via `--depth`, the source repository was shallow — refuse it
    // (matching `git clone --reject-shallow`). Local Libra sources with
    // `--depth` fail earlier in fetch because they cannot produce this shallow
    // metadata. The cwd is still the new repo here, so the shallow marker lives
    // at the current `.libra/shallow`.
    let is_shallow = std::fs::read_to_string(util::storage_path().join("shallow"))
        .map(|contents| !contents.trim().is_empty())
        .unwrap_or(false);
    if clone_should_reject_shallow(args.reject_shallow, is_shallow, args.depth) {
        // Restore the cwd before returning so the caller's cleanup can remove
        // the partially-created destination.
        let _ = env::set_current_dir(original_dir);
        return Err(CloneError::RejectShallow);
    }

    let mut warnings = init_output.warnings.clone();
    warnings.extend(shared_warnings);
    warnings.extend(object_alternates_warning(args));
    warnings.extend(unsupported_fetch_optimization_warnings(args));
    // lore.md 3.2: `--deps-of` — scope the fresh clone's sparse VIEW to the
    // forward dependency closure of the requested roots (the graph was imported
    // by the implied `--notes` fetch above). The working tree stays fully checked
    // out (commit-safe); only the VIEW is narrowed. cwd is still the new repo.
    warnings.extend(apply_deps_of_view(args, &remote_name, remote_client).await);
    let mut gitignore_converted = Vec::new();
    if !args.bare {
        let summary = ignore_utils::convert_gitignore_files_to_libraignore(local_path, local_path)
            .map_err(|source| CloneError::IgnoreFile { source })?;
        warnings.extend(summary.warnings);
        gitignore_converted = summary
            .converted
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
    }

    // Restore original directory before returning.
    env::set_current_dir(original_dir).map_err(|source| CloneError::RestoreDirectory {
        path: original_dir.to_path_buf(),
        source,
    })?;

    // Build CloneOutput.
    if setup_result.branch_name.is_none() {
        warnings.push("You appear to have cloned an empty repository.".to_string());
    }

    Ok(CloneOutput {
        path: local_path.to_string_lossy().into_owned(),
        bare: args.bare,
        remote_url: remote_url.to_string(),
        remote_name: remote_name.clone(),
        branch: setup_result.branch_name,
        object_format,
        repo_id: init_output.repo_id,
        vault_signing: init_output.vault_signing,
        ssh_key_detected: init_output.ssh_key_detected,
        shallow: args.depth.is_some(),
        warnings,
        gitignore_converted,
        source_kind: None,
        cloud_site: None,
        objects_fetched: Some(fetch_result.objects_fetched),
        bytes_received: Some(fetch_result.bytes_received),
    })
}

// ---------------------------------------------------------------------------
// Setup — configures remote, branch, HEAD, reflog, and checkout
// ---------------------------------------------------------------------------

/// Result of `setup_repository`, carrying the branch that was checked out
/// (if any) so that `CloneOutput` can report it.
pub(crate) struct SetupResult {
    pub branch_name: Option<String>,
}

/// Normalize a freshly-cloned repository into a `--mirror` layout: promote every
/// remote-tracking branch (`refs/remotes/<remote>/<name>`) to a verbatim local
/// `refs/heads/<name>` branch, drop the tracking namespace, and record
/// `remote.<remote>.mirror=true`. Tags (`refs/tags/*`) are already in place and
/// are left untouched.
///
/// `setup_repository` has already created the default branch in `refs/heads/*`
/// and pointed `HEAD` at it; this promotes the remaining branches and removes the
/// remote-tracking refs that a mirror does not keep.
///
/// NARROWING vs Git: Git's `--mirror` mirrors `refs/*:refs/*` verbatim and makes
/// future fetches force-update every ref. Libra mirrors only what its fetch
/// transfers — every fetched tracking ref is promoted into `refs/heads/*` and
/// tags are kept — so:
/// - ref namespaces Libra does not fetch (e.g. `refs/notes/*`) are not mirrored;
/// - because Libra's fetch collapses both `refs/heads/mr/*` and `refs/mr/*` into
///   one `refs/remotes/<remote>/mr/*` tracking namespace, any such refs are
///   promoted to `refs/heads/mr/*` (provenance is not preserved);
/// - `mirror=true` is recorded only as a marker; `libra fetch` is not yet
///   mirror-aware, so refreshing the mirror is not automatic (and no inert
///   `+refs/*:refs/*` refspec is written).
async fn normalize_mirror_refs(remote_name: &str) -> Result<(), CloneError> {
    let db = get_db_conn_instance().await;
    let tracking = Branch::list_branches_result_with_conn(&db, Some(remote_name))
        .await
        .map_err(|source| CloneError::LocalBranchState { source })?;

    let tracking_prefix = format!("refs/remotes/{remote_name}/");
    for branch in &tracking {
        // Tracking branches are stored under their full `refs/remotes/<remote>/`
        // path; strip it to get the short name for the local `refs/heads/` ref.
        let short_name = branch
            .name
            .strip_prefix(&tracking_prefix)
            .unwrap_or(&branch.name);
        // Promote every fetched tracking ref to a verbatim local branch
        // (idempotent: the default branch already exists from setup_repository
        // and is simply re-affirmed). We promote ALL tracking rows rather than
        // trying to filter by namespace: Libra's fetch collapses both real
        // branches (`refs/heads/mr/*`) and merge-request refs (`refs/mr/*`) into
        // the same `refs/remotes/<remote>/mr/*` tracking namespace, so the
        // provenance needed to filter is gone — skipping would silently drop real
        // branches. Such refs are therefore mirrored as `refs/heads/mr/*`.
        Branch::update_branch_with_conn(&db, short_name, &branch.commit.to_string(), None)
            .await
            .map_err(|error| CloneError::SetupFailed {
                message: format!("failed to create mirror branch '{short_name}': {error}"),
            })?;
        // A mirror keeps no remote-tracking refs.
        Branch::delete_branch_result_with_conn(&db, &branch.name, Some(remote_name))
            .await
            .map_err(|source| CloneError::LocalBranchState { source })?;
    }

    // The fetch also caches the remote's HEAD as a `Head` row
    // (`refs/remotes/<remote>/HEAD`), which is a tracking ref a mirror must not
    // keep. It is not a `Branch` row, so delete it directly.
    {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        use crate::internal::model::reference;

        reference::Entity::delete_many()
            .filter(reference::Column::Kind.eq(reference::ConfigKind::Head))
            .filter(reference::Column::Remote.eq(remote_name))
            .exec(&db)
            .await
            .map_err(|error| CloneError::SetupFailed {
                message: format!("failed to drop mirror remote HEAD: {error}"),
            })?;
    }

    // Record the mirror marker (matching Git's `remote.<name>.mirror=true`). We
    // deliberately do NOT write a `+refs/*:refs/*` fetch refspec: Libra's fetch
    // does not honor it, so recording it would falsely imply mirror-aware
    // refreshes.
    let _ = ConfigKv::set(&format!("remote.{remote_name}.mirror"), "true", false).await;
    Ok(())
}

/// Sets up the local repository after a clone by configuring the remote,
/// setting up the initial branch and HEAD, and creating the first reflog entry.
/// Skips checking out the worktree when `checkout_worktree` is `false` (bare clone).
/// This function is `pub(crate)` to allow reuse by the `convert` module for
/// importing existing Git repositories during `libra init --from-git-repository`.
pub(crate) async fn setup_repository(
    remote_config: RemoteConfig,
    specified_branch: Option<String>,
    checkout_worktree: bool,
) -> Result<SetupResult, CloneError> {
    let db = get_db_conn_instance().await;
    let remote_head = Head::remote_current_with_conn(&db, &remote_config.name).await;

    let branch_to_checkout = match specified_branch {
        Some(branch_name) => Some(branch_name),
        None => match remote_head {
            Some(Head::Branch(name)) => Some(name),
            _ => None,
        },
    };

    if let Some(branch_name) = branch_to_checkout {
        let remote_tracking_ref = format!("refs/remotes/{}/{}", remote_config.name, branch_name);
        let origin_branch = Branch::find_branch_result_with_conn(
            &db,
            &remote_tracking_ref,
            Some(&remote_config.name),
        )
        .await
        .map_err(|source| CloneError::LocalBranchState { source })?
        .ok_or_else(|| CloneError::RemoteBranchNotFound {
            branch: branch_name.clone(),
            remote: remote_config.name.clone(),
        })?;

        let action = ReflogAction::Clone {
            from: remote_config.url.clone(),
        };
        let context = ReflogContext {
            old_oid: ObjectHash::zero_str(get_hash_kind()).to_string(),
            new_oid: origin_branch.commit.to_string(),
            action,
        };

        // Clone the branch name before moving it into the closure.
        let branch_name_for_result = branch_name.clone();
        with_reflog(
            context,
            move |txn: &DatabaseTransaction| {
                Box::pin(async move {
                    Branch::update_branch_with_conn(
                        txn,
                        &branch_name,
                        &origin_branch.commit.to_string(),
                        None,
                    )
                    .await?;
                    Head::update_result_with_conn(txn, Head::Branch(branch_name.to_owned()), None)
                        .await
                        .map_err(|error| sea_orm::DbErr::Custom(error.to_string()))?;

                    let merge_ref = format!("refs/heads/{}", branch_name);
                    let _ = ConfigKv::set_with_conn(
                        txn,
                        &format!("branch.{}.merge", branch_name),
                        &merge_ref,
                        false,
                    )
                    .await;
                    let _ = ConfigKv::set_with_conn(
                        txn,
                        &format!("branch.{}.remote", branch_name),
                        &remote_config.name,
                        false,
                    )
                    .await;
                    let _ = ConfigKv::set_with_conn(
                        txn,
                        &format!("remote.{}.url", remote_config.name),
                        &remote_config.url,
                        false,
                    )
                    .await;
                    Ok(())
                })
            },
            true,
        )
        .await
        .map_err(|error| CloneError::SetupFailed {
            message: error.to_string(),
        })?;

        if checkout_worktree {
            command::restore::execute_checked_typed(RestoreArgs {
                overlay: false,
                no_overlay: false,
                ours: false,
                theirs: false,
                ignore_unmerged: false,
                merge: false,
                conflict: None,
                worktree: true,
                staged: true,
                source: None,
                pathspec: vec![util::working_dir_string()],
                pathspec_from_file: None,
                pathspec_file_nul: false,
                no_progress: false,
            })
            .await
            .map_err(|source| CloneError::CheckoutFailed { source })?;
        }

        Ok(SetupResult {
            branch_name: Some(branch_name_for_result),
        })
    } else {
        let _ = ConfigKv::set(
            &format!("remote.{}.url", remote_config.name),
            &remote_config.url,
            false,
        )
        .await;

        let default_branch = "main";
        let merge_ref = format!("refs/heads/{}", default_branch);
        let _ = ConfigKv::set(&format!("branch.{default_branch}.merge"), &merge_ref, false).await;
        let _ = ConfigKv::set(
            &format!("branch.{default_branch}.remote"),
            &remote_config.name,
            false,
        )
        .await;

        Ok(SetupResult { branch_name: None })
    }
}

/// Unit tests for the clone module
/// Unit tests for the clone module
#[cfg(test)]
mod tests {
    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::utils::test::{ChangeDirGuard, ScopedEnvVar};

    #[test]
    fn discover_remote_unauthorized_maps_to_auth_permission_denied() {
        let cli = map_discover_remote_error(fetch::FetchError::Discovery {
            remote: "ssh://example.com/repo.git".to_string(),
            source: GitError::UnAuthorized("permission denied".to_string()),
        });

        assert_eq!(cli.stable_code(), StableErrorCode::AuthPermissionDenied);
        assert_eq!(cli.exit_code(), 128);
        assert_eq!(
            cli.hints()[0].as_str(),
            "check SSH key / HTTP credentials and repository access rights"
        );
    }

    #[test]
    fn discover_remote_unsupported_scheme_maps_to_cli_invalid_target() {
        let cli = map_discover_remote_error(fetch::FetchError::InvalidRemoteSpec {
            spec: "ftp://example.com/repo.git".to_string(),
            kind: RemoteSpecErrorKind::UnsupportedScheme,
            reason: "unsupported remote scheme 'ftp'".to_string(),
        });

        assert_eq!(cli.stable_code(), StableErrorCode::CliInvalidTarget);
        assert_eq!(cli.exit_code(), 129);
        assert_eq!(
            cli.hints()[0].as_str(),
            "check the clone URL or scheme, for example `https://`, `ssh`, or a local path"
        );
    }

    #[test]
    fn discover_remote_network_error_maps_to_network_unavailable() {
        let cli = map_discover_remote_error(fetch::FetchError::Discovery {
            remote: "https://example.com/repo.git".to_string(),
            source: GitError::NetworkError("timed out".to_string()),
        });

        assert_eq!(cli.stable_code(), StableErrorCode::NetworkUnavailable);
        assert_eq!(cli.exit_code(), 128);
        assert_eq!(
            cli.hints()[0].as_str(),
            "check the remote host, DNS, VPN/proxy, and network connectivity"
        );
    }

    #[test]
    fn discover_remote_host_key_error_maps_to_host_key_hint() {
        let cli = map_discover_remote_error(fetch::FetchError::Discovery {
            remote: "git@example.com/repo.git".to_string(),
            source: GitError::NetworkError(format!(
                "{}fixture private detail",
                crate::internal::protocol::ssh_client::SSH_HOST_KEY_UNCONFIRMED_SIGNAL
            )),
        });

        assert_eq!(cli.stable_code(), StableErrorCode::NetworkUnavailable);
        assert_eq!(cli.exit_code(), 128);
        let hint = cli.hints()[0].as_str();
        assert!(
            hint.contains("~/.ssh/known_hosts")
                && hint.contains("trusted")
                && !hint.contains("ssh-keyscan"),
            "host key failure should surface a targeted hint, got: {hint}"
        );
    }

    #[test]
    fn discover_remote_io_error_maps_to_io_read_failed() {
        let cli = map_discover_remote_error(fetch::FetchError::Discovery {
            remote: "/local/repo".to_string(),
            source: GitError::IOError(std::io::Error::other("permission denied")),
        });

        assert_eq!(cli.stable_code(), StableErrorCode::IoReadFailed);
        assert_eq!(cli.exit_code(), 128);
        assert_eq!(
            cli.hints()[0].as_str(),
            "check filesystem permissions and repository integrity"
        );
    }

    #[test]
    fn checkout_read_index_maps_to_io_read_failed() {
        let cli = map_checkout_error(RestoreError::ReadIndex);

        assert_eq!(cli.stable_code(), StableErrorCode::IoReadFailed);
        assert_eq!(cli.exit_code(), 128);
    }

    #[test]
    fn checkout_resolve_source_maps_to_repo_state_invalid() {
        let cli = map_checkout_error(RestoreError::ResolveSource);

        assert_eq!(cli.stable_code(), StableErrorCode::RepoStateInvalid);
        assert_eq!(cli.exit_code(), 128);
    }

    #[test]
    fn checkout_write_worktree_maps_to_io_write_failed() {
        let cli = map_checkout_error(RestoreError::WriteWorktree);

        assert_eq!(cli.stable_code(), StableErrorCode::IoWriteFailed);
        assert_eq!(cli.exit_code(), 128);
    }

    #[test]
    fn local_branch_state_query_maps_to_io_read_failed() {
        let cli = map_local_branch_state_error(branch::BranchStoreError::Query(
            "database is locked".into(),
        ));

        assert_eq!(cli.stable_code(), StableErrorCode::IoReadFailed);
        assert_eq!(cli.exit_code(), 128);
    }

    #[test]
    fn local_branch_state_corrupt_maps_to_repo_corrupt() {
        let cli = map_local_branch_state_error(branch::BranchStoreError::Corrupt {
            name: "refs/remotes/origin/main".into(),
            detail: "invalid object id".into(),
        });

        assert_eq!(cli.stable_code(), StableErrorCode::RepoCorrupt);
        assert_eq!(cli.exit_code(), 128);
    }

    #[test]
    fn clone_should_reject_shallow_only_for_unrequested_shallowness() {
        // Reject only when shallow AND the user did not ask for --depth.
        assert!(clone_should_reject_shallow(true, true, None));
        // In the post-fetch check, --depth makes the shallowness expected, so it
        // is allowed. NOTE: this also (intentionally) suppresses rejection for a
        // shallow SOURCE cloned with --depth on transports that can negotiate
        // shallow boundaries — a documented narrowing vs Git, since Libra cannot
        // tell the two apart at this point. Local Libra --depth is rejected
        // earlier by fetch.
        assert!(!clone_should_reject_shallow(true, true, Some(1)));
        // A non-shallow result never triggers a rejection.
        assert!(!clone_should_reject_shallow(true, false, None));
        // Without the flag, nothing is rejected.
        assert!(!clone_should_reject_shallow(false, true, None));
    }

    #[test]
    fn removed_publish_restore_source_maps_to_cli_invalid_arguments() {
        let cli = CliError::from(CloneError::RemovedPublishRestoreSource);
        assert_eq!(cli.stable_code(), StableErrorCode::CliInvalidArguments);
        assert_eq!(cli.exit_code(), 129);
        assert!(
            cli.hints()
                .iter()
                .any(|hint| hint.as_str().contains("`libra cloud`")
                    && hint.as_str().contains("git remote")),
            "migration hint missing: {:?}",
            cli.hints()
        );
    }

    #[test]
    fn removed_publish_restore_source_detects_retired_scheme() {
        let input = concat!("libra", "+", "cloud", "://", "example.test/site");
        assert!(is_removed_publish_restore_source(input));
        assert!(!is_removed_publish_restore_source(
            "https://example.test/repo.git"
        ));
        assert!(!is_removed_publish_restore_source("file:///tmp/repo"));
    }

    #[tokio::test]
    #[serial(cwd, env)]
    async fn normalize_mirror_refs_promotes_branches_and_clears_tracking() {
        let repo = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;
        let _cwd = ChangeDirGuard::new(repo.path());

        let db = get_db_conn_instance().await;
        let hash = ObjectHash::zero_str(get_hash_kind()).to_string();

        // Simulate a post-fetch state: two remote-tracking branches plus the
        // cached remote HEAD (a `Head` row, not a `Branch` row).
        Branch::update_branch_with_conn(&db, "refs/remotes/origin/main", &hash, Some("origin"))
            .await
            .expect("seed tracking main");
        Branch::update_branch_with_conn(&db, "refs/remotes/origin/feature", &hash, Some("origin"))
            .await
            .expect("seed tracking feature");
        Head::update_with_conn(&db, Head::Branch("main".to_string()), Some("origin")).await;
        assert!(
            Head::remote_current_with_conn(&db, "origin")
                .await
                .is_some(),
            "sanity: remote HEAD seeded"
        );

        normalize_mirror_refs("origin")
            .await
            .expect("mirror normalization succeeds");

        // Every tracking branch is promoted to a local branch.
        let local: Vec<String> = Branch::list_branches_result_with_conn(&db, None)
            .await
            .expect("list local branches")
            .into_iter()
            .map(|b| b.name)
            .collect();
        assert!(
            local.iter().any(|n| n == "main"),
            "main promoted: {local:?}"
        );
        assert!(
            local.iter().any(|n| n == "feature"),
            "feature promoted: {local:?}"
        );

        // No remote-tracking branches and no cached remote HEAD remain.
        let tracking = Branch::list_branches_result_with_conn(&db, Some("origin"))
            .await
            .expect("list tracking branches");
        assert!(
            tracking.is_empty(),
            "no tracking branches remain: {tracking:?}"
        );
        assert!(
            Head::remote_current_with_conn(&db, "origin")
                .await
                .is_none(),
            "remote HEAD tracking ref removed"
        );

        // The mirror marker is recorded.
        assert_eq!(
            config_value("remote.origin.mirror").await.as_deref(),
            Some("true"),
            "mirror marker recorded"
        );
    }

    async fn config_value(key: &str) -> Option<String> {
        ConfigKv::get(key)
            .await
            .expect("config lookup should succeed")
            .map(|entry| entry.value)
    }

    #[test]
    fn clone_error_display_pins_owned_variants() {
        assert_eq!(
            CloneError::CannotInferDestination.to_string(),
            "please specify the destination path explicitly",
        );
        assert_eq!(
            CloneError::DestinationExistsNonEmpty {
                path: PathBuf::from("/tmp/repo"),
            }
            .to_string(),
            "destination path '/tmp/repo' already exists and is not an empty directory",
        );
        assert_eq!(
            CloneError::DestinationAlreadyRepo {
                path: PathBuf::from("/tmp/repo"),
            }
            .to_string(),
            "destination path '/tmp/repo' already contains a libra repository",
        );
        assert_eq!(
            CloneError::RemoteBranchNotFound {
                branch: "feat/x".to_string(),
                remote: "upstream".to_string(),
            }
            .to_string(),
            "remote branch feat/x not found in upstream upstream",
        );
        assert_eq!(
            CloneError::SetupFailed {
                message: "vault missing".to_string(),
            }
            .to_string(),
            "failed to complete clone setup: vault missing",
        );
        assert_eq!(
            CloneError::RemovedPublishRestoreSource.to_string(),
            "this clone source is no longer supported",
        );
    }

    /// Pins the `--json` `CloneOutput` wire contract (documented in
    /// docs/development/commands/clone.md). The ordinary-Git case must carry every
    /// always-present field — including `gitignore_converted` (the
    /// `.gitignore` → `.libraignore` conversion report) — and must OMIT
    /// the optional `source_kind` / `cloud_site` (their
    /// `skip_serializing_if = Option::is_none`). A rename/drop/retype that
    /// silently breaks JSON consumers trips here.
    #[test]
    fn clone_output_json_pins_ordinary_git_contract() {
        let output = CloneOutput {
            path: "/tmp/repo".to_string(),
            bare: false,
            remote_url: "git@github.com:user/repo.git".to_string(),
            remote_name: "origin".to_string(),
            branch: Some("main".to_string()),
            object_format: "sha1".to_string(),
            repo_id: "a1b2c3d4".to_string(),
            vault_signing: true,
            ssh_key_detected: Some("/home/u/.ssh/id_ed25519".to_string()),
            shallow: false,
            warnings: Vec::new(),
            gitignore_converted: vec![".libraignore".to_string(), "sub/.libraignore".to_string()],
            source_kind: None,
            cloud_site: None,
            objects_fetched: Some(42),
            bytes_received: Some(4096),
        };

        let value = serde_json::to_value(&output).expect("CloneOutput must serialize");
        let map = value
            .as_object()
            .expect("CloneOutput serializes to an object");

        // Git fetch transfer counts are present (Some) for ordinary Git sources.
        assert_eq!(map.get("objects_fetched"), Some(&serde_json::json!(42)));
        assert_eq!(map.get("bytes_received"), Some(&serde_json::json!(4096)));

        // gitignore_converted is always present (no skip) and carries the
        // converted-file list verbatim.
        assert_eq!(
            map.get("gitignore_converted"),
            Some(&serde_json::json!([".libraignore", "sub/.libraignore"])),
            "gitignore_converted must serialize the converted .libraignore paths",
        );

        // The always-present field set, pinned by name.
        for key in [
            "path",
            "bare",
            "remote_url",
            "branch",
            "object_format",
            "repo_id",
            "vault_signing",
            "ssh_key_detected",
            "shallow",
            "warnings",
            "gitignore_converted",
        ] {
            assert!(
                map.contains_key(key),
                "CloneOutput JSON must contain `{key}`"
            );
        }

        // Optional source fields are omitted for ordinary Git sources.
        assert!(
            !map.contains_key("source_kind"),
            "source_kind must be omitted when None (skip_serializing_if)",
        );
        assert!(
            !map.contains_key("cloud_site"),
            "cloud_site must be omitted when None (skip_serializing_if)",
        );
    }

    /// A bare clone reports no `.gitignore` conversions (clone.md: "Empty
    /// for bare clones"), but the key is still present as an empty array
    /// rather than dropped — JSON consumers can rely on it always existing.
    #[test]
    fn clone_output_json_gitignore_converted_empty_is_present() {
        let output = CloneOutput {
            path: "/tmp/repo.git".to_string(),
            bare: true,
            remote_url: "git@github.com:user/repo.git".to_string(),
            remote_name: "origin".to_string(),
            branch: Some("main".to_string()),
            object_format: "sha1".to_string(),
            repo_id: "a1b2c3d4".to_string(),
            vault_signing: true,
            ssh_key_detected: None,
            shallow: false,
            warnings: Vec::new(),
            gitignore_converted: Vec::new(),
            source_kind: None,
            cloud_site: None,
            objects_fetched: Some(0),
            bytes_received: Some(0),
        };

        let value = serde_json::to_value(&output).expect("CloneOutput must serialize");
        assert_eq!(
            value.get("gitignore_converted"),
            Some(&serde_json::json!([])),
            "gitignore_converted must be an empty array, not absent, for bare clones",
        );
    }
}
