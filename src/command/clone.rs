//! Supports cloning repositories by parsing URLs, fetching objects via protocol
//! clients, checking out the working tree, and writing initial refs/config.
//!
//! The execution layer (`execute_clone`) produces a structured [`CloneOutput`]
//! and the rendering layer (`execute_safe`) converts it to human / JSON /
//! machine output according to the global [`OutputConfig`].

use std::{
    collections::BTreeSet,
    env, fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use clap::Parser;
use git_internal::{
    errors::GitError,
    hash::{ObjectHash, get_hash_kind},
};
use object_store::{aws::AmazonS3Builder, local::LocalFileSystem};
use sea_orm::{DatabaseConnection, DatabaseTransaction};
use serde::Serialize;
use url::Url;

use super::fetch::{self, RemoteSpecErrorKind};
use crate::{
    command::{
        self,
        cloud::{restore_indexed_objects_from_remote, restore_metadata_strict},
        init::InitError,
        restore::{RestoreArgs, RestoreError},
    },
    internal::{
        ai::history::HistoryManager,
        branch::{self, Branch},
        config::{
            ConfigKv, LocalIdentityTarget, RemoteConfig, read_cascaded_config_value_decrypted,
            resolve_env_for_target,
        },
        db::get_db_conn_instance,
        head::Head,
        protocol::DiscoveryResult,
        publish::{
            ai_export::publish_ai_graph_relative_key,
            contract::{
                AiObjectLayer, PublishAiBundle, PublishAiGraph, PublishAiIndex, PublishAiObject,
                RedactionMode,
            },
            snapshot::sha256_hex,
        },
        reflog::{ReflogAction, ReflogContext, with_reflog},
    },
    utils::{
        d1_client::{
            D1Client, ObjectIndexRow, PublishAiObjectRow, PublishAiVersionRow, PublishRefRow,
            PublishRevisionRow, PublishSiteRow, RepositoryRow,
        },
        error::{CliError, CliResult, StableErrorCode},
        ignore as ignore_utils,
        output::{OutputConfig, emit_json_data},
        pager::LIBRA_TEST_ENV,
        path,
        storage::{
            Storage, local::LocalStorage, publish_storage::PublishStorage, remote::RemoteStorage,
        },
        storage_ext::StorageExt,
        util,
    },
};

const ISSUE_URL: &str = "https://github.com/libra-tools/libra/issues";
const CLOUD_CLONE_TEST_R2_ROOT_ENV: &str = "LIBRA_CLOUD_CLONE_TEST_R2_ROOT";
const CLOUD_CLONE_D1_API_BASE_URL_ENV: &str = "LIBRA_D1_API_BASE_URL";

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
    /// instead of the default `origin`, matching `git clone -o`. Applies to
    /// standard clones; libra+cloud clones always use `origin`.
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
    /// (`libra fetch` is not yet mirror-aware). Not supported for `libra+cloud://`
    /// sources.
    #[clap(long = "mirror")]
    pub mirror: bool,

    /// Partial-clone object filter (Git's `--filter <spec>`, e.g. `blob:none`).
    /// Accepted for compatibility but a no-op with a warning: Libra has no
    /// partial-clone/promisor support, so the filter is ignored (no objects are
    /// excluded) and a complete clone is performed (subject only to `--depth` if
    /// also given) — mirroring Git's own behavior when a server does not advertise
    /// filtering. Not supported for `libra+cloud://` sources.
    #[clap(long = "filter", value_name = "spec")]
    pub filter: Option<String>,

    /// Deepen history to commits more recent than a date (Git's
    /// `--shallow-since <date>`). Accepted for compatibility but a no-op with a
    /// warning: Libra bounds shallow history only by `--depth`, so the date bound
    /// is not applied (history is limited only by `--depth` if also given). Not
    /// supported for `libra+cloud://` sources.
    #[clap(long = "shallow-since", value_name = "date")]
    pub shallow_since: Option<String>,

    /// Deepen history, excluding commits reachable from a ref (Git's
    /// `--shallow-exclude <rev>`). Accepted for compatibility but a no-op with a
    /// warning: Libra bounds shallow history only by `--depth`, so the exclusion
    /// is not applied (history is limited only by `--depth` if also given). May be
    /// given multiple times. Not supported for `libra+cloud://` sources.
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
    /// the checkout that keeps the repository commit-safe). Not supported for
    /// `libra+cloud://` sources.
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
    /// Cloudflare publish metadata for `libra+cloud://` sources.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud_site: Option<CloudCloneSiteOutput>,
    /// Number of objects received in the fetch pack (Git sources only; omitted
    /// for `libra+cloud://` restores, which download indexed objects from R2
    /// rather than a pack stream).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objects_fetched: Option<usize>,
    /// Bytes received in the fetch pack stream (Git sources only; omitted for
    /// `libra+cloud://` restores).
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
    #[error("clone domain '{domain}' is not configured for libra+cloud restore")]
    CloudCloneDomainNotConfigured {
        domain: String,
        missing_keys: String,
    },
    #[error("D1 API token is not configured for clone domain '{domain}'")]
    CloudCloneD1ApiTokenNotConfigured { domain: String },
    #[error("D1 API base URL is invalid for clone domain '{domain}': {message}")]
    CloudCloneD1ApiBaseUrlInvalid { domain: String, message: String },
    #[error("R2 credentials are not configured for clone domain '{domain}'")]
    CloudCloneR2CredentialsNotConfigured {
        domain: String,
        missing_keys: String,
    },
    #[error("failed to read R2 configuration for clone domain '{domain}': {source}")]
    CloudCloneR2ConfigRead {
        domain: String,
        #[source]
        source: anyhow::Error,
    },
    #[error("failed to build R2 client for clone domain '{domain}': {message}")]
    CloudCloneR2ClientBuildFailed { domain: String, message: String },
    #[error("failed to read clone domain '{domain}' configuration: {source}")]
    CloudCloneDomainConfigRead {
        domain: String,
        #[source]
        source: anyhow::Error,
    },
    #[error("{option} is not supported with libra+cloud:// clone sources: {reason}")]
    UnsupportedCloudCloneOption {
        option: &'static str,
        reason: &'static str,
        hint: &'static str,
    },
    #[error(
        "failed to resolve libra+cloud site {target} in clone domain '{domain}' \
         via D1 (code {code}): {message}"
    )]
    CloudPublishSiteLookupFailed {
        domain: String,
        target: String,
        code: i32,
        message: String,
    },
    #[error("libra+cloud site {target} was not found in clone domain '{domain}'")]
    CloudPublishSiteNotFound { domain: String, target: String },
    #[error("libra+cloud site {target} in clone domain '{domain}' is not active: {status}")]
    CloudPublishSiteUnavailable {
        domain: String,
        target: String,
        status: String,
    },
    #[error(
        "failed to resolve libra+cloud metadata for site {site_id} in clone domain '{domain}' during {operation} (code {code}): {message}"
    )]
    CloudPublishMetadataLookupFailed {
        domain: String,
        site_id: String,
        operation: &'static str,
        code: i32,
        message: String,
    },
    #[error(
        "libra+cloud site {target} in clone domain '{domain}' has no repositories row for repo_id {repo_id}"
    )]
    CloudPublishRepositoryNotFound {
        domain: String,
        target: String,
        repo_id: String,
    },
    #[error("libra+cloud site {target} in clone domain '{domain}' has no published refs")]
    CloudPublishRefsMissing { domain: String, target: String },
    #[error(
        "libra+cloud ref selector '{selector}' did not match a published branch or tag for site {target} in clone domain '{domain}'"
    )]
    CloudPublishRefNotFound {
        domain: String,
        target: String,
        selector: String,
    },
    #[error(
        "libra+cloud ref selector '{selector}' is ambiguous for site {target} in clone domain '{domain}'; matches: {matches}"
    )]
    CloudPublishRefAmbiguous {
        domain: String,
        target: String,
        selector: String,
        matches: String,
    },
    #[error(
        "libra+cloud site {target} in clone domain '{domain}' has no default_ref for clone checkout"
    )]
    CloudPublishDefaultRefMissing { domain: String, target: String },
    #[error(
        "libra+cloud site {target} in clone domain '{domain}' has no latest_revision_oid for revision=latest"
    )]
    CloudPublishLatestRevisionMissing { domain: String, target: String },
    #[error(
        "published revision {revision_oid} for libra+cloud site {target} in clone domain '{domain}' was not found"
    )]
    CloudPublishRevisionNotFound {
        domain: String,
        target: String,
        revision_oid: String,
    },
    #[error(
        "libra+cloud site {target} in clone domain '{domain}' has no object_index rows for repo_id {repo_id}"
    )]
    CloudPublishObjectIndexMissing {
        domain: String,
        target: String,
        repo_id: String,
    },
    #[error(
        "R2 object {object_oid} for libra+cloud site {target} in clone domain '{domain}' is missing"
    )]
    CloudPublishObjectMissing {
        domain: String,
        target: String,
        object_oid: String,
    },
    #[error(
        "failed to restore R2 objects for libra+cloud site {target} in clone domain '{domain}': {message}"
    )]
    CloudPublishObjectRestoreFailed {
        domain: String,
        target: String,
        message: String,
    },
    #[error(
        "failed to restore refs metadata for libra+cloud site {target} in clone domain '{domain}': {message}"
    )]
    CloudPublishRefsMetadataRestoreFailed {
        domain: String,
        target: String,
        message: String,
    },
    #[error(
        "failed to restore AI object model for libra+cloud site {target} in clone domain '{domain}': {message}"
    )]
    CloudPublishAiRestoreFailed {
        domain: String,
        target: String,
        message: String,
    },
    #[error(
        "failed to configure checkout for libra+cloud site {target} in clone domain '{domain}': {message}"
    )]
    CloudPublishCheckoutSetupFailed {
        domain: String,
        target: String,
        message: String,
    },
    #[error(
        "object_index row {object_oid} for libra+cloud site {target} in clone domain '{domain}' is not a valid object id: {reason}"
    )]
    CloudPublishObjectIndexInvalid {
        domain: String,
        target: String,
        object_oid: String,
        reason: String,
    },
    #[error("invalid libra+cloud clone source '{input}': {reason}")]
    InvalidCloudPublishSource { input: String, reason: String },
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
            CloneError::CloudCloneDomainNotConfigured {
                ref domain,
                ref missing_keys,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::AuthMissingCredentials)
                .with_detail("clone_domain", domain.clone())
                .with_detail("missing_keys", missing_keys.clone())
                .with_hint(format!(
                    "configure cloud.clone_domains.{domain}.account_id, \
                     cloud.clone_domains.{domain}.d1_database_id, and \
                     cloud.clone_domains.{domain}.r2_bucket, and set LIBRA_D1_API_TOKEN \
                     before cloning this source."
                )),
            CloneError::CloudCloneD1ApiTokenNotConfigured { ref domain } => {
                CliError::fatal(error.to_string())
                    .with_stable_code(StableErrorCode::AuthMissingCredentials)
                    .with_detail("clone_domain", domain.clone())
                    .with_detail(
                        "missing_keys",
                        "vault.env.LIBRA_D1_API_TOKEN or LIBRA_D1_API_TOKEN",
                    )
                .with_hint(
                    "set vault.env.LIBRA_D1_API_TOKEN with `libra config set`, or export \
                     LIBRA_D1_API_TOKEN, so the CLI can query the configured D1 database.",
                )
            }
            CloneError::CloudCloneD1ApiBaseUrlInvalid {
                ref domain,
                ref message,
            } => CliError::command_usage(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_detail("clone_domain", domain.clone())
                .with_detail("d1_api_base_url_error", message.clone())
                .with_hint(
                    "unset LIBRA_D1_API_BASE_URL or set it to a valid Cloudflare-compatible API root.",
                ),
            CloneError::CloudCloneR2CredentialsNotConfigured {
                ref domain,
                ref missing_keys,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::AuthMissingCredentials)
                .with_detail("clone_domain", domain.clone())
                .with_detail("missing_keys", missing_keys.clone())
                .with_hint(
                    "set vault.env.LIBRA_STORAGE_ENDPOINT, vault.env.LIBRA_STORAGE_ACCESS_KEY, \
                     and vault.env.LIBRA_STORAGE_SECRET_KEY with `libra config set`, or export \
                     the matching LIBRA_STORAGE_* variables.",
                ),
            CloneError::CloudCloneR2ConfigRead { ref domain, .. } => {
                CliError::fatal(error.to_string())
                    .with_stable_code(StableErrorCode::IoReadFailed)
                    .with_detail("clone_domain", domain.clone())
                    .with_hint("check the local/global Libra config database and vault state.")
            }
            CloneError::CloudCloneR2ClientBuildFailed {
                ref domain,
                ref message,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::NetworkProtocol)
                .with_detail("clone_domain", domain.clone())
                .with_detail("r2_error_message", message.clone())
                .with_hint("check the R2 endpoint, bucket, region, and access credentials."),
            CloneError::CloudCloneDomainConfigRead { ref domain, .. } => {
                CliError::fatal(error.to_string())
                    .with_stable_code(StableErrorCode::IoReadFailed)
                    .with_detail("clone_domain", domain.clone())
                    .with_hint("check the local/global Libra config database and vault state.")
            }
            CloneError::UnsupportedCloudCloneOption {
                ref option,
                ref hint,
                ..
            } => CliError::command_usage(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_detail("option", option.to_string())
                .with_hint(hint.to_string()),
            CloneError::CloudPublishSiteLookupFailed {
                ref domain,
                ref target,
                code,
                ref message,
            } => {
                let stable_code = if matches!(code, 401 | 403 | 1002) {
                    StableErrorCode::AuthPermissionDenied
                } else {
                    StableErrorCode::NetworkUnavailable
                };
                CliError::fatal(error.to_string())
                    .with_stable_code(stable_code)
                    .with_detail("clone_domain", domain.clone())
                    .with_detail("cloud_target", target.clone())
                    .with_detail("d1_error_code", code.to_string())
                    .with_detail("d1_error_message", message.clone())
                    .with_hint(
                        "check the D1 database id, API token, account id, and network access.",
                    )
            }
            CloneError::CloudPublishSiteNotFound {
                ref domain,
                ref target,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoNotFound)
                .with_detail("clone_domain", domain.clone())
                .with_detail("cloud_target", target.clone())
                .with_hint(
                    "check the clone domain, slug/repo id, or publish the site before cloning.",
                ),
            CloneError::CloudPublishSiteUnavailable {
                ref domain,
                ref target,
                ref status,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_detail("clone_domain", domain.clone())
                .with_detail("cloud_target", target.clone())
                .with_detail("cloud_site_status", status.clone())
                .with_hint("enable the publish site before cloning it from libra+cloud://."),
            CloneError::CloudPublishMetadataLookupFailed {
                ref domain,
                ref site_id,
                ref operation,
                code,
                ref message,
            } => {
                let stable_code = if matches!(code, 401 | 403 | 1002) {
                    StableErrorCode::AuthPermissionDenied
                } else {
                    StableErrorCode::NetworkUnavailable
                };
                CliError::fatal(error.to_string())
                    .with_stable_code(stable_code)
                    .with_detail("clone_domain", domain.clone())
                    .with_detail("cloud_site_id", site_id.clone())
                    .with_detail("cloud_lookup", operation.to_string())
                    .with_detail("d1_error_code", code.to_string())
                    .with_detail("d1_error_message", message.clone())
                    .with_hint(
                        "check the D1 database id, API token, account id, and publish schema.",
                    )
            }
            CloneError::CloudPublishRepositoryNotFound {
                ref domain,
                ref target,
                ref repo_id,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoNotFound)
                .with_detail("clone_domain", domain.clone())
                .with_detail("cloud_target", target.clone())
                .with_detail("cloud_repo_id", repo_id.clone())
                .with_hint("run 'libra cloud sync' for this repository before cloud clone."),
            CloneError::CloudPublishRefsMissing {
                ref domain,
                ref target,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_detail("clone_domain", domain.clone())
                .with_detail("cloud_target", target.clone())
                .with_hint("run a full 'libra publish sync' so D1 publish_refs are available."),
            CloneError::CloudPublishRefNotFound {
                ref domain,
                ref target,
                ref selector,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_detail("clone_domain", domain.clone())
                .with_detail("cloud_target", target.clone())
                .with_detail("cloud_selector", format!("ref:{selector}"))
                .with_hint(
                    "use a published full ref such as 'refs/heads/main' or 'refs/tags/v1.0.0'.",
                ),
            CloneError::CloudPublishRefAmbiguous {
                ref domain,
                ref target,
                ref selector,
                ref matches,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_detail("clone_domain", domain.clone())
                .with_detail("cloud_target", target.clone())
                .with_detail("cloud_selector", format!("ref:{selector}"))
                .with_detail("cloud_ref_matches", matches.clone())
                .with_hint(
                    "branch and tag short names conflict; use the complete ref name in ?ref=.",
                ),
            CloneError::CloudPublishDefaultRefMissing {
                ref domain,
                ref target,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_detail("clone_domain", domain.clone())
                .with_detail("cloud_target", target.clone())
                .with_hint(
                    "run a full 'libra publish sync' so publish_sites.default_ref is populated.",
                ),
            CloneError::CloudPublishLatestRevisionMissing {
                ref domain,
                ref target,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_detail("clone_domain", domain.clone())
                .with_detail("cloud_target", target.clone())
                .with_hint(
                    "run a full 'libra publish sync' so publish_sites.latest_revision_oid is populated.",
                ),
            CloneError::CloudPublishRevisionNotFound {
                ref domain,
                ref target,
                ref revision_oid,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_detail("clone_domain", domain.clone())
                .with_detail("cloud_target", target.clone())
                .with_detail("cloud_revision_oid", revision_oid.clone())
                .with_hint(
                    "rerun 'libra publish sync' so the selected revision is marked published.",
                ),
            CloneError::CloudPublishObjectIndexMissing {
                ref domain,
                ref target,
                ref repo_id,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_detail("clone_domain", domain.clone())
                .with_detail("cloud_target", target.clone())
                .with_detail("cloud_repo_id", repo_id.clone())
                .with_hint("run 'libra cloud sync --force' so D1 object_index contains git objects."),
            CloneError::CloudPublishObjectMissing {
                ref domain,
                ref target,
                ref object_oid,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_detail("clone_domain", domain.clone())
                .with_detail("cloud_target", target.clone())
                .with_detail("object_oid", object_oid.clone())
                .with_hint(
                    "run 'libra cloud sync --force' and then 'libra publish sync' again before cloning.",
                ),
            CloneError::CloudPublishObjectRestoreFailed {
                ref domain,
                ref target,
                ref message,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_detail("clone_domain", domain.clone())
                .with_detail("cloud_target", target.clone())
                .with_detail("restore_error", message.clone())
                .with_hint(
                    "run 'libra cloud sync --force' and then 'libra publish sync' again before cloning.",
                ),
            CloneError::CloudPublishRefsMetadataRestoreFailed {
                ref domain,
                ref target,
                ref message,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_detail("clone_domain", domain.clone())
                .with_detail("cloud_target", target.clone())
                .with_detail("restore_error", message.clone())
                .with_hint(
                    "run 'libra cloud sync --force' so refs metadata is available in R2.",
                ),
            CloneError::CloudPublishAiRestoreFailed {
                ref domain,
                ref target,
                ref message,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_detail("clone_domain", domain.clone())
                .with_detail("cloud_target", target.clone())
                .with_detail("restore_error", message.clone())
                .with_hint("rerun 'libra publish sync' so AI object model R2/D1 rows agree."),
            CloneError::CloudPublishCheckoutSetupFailed {
                ref domain,
                ref target,
                ref message,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_detail("clone_domain", domain.clone())
                .with_detail("cloud_target", target.clone())
                .with_detail("checkout_error", message.clone())
                .with_hint("check that the selected published ref points to a commit."),
            CloneError::CloudPublishObjectIndexInvalid {
                ref domain,
                ref target,
                ref object_oid,
                ref reason,
            } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_detail("clone_domain", domain.clone())
                .with_detail("cloud_target", target.clone())
                .with_detail("object_oid", object_oid.clone())
                .with_detail("reason", reason.clone())
                .with_hint("repair the D1 object_index row before cloud clone can restore it."),
            CloneError::InvalidCloudPublishSource { .. } => {
                CliError::command_usage(error.to_string())
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
                    .with_hint(
                        "use `libra+cloud://<clone-domain>/<slug>` or \
                         `libra+cloud://<clone-domain>/repo/<repo_id>`; pass only one of \
                         `?ref=<branch|tag|full-ref>` or `?revision=<oid|latest>`",
                    )
            }
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
            GitError::NetworkError(_) => {
                CliError::fatal(format!("remote discovery failed: {source}"))
                    .with_stable_code(StableErrorCode::NetworkUnavailable)
                    .with_hint(
                        "check the remote host, DNS, VPN/proxy, and network connectivity",
                    )
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
    let (result, cleanup_warning) = execute_clone(&args, &original_dir, output).await;

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
    // Intercept Cloudflare publish sources before generic remote discovery.
    // `libra+cloud://` is a Libra D1/R2 restore source, not a Git transport.
    if args.remote_repo.starts_with("libra+cloud://") {
        let source =
            parse_cloud_publish_source(&args.remote_repo).map_err(|error| (error, None))?;
        validate_cloud_clone_option_compatibility(args).map_err(|error| (error, None))?;
        let clone_config = load_cloud_clone_domain_config(&source)
            .await
            .map_err(|error| (error, None))?;
        let restore_plan = resolve_cloud_publish_restore_plan(&source, &clone_config)
            .await
            .map_err(|error| (error, None))?;
        let r2_storage =
            create_cloud_clone_remote_storage(&source, &clone_config, &restore_plan.site.repo_id)
                .await
                .map_err(|error| (error, None))?;
        return execute_cloud_publish_clone(
            args,
            &source,
            restore_plan,
            r2_storage,
            original_dir,
            output,
        )
        .await;
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

#[allow(clippy::result_large_err)]
async fn execute_cloud_publish_clone(
    args: &CloneArgs,
    source: &CloudPublishSource,
    restore_plan: CloudPublishRestorePlan,
    r2_storage: RemoteStorage,
    original_dir: &Path,
    output: &OutputConfig,
) -> Result<CloneOutput, (CloneError, Option<String>)> {
    let local_path = resolve_cloud_clone_local_path(args, original_dir, &restore_plan);
    let metadata_root = local_path.join(util::ROOT_DIR);

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

    clone_cloud_publish_into_destination(
        args,
        source,
        &restore_plan,
        &r2_storage,
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

fn resolve_cloud_clone_local_path(
    args: &CloneArgs,
    original_dir: &Path,
    restore_plan: &CloudPublishRestorePlan,
) -> PathBuf {
    let local_path = args
        .local_path
        .clone()
        .unwrap_or_else(|| restore_plan.site.slug.clone());
    let local_path = PathBuf::from(local_path);
    if local_path.is_absolute() {
        local_path
    } else {
        original_dir.join(local_path)
    }
}

async fn clone_cloud_publish_into_destination(
    args: &CloneArgs,
    source: &CloudPublishSource,
    restore_plan: &CloudPublishRestorePlan,
    r2_storage: &RemoteStorage,
    local_path: &Path,
    original_dir: &Path,
    output: &OutputConfig,
) -> Result<CloneOutput, CloneError> {
    #[cfg(test)]
    let _cwd_lock = crate::utils::test::cwd_lock_guard();
    env::set_current_dir(local_path).map_err(|source| CloneError::ChangeDirectory {
        path: local_path.to_path_buf(),
        source,
    })?;

    let object_format = cloud_object_format(&restore_plan.object_indexes);

    if !output.quiet && !output.is_json() {
        eprintln!("Initializing repository ...");
    }
    let init_output = command::init::run_init(command::init::InitArgs {
        bare: false,
        template: None,
        initial_branch: cloud_checkout_branch_name(&restore_plan.checkout),
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

    if !output.quiet && !output.is_json() {
        eprintln!("Restoring objects from Cloudflare R2 ...");
    }
    let local_storage = LocalStorage::new(path::objects());
    let object_report = restore_indexed_objects_from_remote(
        &restore_plan.object_indexes,
        r2_storage,
        &local_storage,
    )
    .await
    .map_err(|source_error| CloneError::CloudPublishObjectRestoreFailed {
        domain: source.clone_domain.clone(),
        target: site_target_label(source, &restore_plan.site),
        message: source_error.to_string(),
    })?;
    if object_report.failed > 0 {
        return Err(CloneError::CloudPublishObjectRestoreFailed {
            domain: source.clone_domain.clone(),
            target: site_target_label(source, &restore_plan.site),
            message: cloud_object_restore_failure_message(&object_report.warnings),
        });
    }

    if !output.quiet && !output.is_json() {
        eprintln!("Restoring refs metadata ...");
    }
    let db_conn = get_db_conn_instance().await;
    restore_metadata_strict(&db_conn, r2_storage)
        .await
        .map_err(|error| CloneError::CloudPublishRefsMetadataRestoreFailed {
            domain: source.clone_domain.clone(),
            target: site_target_label(source, &restore_plan.site),
            message: error.to_string(),
        })?;
    restore_cloud_publish_ai_objects(source, restore_plan, r2_storage, &local_storage, &db_conn)
        .await?;

    configure_cloud_publish_checkout(source, restore_plan, &args.remote_repo).await?;

    // `--no-checkout`: leave the working tree unpopulated (objects/refs/HEAD are
    // already restored above), matching `git clone --no-checkout`.
    if !args.no_checkout {
        if !output.quiet && !output.is_json() {
            eprintln!("Checking out working copy ...");
        }
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

    let mut warnings = init_output.warnings.clone();
    warnings.extend(object_report.warnings);
    warnings.extend(object_alternates_warning(args));
    warnings.extend(unsupported_fetch_optimization_warnings(args));
    let summary = ignore_utils::convert_gitignore_files_to_libraignore(local_path, local_path)
        .map_err(|source| CloneError::IgnoreFile { source })?;
    warnings.extend(summary.warnings);
    let gitignore_converted = summary
        .converted
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();

    env::set_current_dir(original_dir).map_err(|source| CloneError::RestoreDirectory {
        path: original_dir.to_path_buf(),
        source,
    })?;

    Ok(CloneOutput {
        path: local_path.to_string_lossy().into_owned(),
        bare: false,
        remote_url: args.remote_repo.clone(),
        // libra+cloud clones always use the fixed `origin` cloud-config schema.
        remote_name: "origin".to_string(),
        branch: cloud_checkout_branch_name(&restore_plan.checkout),
        object_format,
        repo_id: init_output.repo_id,
        vault_signing: init_output.vault_signing,
        ssh_key_detected: init_output.ssh_key_detected,
        shallow: false,
        warnings,
        gitignore_converted,
        source_kind: Some("cloudflare".to_string()),
        cloud_site: Some(CloudCloneSiteOutput {
            clone_domain: source.clone_domain.clone(),
            site_id: restore_plan.site.site_id.clone(),
            slug: restore_plan.site.slug.clone(),
            repo_id: restore_plan.site.repo_id.clone(),
            ref_name: restore_plan.checkout.ref_name.clone(),
            revision: restore_plan.checkout.revision_oid.clone(),
        }),
        // Cloud restores download indexed objects from R2, not a pack stream, so
        // the Git fetch transfer counts do not apply.
        objects_fetched: None,
        bytes_received: None,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CloudPublishSource {
    clone_domain: String,
    target: CloudPublishTarget,
    selector: Option<CloudPublishSelector>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CloudPublishTarget {
    Slug(String),
    RepoId(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum CloudPublishSelector {
    Ref(String),
    Revision(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CloudCloneDomainConfig {
    account_id: String,
    api_token: String,
    d1_database_id: String,
    r2_bucket: String,
    credential_profile: Option<String>,
}

#[derive(Debug)]
struct CloudPublishRestorePlan {
    site: PublishSiteRow,
    repository: RepositoryRow,
    checkout: CloudPublishCheckoutTarget,
    revision: PublishRevisionRow,
    object_indexes: Vec<ObjectIndexRow>,
    ai_objects: Vec<PublishAiObjectRow>,
    ai_versions: Vec<PublishAiVersionRow>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CloudPublishCheckoutTarget {
    revision_oid: String,
    ref_name: Option<String>,
    selector_kind: CloudPublishCheckoutSelectorKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CloudPublishCheckoutSelectorKind {
    DefaultRef,
    Ref,
    LatestRevision,
    Revision,
}

impl CloudPublishSource {
    fn target_label(&self) -> String {
        match &self.target {
            CloudPublishTarget::Slug(slug) => format!("slug:{slug}"),
            CloudPublishTarget::RepoId(repo_id) => format!("repo:{repo_id}"),
        }
    }
}

fn cloud_object_format(object_indexes: &[ObjectIndexRow]) -> String {
    if object_indexes.iter().any(|row| row.o_id.len() == 64) {
        "sha256".to_string()
    } else {
        "sha1".to_string()
    }
}

fn cloud_checkout_branch_name(checkout: &CloudPublishCheckoutTarget) -> Option<String> {
    checkout
        .ref_name
        .as_deref()
        .and_then(|ref_name| ref_name.strip_prefix("refs/heads/"))
        .map(ToString::to_string)
}

fn cloud_object_restore_failure_message(warnings: &[String]) -> String {
    if warnings.is_empty() {
        "one or more indexed objects failed to restore".to_string()
    } else {
        warnings.join("; ")
    }
}

async fn restore_cloud_publish_ai_objects(
    source: &CloudPublishSource,
    restore_plan: &CloudPublishRestorePlan,
    r2_storage: &RemoteStorage,
    local_storage: &LocalStorage,
    db_conn: &DatabaseConnection,
) -> Result<(), CloneError> {
    if restore_plan.ai_objects.is_empty()
        && restore_plan.ai_versions.is_empty()
        && restore_plan.revision.ai_index_key.is_none()
    {
        return Ok(());
    }
    if !restore_plan.ai_objects.is_empty() && restore_plan.revision.ai_index_key.is_none() {
        return Err(CloneError::CloudPublishAiRestoreFailed {
            domain: source.clone_domain.clone(),
            target: site_target_label(source, &restore_plan.site),
            message: "published revision has AI object rows but no AI index key".to_string(),
        });
    }
    if !restore_plan.ai_objects.is_empty() && restore_plan.ai_versions.is_empty() {
        return Err(CloneError::CloudPublishAiRestoreFailed {
            domain: source.clone_domain.clone(),
            target: site_target_label(source, &restore_plan.site),
            message: "published revision has AI object rows but no AI version rows".to_string(),
        });
    }

    let publish_storage = PublishStorage::new(
        r2_storage.object_store(),
        &restore_plan.site.repo_id,
        &restore_plan.site.site_id,
    )
    .map_err(|source_error| CloneError::CloudPublishAiRestoreFailed {
        domain: source.clone_domain.clone(),
        target: site_target_label(source, &restore_plan.site),
        message: source_error.to_string(),
    })?;
    let history = HistoryManager::new(
        Arc::new(local_storage.clone()),
        util::storage_path(),
        Arc::new(db_conn.clone()),
    );

    if let Some(index_key) = &restore_plan.revision.ai_index_key {
        let relative_key =
            cloud_publish_relative_r2_key(source, restore_plan, index_key, "AI index")?;
        let index: PublishAiIndex =
            publish_storage
                .get_json(&relative_key)
                .await
                .map_err(|source_error| CloneError::CloudPublishAiRestoreFailed {
                    domain: source.clone_domain.clone(),
                    target: site_target_label(source, &restore_plan.site),
                    message: source_error.to_string(),
                })?;
        validate_cloud_publish_ai_index(source, restore_plan, &index)?;
        append_cloud_publish_ai_history(
            source,
            restore_plan,
            local_storage,
            &history,
            "publish_ai_index",
            &index.revision_oid,
            &index,
        )
        .await?;
    }

    if !restore_plan.ai_objects.is_empty() {
        let graph: PublishAiGraph = publish_storage
            .get_json(&publish_ai_graph_relative_key(
                &restore_plan.revision.revision_oid,
            ))
            .await
            .map_err(|source_error| CloneError::CloudPublishAiRestoreFailed {
                domain: source.clone_domain.clone(),
                target: site_target_label(source, &restore_plan.site),
                message: source_error.to_string(),
            })?;
        validate_cloud_publish_ai_graph(source, restore_plan, &graph)?;
        append_cloud_publish_ai_history(
            source,
            restore_plan,
            local_storage,
            &history,
            "publish_ai_graph",
            &graph.revision_oid,
            &graph,
        )
        .await?;
    }

    for row in &restore_plan.ai_versions {
        let relative_key =
            cloud_publish_relative_r2_key(source, restore_plan, &row.bundle_key, "AI bundle")?;
        let bundle: PublishAiBundle =
            publish_storage
                .get_json(&relative_key)
                .await
                .map_err(|source_error| CloneError::CloudPublishAiRestoreFailed {
                    domain: source.clone_domain.clone(),
                    target: site_target_label(source, &restore_plan.site),
                    message: source_error.to_string(),
                })?;
        validate_cloud_publish_ai_bundle(source, restore_plan, row, &bundle)?;
        append_cloud_publish_ai_history(
            source,
            restore_plan,
            local_storage,
            &history,
            "publish_ai_version",
            &row.ai_version_id,
            row,
        )
        .await?;
        append_cloud_publish_ai_history(
            source,
            restore_plan,
            local_storage,
            &history,
            "publish_ai_bundle",
            &bundle.ai_version_id,
            &bundle,
        )
        .await?;
    }

    for row in &restore_plan.ai_objects {
        let relative_key =
            cloud_publish_relative_r2_key(source, restore_plan, &row.r2_key, "AI object")?;
        let object: PublishAiObject =
            publish_storage
                .get_json(&relative_key)
                .await
                .map_err(|source_error| CloneError::CloudPublishAiRestoreFailed {
                    domain: source.clone_domain.clone(),
                    target: site_target_label(source, &restore_plan.site),
                    message: source_error.to_string(),
                })?;
        validate_cloud_publish_ai_object_row(source, restore_plan, row, &object)?;

        append_cloud_publish_ai_history(
            source,
            restore_plan,
            local_storage,
            &history,
            &cloud_publish_ai_history_type(&row.object_type),
            &row.object_id,
            &object,
        )
        .await?;
    }

    Ok(())
}

fn cloud_publish_relative_r2_key(
    source: &CloudPublishSource,
    restore_plan: &CloudPublishRestorePlan,
    r2_key: &str,
    artifact: &str,
) -> Result<String, CloneError> {
    let r2_prefix = format!(
        "{}/publish/sites/{}/",
        restore_plan.site.repo_id, restore_plan.site.site_id
    );
    r2_key
        .strip_prefix(&r2_prefix)
        .map(ToString::to_string)
        .ok_or_else(|| CloneError::CloudPublishAiRestoreFailed {
            domain: source.clone_domain.clone(),
            target: site_target_label(source, &restore_plan.site),
            message: format!("{artifact} key is outside site namespace: {r2_key}"),
        })
}

async fn append_cloud_publish_ai_history<T>(
    source: &CloudPublishSource,
    restore_plan: &CloudPublishRestorePlan,
    local_storage: &LocalStorage,
    history: &HistoryManager,
    history_type: &str,
    object_id: &str,
    value: &T,
) -> Result<(), CloneError>
where
    T: Serialize + Send + Sync,
{
    let hash =
        local_storage
            .put_json(value)
            .await
            .map_err(|source_error| CloneError::CloudPublishAiRestoreFailed {
                domain: source.clone_domain.clone(),
                target: site_target_label(source, &restore_plan.site),
                message: format!(
                    "failed to store AI restore artifact {history_type}/{object_id} locally: {source_error}"
                ),
            })?;
    history
        .append(history_type, object_id, hash)
        .await
        .map_err(|source_error| CloneError::CloudPublishAiRestoreFailed {
            domain: source.clone_domain.clone(),
            target: site_target_label(source, &restore_plan.site),
            message: format!(
                "failed to index AI restore artifact {history_type}/{object_id} locally: {source_error}"
            ),
        })?;
    Ok(())
}

fn validate_cloud_publish_ai_index(
    source: &CloudPublishSource,
    restore_plan: &CloudPublishRestorePlan,
    index: &PublishAiIndex,
) -> Result<(), CloneError> {
    if index.site_id != restore_plan.site.site_id
        || index.revision_oid != restore_plan.revision.revision_oid
    {
        return Err(CloneError::CloudPublishAiRestoreFailed {
            domain: source.clone_domain.clone(),
            target: site_target_label(source, &restore_plan.site),
            message: "AI index does not match published revision".to_string(),
        });
    }
    let indexed_objects = index
        .objects
        .iter()
        .map(|entry| {
            (
                entry.object_type.as_str(),
                entry.object_id.as_str(),
                cloud_publish_ai_layer_label(entry.layer),
                entry.r2_key.as_str(),
                entry.payload_sha256.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    for row in &restore_plan.ai_objects {
        let key = (
            row.object_type.as_str(),
            row.object_id.as_str(),
            row.layer.as_str(),
            row.r2_key.as_str(),
            row.payload_sha256.as_str(),
        );
        if !indexed_objects.contains(&key) {
            return Err(CloneError::CloudPublishAiRestoreFailed {
                domain: source.clone_domain.clone(),
                target: site_target_label(source, &restore_plan.site),
                message: format!(
                    "AI index is missing object row {}/{}",
                    row.object_type, row.object_id
                ),
            });
        }
    }
    Ok(())
}

fn validate_cloud_publish_ai_graph(
    source: &CloudPublishSource,
    restore_plan: &CloudPublishRestorePlan,
    graph: &PublishAiGraph,
) -> Result<(), CloneError> {
    if graph.site_id != restore_plan.site.site_id
        || graph.revision_oid != restore_plan.revision.revision_oid
    {
        return Err(CloneError::CloudPublishAiRestoreFailed {
            domain: source.clone_domain.clone(),
            target: site_target_label(source, &restore_plan.site),
            message: "AI graph does not match published revision".to_string(),
        });
    }
    let graph_nodes = graph
        .nodes
        .iter()
        .map(|node| {
            (
                node.object_type.as_str(),
                node.object_id.as_str(),
                cloud_publish_ai_layer_label(node.layer),
                node.r2_key.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    for row in &restore_plan.ai_objects {
        let key = (
            row.object_type.as_str(),
            row.object_id.as_str(),
            row.layer.as_str(),
            row.r2_key.as_str(),
        );
        if !graph_nodes.contains(&key) {
            return Err(CloneError::CloudPublishAiRestoreFailed {
                domain: source.clone_domain.clone(),
                target: site_target_label(source, &restore_plan.site),
                message: format!(
                    "AI graph is missing object row {}/{}",
                    row.object_type, row.object_id
                ),
            });
        }
    }
    Ok(())
}

fn validate_cloud_publish_ai_bundle(
    source: &CloudPublishSource,
    restore_plan: &CloudPublishRestorePlan,
    row: &PublishAiVersionRow,
    bundle: &PublishAiBundle,
) -> Result<(), CloneError> {
    let object_count = i64::try_from(bundle.objects.len()).map_err(|source_error| {
        CloneError::CloudPublishAiRestoreFailed {
            domain: source.clone_domain.clone(),
            target: site_target_label(source, &restore_plan.site),
            message: format!("AI bundle object count is too large: {source_error}"),
        }
    })?;
    let mismatch = bundle.site_id != row.site_id
        || bundle.revision_oid != row.revision_oid
        || bundle.ai_version_id != row.ai_version_id
        || i64::from(bundle.schema_version) != row.schema_version
        || object_count != row.object_count
        || cloud_publish_ai_redaction_mode_label(bundle.redaction.mode) != row.redaction_mode
        || bundle.redaction.rules_version != row.redaction_rules_version;
    if mismatch {
        return Err(CloneError::CloudPublishAiRestoreFailed {
            domain: source.clone_domain.clone(),
            target: site_target_label(source, &restore_plan.site),
            message: format!(
                "AI bundle {} does not match D1 version row",
                row.ai_version_id
            ),
        });
    }
    let bundle_bytes = serde_json::to_vec(bundle).map_err(|source_error| {
        CloneError::CloudPublishAiRestoreFailed {
            domain: source.clone_domain.clone(),
            target: site_target_label(source, &restore_plan.site),
            message: format!(
                "failed to verify AI bundle {} checksum: {source_error}",
                row.ai_version_id
            ),
        }
    })?;
    let actual_sha256 = sha256_hex(&bundle_bytes);
    if actual_sha256 != row.bundle_sha256 {
        return Err(CloneError::CloudPublishAiRestoreFailed {
            domain: source.clone_domain.clone(),
            target: site_target_label(source, &restore_plan.site),
            message: format!(
                "AI bundle {} checksum does not match D1 version row",
                row.ai_version_id
            ),
        });
    }
    Ok(())
}

fn validate_cloud_publish_ai_object_row(
    source: &CloudPublishSource,
    restore_plan: &CloudPublishRestorePlan,
    row: &PublishAiObjectRow,
    object: &PublishAiObject,
) -> Result<(), CloneError> {
    let object_schema_version = i64::from(object.schema_version);
    let mismatch = object.site_id != row.site_id
        || object.revision_oid != row.revision_oid
        || object.object_type != row.object_type
        || object.object_id != row.object_id
        || cloud_publish_ai_layer_label(object.layer) != row.layer
        || cloud_publish_ai_redaction_mode_label(object.redaction.mode) != row.redaction_mode
        || object_schema_version != row.schema_version;
    if mismatch {
        return Err(CloneError::CloudPublishAiRestoreFailed {
            domain: source.clone_domain.clone(),
            target: site_target_label(source, &restore_plan.site),
            message: format!(
                "AI object row {}/{} does not match R2 object envelope",
                row.object_type, row.object_id
            ),
        });
    }
    let object_bytes = serde_json::to_vec(object).map_err(|source_error| {
        CloneError::CloudPublishAiRestoreFailed {
            domain: source.clone_domain.clone(),
            target: site_target_label(source, &restore_plan.site),
            message: format!(
                "failed to verify AI object {}/{} checksum: {source_error}",
                row.object_type, row.object_id
            ),
        }
    })?;
    let actual_sha256 = sha256_hex(&object_bytes);
    if actual_sha256 != row.payload_sha256 {
        return Err(CloneError::CloudPublishAiRestoreFailed {
            domain: source.clone_domain.clone(),
            target: site_target_label(source, &restore_plan.site),
            message: format!(
                "AI object row {}/{} payload checksum does not match R2 object",
                row.object_type, row.object_id
            ),
        });
    }
    Ok(())
}

fn cloud_publish_ai_layer_label(layer: AiObjectLayer) -> &'static str {
    match layer {
        AiObjectLayer::Snapshot => "snapshot",
        AiObjectLayer::Event => "event",
        AiObjectLayer::Projection => "projection",
    }
}

fn cloud_publish_ai_redaction_mode_label(mode: RedactionMode) -> &'static str {
    match mode {
        RedactionMode::Default => "default",
        RedactionMode::Strict => "strict",
    }
}

fn cloud_publish_ai_history_type(object_type: &str) -> String {
    let mut out = String::from("publish_ai");
    for ch in object_type.chars() {
        if ch.is_ascii_uppercase() {
            out.push('_');
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    out
}

async fn configure_cloud_publish_checkout(
    source: &CloudPublishSource,
    restore_plan: &CloudPublishRestorePlan,
    remote_url: &str,
) -> Result<(), CloneError> {
    let selected_commit = object_hash_from_cloud_index(
        source,
        &restore_plan.site,
        &restore_plan.checkout.revision_oid,
    )?;

    let db = get_db_conn_instance().await;
    if let Some(branch_name) = cloud_checkout_branch_name(&restore_plan.checkout) {
        Branch::update_branch_with_conn(&db, &branch_name, &selected_commit.to_string(), None)
            .await
            .map_err(|error| CloneError::CloudPublishCheckoutSetupFailed {
                domain: source.clone_domain.clone(),
                target: site_target_label(source, &restore_plan.site),
                message: format!("failed to update branch '{branch_name}': {error}"),
            })?;
        Head::update_result_with_conn(&db, Head::Branch(branch_name.clone()), None)
            .await
            .map_err(|error| CloneError::CloudPublishCheckoutSetupFailed {
                domain: source.clone_domain.clone(),
                target: site_target_label(source, &restore_plan.site),
                message: format!("failed to update HEAD to branch '{branch_name}': {error}"),
            })?;

        let merge_ref = format!("refs/heads/{branch_name}");
        let _ = ConfigKv::set(&format!("branch.{branch_name}.merge"), &merge_ref, false).await;
        let _ = ConfigKv::set(&format!("branch.{branch_name}.remote"), "origin", false).await;
    } else {
        Head::update_result_with_conn(&db, Head::Detached(selected_commit), None)
            .await
            .map_err(|error| CloneError::CloudPublishCheckoutSetupFailed {
                domain: source.clone_domain.clone(),
                target: site_target_label(source, &restore_plan.site),
                message: format!("failed to detach HEAD at selected revision: {error}"),
            })?;
    }

    let _ = ConfigKv::set("remote.origin.url", remote_url, false).await;
    let _ = ConfigKv::set("remote.origin.type", "libra+cloud", false).await;
    let _ = ConfigKv::set("cloud.origin.clone_domain", &source.clone_domain, false).await;
    let _ = ConfigKv::set("cloud.origin.site_id", &restore_plan.site.site_id, false).await;
    let _ = ConfigKv::set("cloud.origin.repo_id", &restore_plan.site.repo_id, false).await;
    let _ = ConfigKv::set(
        "cloud.origin.repository_name",
        &restore_plan.repository.name,
        false,
    )
    .await;
    let _ = ConfigKv::set("cloud.origin.slug", &restore_plan.site.slug, false).await;
    let _ = ConfigKv::set(
        "cloud.origin.revision_status",
        &restore_plan.revision.status,
        false,
    )
    .await;
    let _ = ConfigKv::set(
        "cloud.origin.revision_oid",
        &restore_plan.checkout.revision_oid,
        false,
    )
    .await;

    Ok(())
}

fn parse_cloud_publish_source(input: &str) -> Result<CloudPublishSource, CloneError> {
    let url = Url::parse(input).map_err(|source| CloneError::InvalidCloudPublishSource {
        input: input.to_string(),
        reason: format!("URL parse failed: {source}"),
    })?;
    if url.scheme() != "libra+cloud" {
        return Err(invalid_cloud_source(input, "scheme must be libra+cloud"));
    }

    let clone_domain = url
        .host_str()
        .ok_or_else(|| invalid_cloud_source(input, "clone domain is required"))?;
    validate_cloud_clone_domain(input, clone_domain)?;
    let clone_domain = clone_domain.to_ascii_lowercase();

    let segments = url
        .path_segments()
        .map(|segments| segments.collect::<Vec<_>>())
        .unwrap_or_default();
    let target = match segments.as_slice() {
        [] | [""] => return Err(invalid_cloud_source(input, "slug or repo_id is required")),
        [slug] => {
            validate_cloud_slug(input, slug, "slug")?;
            CloudPublishTarget::Slug((*slug).to_string())
        }
        ["repo", repo_id] => {
            validate_cloud_slug(input, repo_id, "repo_id")?;
            CloudPublishTarget::RepoId((*repo_id).to_string())
        }
        _ => {
            return Err(invalid_cloud_source(
                input,
                "path must be /<slug> or /repo/<repo_id>",
            ));
        }
    };

    let mut ref_selector: Option<String> = None;
    let mut revision_selector: Option<String> = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "ref" => {
                if ref_selector.replace(value.into_owned()).is_some() {
                    return Err(invalid_cloud_source(
                        input,
                        "ref selector appears more than once",
                    ));
                }
            }
            "revision" => {
                if revision_selector.replace(value.into_owned()).is_some() {
                    return Err(invalid_cloud_source(
                        input,
                        "revision selector appears more than once",
                    ));
                }
            }
            other => {
                return Err(invalid_cloud_source(
                    input,
                    &format!("unsupported query parameter '{other}'"),
                ));
            }
        }
    }
    if ref_selector.is_some() && revision_selector.is_some() {
        return Err(invalid_cloud_source(
            input,
            "ref and revision selectors are mutually exclusive",
        ));
    }
    let selector = if let Some(selector) = ref_selector {
        validate_cloud_ref_selector(input, &selector)?;
        Some(CloudPublishSelector::Ref(selector))
    } else if let Some(selector) = revision_selector {
        validate_cloud_revision_selector(input, &selector)?;
        Some(CloudPublishSelector::Revision(selector))
    } else {
        None
    };

    Ok(CloudPublishSource {
        clone_domain,
        target,
        selector,
    })
}

fn validate_cloud_clone_option_compatibility(args: &CloneArgs) -> Result<(), CloneError> {
    if args.branch.is_some() {
        return Err(CloneError::UnsupportedCloudCloneOption {
            option: "--branch",
            reason: "cloud source refs are selected in the source URL, not with Git branch flags",
            hint: "use `?ref=<branch|tag|full-ref>` on the libra+cloud:// URL instead of `--branch`.",
        });
    }
    if args.depth.is_some() {
        return Err(CloneError::UnsupportedCloudCloneOption {
            option: "--depth",
            reason: "Cloudflare restore must download the complete published object set",
            hint: "`--depth` is only supported for Git remotes; omit it for libra+cloud:// sources.",
        });
    }
    if args.single_branch {
        return Err(CloneError::UnsupportedCloudCloneOption {
            option: "--single-branch",
            reason: "Cloudflare restore must preserve all published refs in the local repository",
            hint: "`--single-branch` is only supported for Git remotes; use `?ref=<branch|tag|full-ref>` to select the checkout target.",
        });
    }
    if args.mirror {
        return Err(CloneError::UnsupportedCloudCloneOption {
            option: "--mirror",
            reason: "Cloudflare restore targets a non-bare working repository and cannot mirror refs verbatim",
            hint: "`--mirror` is only supported for Git remotes; clone a libra+cloud:// source without it.",
        });
    }
    if args.bare {
        return Err(CloneError::UnsupportedCloudCloneOption {
            option: "--bare",
            reason: "Cloudflare restore currently targets a non-bare working repository",
            hint: "`--bare` is only supported for Git remotes until libra+cloud:// restore grows bare-repository support.",
        });
    }
    if args.filter.is_some() {
        return Err(CloneError::UnsupportedCloudCloneOption {
            option: "--filter",
            reason: "Cloudflare restore must download the complete published object set",
            hint: "`--filter` (partial clone) is not supported for libra+cloud:// sources; omit it.",
        });
    }
    if !args.deps_of.is_empty() {
        return Err(CloneError::UnsupportedCloudCloneOption {
            option: "--deps-of",
            reason: "Cloudflare restore must download the complete published object set and does not carry the dependency graph (refs/notes/deps)",
            hint: "`--deps-of` (dependency-filtered clone) is only supported for a local Libra source; omit it for libra+cloud:// sources.",
        });
    }
    if args.shallow_since.is_some() {
        return Err(CloneError::UnsupportedCloudCloneOption {
            option: "--shallow-since",
            reason: "Cloudflare restore must download the complete published object set",
            hint: "`--shallow-since` is not supported for libra+cloud:// sources; omit it.",
        });
    }
    if !args.shallow_exclude.is_empty() {
        return Err(CloneError::UnsupportedCloudCloneOption {
            option: "--shallow-exclude",
            reason: "Cloudflare restore must download the complete published object set",
            hint: "`--shallow-exclude` is not supported for libra+cloud:// sources; omit it.",
        });
    }
    Ok(())
}

async fn load_cloud_clone_domain_config(
    source: &CloudPublishSource,
) -> Result<CloudCloneDomainConfig, CloneError> {
    let required_suffixes = ["account_id", "d1_database_id", "r2_bucket"];
    let mut missing_keys = Vec::new();
    let local_target = clone_config_local_target();
    let mut account_id = None;
    let mut d1_database_id = None;
    let mut r2_bucket = None;

    for suffix in required_suffixes {
        let key = format!("cloud.clone_domains.{}.{}", source.clone_domain, suffix);
        match read_cascaded_config_value_decrypted(local_target, &key).await {
            Ok(Some(value)) => match suffix {
                "account_id" => account_id = Some(value),
                "d1_database_id" => d1_database_id = Some(value),
                "r2_bucket" => r2_bucket = Some(value),
                _ => {}
            },
            Ok(None) => missing_keys.push(key),
            Err(source_error) => {
                return Err(CloneError::CloudCloneDomainConfigRead {
                    domain: source.clone_domain.clone(),
                    source: source_error,
                });
            }
        }
    }

    if !missing_keys.is_empty() {
        return Err(CloneError::CloudCloneDomainNotConfigured {
            domain: source.clone_domain.clone(),
            missing_keys: missing_keys.join(", "),
        });
    }

    let credential_profile_key = format!(
        "cloud.clone_domains.{}.credential_profile",
        source.clone_domain
    );
    let credential_profile =
        read_cascaded_config_value_decrypted(local_target, &credential_profile_key)
            .await
            .map_err(|source_error| CloneError::CloudCloneDomainConfigRead {
                domain: source.clone_domain.clone(),
                source: source_error,
            })?;

    Ok(CloudCloneDomainConfig {
        account_id: account_id.ok_or_else(|| CloneError::CloudCloneDomainNotConfigured {
            domain: source.clone_domain.clone(),
            missing_keys: format!("cloud.clone_domains.{}.account_id", source.clone_domain),
        })?,
        api_token: resolve_env_for_target("LIBRA_D1_API_TOKEN", local_target)
            .await
            .map_err(|source_error| CloneError::CloudCloneDomainConfigRead {
                domain: source.clone_domain.clone(),
                source: source_error,
            })?
            .filter(|value| !value.is_empty())
            .ok_or_else(|| CloneError::CloudCloneD1ApiTokenNotConfigured {
                domain: source.clone_domain.clone(),
            })?,
        d1_database_id: d1_database_id.ok_or_else(|| {
            CloneError::CloudCloneDomainNotConfigured {
                domain: source.clone_domain.clone(),
                missing_keys: format!("cloud.clone_domains.{}.d1_database_id", source.clone_domain),
            }
        })?,
        r2_bucket: r2_bucket.ok_or_else(|| CloneError::CloudCloneDomainNotConfigured {
            domain: source.clone_domain.clone(),
            missing_keys: format!("cloud.clone_domains.{}.r2_bucket", source.clone_domain),
        })?,
        credential_profile,
    })
}

async fn resolve_cloud_publish_restore_plan(
    source: &CloudPublishSource,
    config: &CloudCloneDomainConfig,
) -> Result<CloudPublishRestorePlan, CloneError> {
    let d1_client = create_cloud_clone_d1_client(source, config)?;
    let site = resolve_cloud_publish_site(source, &d1_client).await?;
    let target = source.target_label();

    let repository = d1_client
        .find_repository(&site.repo_id)
        .await
        .map_err(
            |source_error| CloneError::CloudPublishMetadataLookupFailed {
                domain: source.clone_domain.clone(),
                site_id: site.site_id.clone(),
                operation: "repositories lookup",
                code: source_error.code,
                message: source_error.message,
            },
        )?
        .ok_or_else(|| CloneError::CloudPublishRepositoryNotFound {
            domain: source.clone_domain.clone(),
            target: target.clone(),
            repo_id: site.repo_id.clone(),
        })?;

    let refs = d1_client
        .list_publish_refs(&site.site_id)
        .await
        .map_err(
            |source_error| CloneError::CloudPublishMetadataLookupFailed {
                domain: source.clone_domain.clone(),
                site_id: site.site_id.clone(),
                operation: "publish_refs lookup",
                code: source_error.code,
                message: source_error.message,
            },
        )?;
    let checkout = resolve_cloud_publish_checkout_target(source, &site, &refs)?;

    let revision = d1_client
        .find_published_revision(&site.site_id, &checkout.revision_oid)
        .await
        .map_err(
            |source_error| CloneError::CloudPublishMetadataLookupFailed {
                domain: source.clone_domain.clone(),
                site_id: site.site_id.clone(),
                operation: "publish_revisions lookup",
                code: source_error.code,
                message: source_error.message,
            },
        )?
        .ok_or_else(|| CloneError::CloudPublishRevisionNotFound {
            domain: source.clone_domain.clone(),
            target: target.clone(),
            revision_oid: checkout.revision_oid.clone(),
        })?;

    let ai_objects = if revision.ai_object_count > 0 {
        d1_client
            .list_publish_ai_objects(&site.site_id, &revision.revision_oid)
            .await
            .map_err(
                |source_error| CloneError::CloudPublishMetadataLookupFailed {
                    domain: source.clone_domain.clone(),
                    site_id: site.site_id.clone(),
                    operation: "publish_ai_objects lookup",
                    code: source_error.code,
                    message: source_error.message,
                },
            )?
    } else {
        Vec::new()
    };
    let ai_versions = if revision.ai_bundle_count > 0 || revision.ai_object_count > 0 {
        d1_client
            .list_publish_ai_versions(&site.site_id, &revision.revision_oid)
            .await
            .map_err(
                |source_error| CloneError::CloudPublishMetadataLookupFailed {
                    domain: source.clone_domain.clone(),
                    site_id: site.site_id.clone(),
                    operation: "publish_ai_versions lookup",
                    code: source_error.code,
                    message: source_error.message,
                },
            )?
    } else {
        Vec::new()
    };

    let object_indexes =
        d1_client
            .get_object_indexes(&site.repo_id)
            .await
            .map_err(
                |source_error| CloneError::CloudPublishMetadataLookupFailed {
                    domain: source.clone_domain.clone(),
                    site_id: site.site_id.clone(),
                    operation: "object_index lookup",
                    code: source_error.code,
                    message: source_error.message,
                },
            )?;
    if object_indexes.is_empty() {
        return Err(CloneError::CloudPublishObjectIndexMissing {
            domain: source.clone_domain.clone(),
            target: target.clone(),
            repo_id: site.repo_id.clone(),
        });
    }
    let object_probe = create_cloud_clone_object_probe(source, config, &site.repo_id).await?;
    validate_cloud_publish_objects_available(source, &site, &object_indexes, object_probe.as_ref())
        .await?;

    Ok(CloudPublishRestorePlan {
        site,
        repository,
        checkout,
        revision,
        object_indexes,
        ai_objects,
        ai_versions,
    })
}

fn create_cloud_clone_d1_client(
    source: &CloudPublishSource,
    config: &CloudCloneDomainConfig,
) -> Result<D1Client, CloneError> {
    match env::var(CLOUD_CLONE_D1_API_BASE_URL_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        Some(api_base_url) => D1Client::new_with_api_base_url(
            config.account_id.clone(),
            config.api_token.clone(),
            config.d1_database_id.clone(),
            &api_base_url,
        )
        .map_err(|source_error| CloneError::CloudCloneD1ApiBaseUrlInvalid {
            domain: source.clone_domain.clone(),
            message: source_error.message,
        }),
        None => Ok(D1Client::new(
            config.account_id.clone(),
            config.api_token.clone(),
            config.d1_database_id.clone(),
        )),
    }
}

#[async_trait]
trait CloudCloneObjectProbe {
    async fn exists(&self, hash: &ObjectHash) -> Result<bool, CloneError>;
}

struct RemoteStorageObjectProbe {
    storage: RemoteStorage,
}

#[async_trait]
impl CloudCloneObjectProbe for RemoteStorageObjectProbe {
    async fn exists(&self, hash: &ObjectHash) -> Result<bool, CloneError> {
        Ok(self.storage.exist(hash).await)
    }
}

async fn create_cloud_clone_object_probe(
    source: &CloudPublishSource,
    config: &CloudCloneDomainConfig,
    repo_id: &str,
) -> Result<Box<dyn CloudCloneObjectProbe + Send + Sync>, CloneError> {
    let storage = create_cloud_clone_remote_storage(source, config, repo_id).await?;
    Ok(Box::new(RemoteStorageObjectProbe { storage }))
}

async fn create_cloud_clone_remote_storage(
    source: &CloudPublishSource,
    config: &CloudCloneDomainConfig,
    repo_id: &str,
) -> Result<RemoteStorage, CloneError> {
    if env::var_os(LIBRA_TEST_ENV).is_some()
        && let Some(root) = env::var_os(CLOUD_CLONE_TEST_R2_ROOT_ENV)
    {
        let store =
            LocalFileSystem::new_with_prefix(PathBuf::from(root)).map_err(|source_error| {
                CloneError::CloudCloneR2ClientBuildFailed {
                    domain: source.clone_domain.clone(),
                    message: source_error.to_string(),
                }
            })?;
        return Ok(RemoteStorage::new_with_prefix(
            Arc::new(store),
            repo_id.to_string(),
        ));
    }

    let endpoint = resolve_required_cloud_clone_r2_env(source, "LIBRA_STORAGE_ENDPOINT").await?;
    let access_key =
        resolve_required_cloud_clone_r2_env(source, "LIBRA_STORAGE_ACCESS_KEY").await?;
    let secret_key =
        resolve_required_cloud_clone_r2_env(source, "LIBRA_STORAGE_SECRET_KEY").await?;
    let region = resolve_optional_cloud_clone_r2_env(source, "LIBRA_STORAGE_REGION")
        .await?
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "auto".to_string());

    let storage = AmazonS3Builder::new()
        .with_bucket_name(&config.r2_bucket)
        .with_region(&region)
        .with_endpoint(&endpoint)
        .with_access_key_id(&access_key)
        .with_secret_access_key(&secret_key)
        .with_virtual_hosted_style_request(false)
        .build()
        .map_err(|source_error| CloneError::CloudCloneR2ClientBuildFailed {
            domain: source.clone_domain.clone(),
            message: source_error.to_string(),
        })?;

    Ok(RemoteStorage::new_with_prefix(
        Arc::new(storage),
        repo_id.to_string(),
    ))
}

async fn resolve_required_cloud_clone_r2_env(
    source: &CloudPublishSource,
    name: &'static str,
) -> Result<String, CloneError> {
    resolve_optional_cloud_clone_r2_env(source, name)
        .await?
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CloneError::CloudCloneR2CredentialsNotConfigured {
            domain: source.clone_domain.clone(),
            missing_keys: name.to_string(),
        })
}

async fn resolve_optional_cloud_clone_r2_env(
    source: &CloudPublishSource,
    name: &'static str,
) -> Result<Option<String>, CloneError> {
    resolve_env_for_target(name, clone_config_local_target())
        .await
        .map_err(|source_error| CloneError::CloudCloneR2ConfigRead {
            domain: source.clone_domain.clone(),
            source: source_error,
        })
}

async fn validate_cloud_publish_objects_available(
    source: &CloudPublishSource,
    site: &PublishSiteRow,
    object_indexes: &[ObjectIndexRow],
    object_probe: &(dyn CloudCloneObjectProbe + Send + Sync),
) -> Result<(), CloneError> {
    for object in object_indexes {
        let hash = object_hash_from_cloud_index(source, site, &object.o_id)?;
        if !object_probe.exists(&hash).await? {
            return Err(CloneError::CloudPublishObjectMissing {
                domain: source.clone_domain.clone(),
                target: source.target_label(),
                object_oid: object.o_id.clone(),
            });
        }
    }
    Ok(())
}

fn object_hash_from_cloud_index(
    source: &CloudPublishSource,
    site: &PublishSiteRow,
    object_oid: &str,
) -> Result<ObjectHash, CloneError> {
    let bytes = hex::decode(object_oid).map_err(|source_error| {
        CloneError::CloudPublishObjectIndexInvalid {
            domain: source.clone_domain.clone(),
            target: site_target_label(source, site),
            object_oid: object_oid.to_string(),
            reason: source_error.to_string(),
        }
    })?;
    ObjectHash::from_bytes(&bytes).map_err(|source_error| {
        CloneError::CloudPublishObjectIndexInvalid {
            domain: source.clone_domain.clone(),
            target: site_target_label(source, site),
            object_oid: object_oid.to_string(),
            reason: source_error.to_string(),
        }
    })
}

fn site_target_label(source: &CloudPublishSource, site: &PublishSiteRow) -> String {
    match &source.target {
        CloudPublishTarget::Slug(_) => format!("slug:{}", site.slug),
        CloudPublishTarget::RepoId(_) => format!("repo:{}", site.repo_id),
    }
}

async fn resolve_cloud_publish_site(
    source: &CloudPublishSource,
    d1_client: &D1Client,
) -> Result<PublishSiteRow, CloneError> {
    let target = source.target_label();
    let result = match &source.target {
        CloudPublishTarget::Slug(slug) => {
            d1_client
                .find_publish_site_by_clone_slug(&source.clone_domain, slug)
                .await
        }
        CloudPublishTarget::RepoId(repo_id) => {
            d1_client
                .find_publish_site_by_clone_repo_id(&source.clone_domain, repo_id)
                .await
        }
    }
    .map_err(|source_error| CloneError::CloudPublishSiteLookupFailed {
        domain: source.clone_domain.clone(),
        target: target.clone(),
        code: source_error.code,
        message: source_error.message,
    })?;

    let site = result.ok_or_else(|| CloneError::CloudPublishSiteNotFound {
        domain: source.clone_domain.clone(),
        target: target.clone(),
    })?;
    if site.status != "active" {
        return Err(CloneError::CloudPublishSiteUnavailable {
            domain: source.clone_domain.clone(),
            target,
            status: site.status,
        });
    }
    Ok(site)
}

fn resolve_cloud_publish_checkout_target(
    source: &CloudPublishSource,
    site: &PublishSiteRow,
    refs: &[PublishRefRow],
) -> Result<CloudPublishCheckoutTarget, CloneError> {
    if refs.is_empty() {
        return Err(CloneError::CloudPublishRefsMissing {
            domain: source.clone_domain.clone(),
            target: source.target_label(),
        });
    }

    match source.selector.as_ref() {
        None => {
            let default_ref = site.default_ref.as_ref().ok_or_else(|| {
                CloneError::CloudPublishDefaultRefMissing {
                    domain: source.clone_domain.clone(),
                    target: source.target_label(),
                }
            })?;
            let row = refs
                .iter()
                .find(|row| row.ref_name == *default_ref)
                .ok_or_else(|| CloneError::CloudPublishRefNotFound {
                    domain: source.clone_domain.clone(),
                    target: source.target_label(),
                    selector: default_ref.clone(),
                })?;
            Ok(CloudPublishCheckoutTarget {
                revision_oid: row.revision_oid.clone(),
                ref_name: Some(row.ref_name.clone()),
                selector_kind: CloudPublishCheckoutSelectorKind::DefaultRef,
            })
        }
        Some(CloudPublishSelector::Ref(selector)) => {
            let matches = matching_cloud_publish_refs(refs, selector);
            match matches.as_slice() {
                [] => Err(CloneError::CloudPublishRefNotFound {
                    domain: source.clone_domain.clone(),
                    target: source.target_label(),
                    selector: selector.clone(),
                }),
                [row] => Ok(CloudPublishCheckoutTarget {
                    revision_oid: row.revision_oid.clone(),
                    ref_name: Some(row.ref_name.clone()),
                    selector_kind: CloudPublishCheckoutSelectorKind::Ref,
                }),
                rows => {
                    let mut names = rows
                        .iter()
                        .map(|row| row.ref_name.clone())
                        .collect::<Vec<_>>();
                    names.sort();
                    Err(CloneError::CloudPublishRefAmbiguous {
                        domain: source.clone_domain.clone(),
                        target: source.target_label(),
                        selector: selector.clone(),
                        matches: names.join(", "),
                    })
                }
            }
        }
        Some(CloudPublishSelector::Revision(selector)) if selector == "latest" => {
            let revision_oid = site.latest_revision_oid.clone().ok_or_else(|| {
                CloneError::CloudPublishLatestRevisionMissing {
                    domain: source.clone_domain.clone(),
                    target: source.target_label(),
                }
            })?;
            Ok(CloudPublishCheckoutTarget {
                revision_oid,
                ref_name: None,
                selector_kind: CloudPublishCheckoutSelectorKind::LatestRevision,
            })
        }
        Some(CloudPublishSelector::Revision(selector)) => Ok(CloudPublishCheckoutTarget {
            revision_oid: selector.clone(),
            ref_name: None,
            selector_kind: CloudPublishCheckoutSelectorKind::Revision,
        }),
    }
}

fn matching_cloud_publish_refs<'a>(
    refs: &'a [PublishRefRow],
    selector: &str,
) -> Vec<&'a PublishRefRow> {
    let exact = refs
        .iter()
        .filter(|row| row.ref_name == selector)
        .collect::<Vec<_>>();
    if !exact.is_empty() {
        return exact;
    }
    refs.iter()
        .filter(|row| row.short_name == selector)
        .collect::<Vec<_>>()
}

fn clone_config_local_target() -> LocalIdentityTarget<'static> {
    if util::try_get_storage_path(None).is_ok() {
        LocalIdentityTarget::CurrentRepo
    } else {
        LocalIdentityTarget::None
    }
}

fn invalid_cloud_source(input: &str, reason: &str) -> CloneError {
    CloneError::InvalidCloudPublishSource {
        input: input.to_string(),
        reason: reason.to_string(),
    }
}

fn validate_cloud_clone_domain(input: &str, domain: &str) -> Result<(), CloneError> {
    if domain.is_empty() || domain.len() > 253 {
        return Err(invalid_cloud_source(input, "clone domain is invalid"));
    }
    for label in domain.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        {
            return Err(invalid_cloud_source(input, "clone domain is invalid"));
        }
    }
    Ok(())
}

fn validate_cloud_slug(input: &str, value: &str, label: &str) -> Result<(), CloneError> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.len() > 128
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(invalid_cloud_source(input, &format!("{label} is invalid")));
    }
    Ok(())
}

fn validate_cloud_ref_selector(input: &str, value: &str) -> Result<(), CloneError> {
    if value.is_empty()
        || value.trim() != value
        || value.contains("..")
        || value.starts_with('/')
        || value.ends_with('/')
        || value
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '?' | '#'))
    {
        return Err(invalid_cloud_source(input, "ref selector is invalid"));
    }
    Ok(())
}

fn validate_cloud_revision_selector(input: &str, value: &str) -> Result<(), CloneError> {
    if value == "latest" {
        return Ok(());
    }
    if value.len() < 4
        || value.len() > 64
        || !value
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, 'a'..='f'))
    {
        return Err(invalid_cloud_source(input, "revision selector is invalid"));
    }
    Ok(())
}

/// Convert a repo-relative closure path into a gitignore-syntax include pattern
/// that matches ONLY that exact path: anchor it with a leading `/` (so a
/// top-level `README` does not also scope `sub/README`) and escape the gitignore
/// glob metacharacters `\ * ? [ ]` so a file literally named e.g. `a[1].txt` is
/// matched verbatim by the sparse VIEW rather than parsed as a character class.
/// A leading `#`/`!` needs no escaping because the leading `/` means the pattern
/// never begins with one.
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
    if let Err(e) = crate::internal::sparse::SparseViewStore::replace(&patterns).await {
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
                    Head::update_with_conn(txn, Head::Branch(branch_name.to_owned()), None).await;

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
#[cfg(test)]
mod tests {
    use std::fs;

    use git_internal::internal::object::{
        ObjectTrait,
        blob::Blob,
        commit::Commit,
        tree::{Tree, TreeItem, TreeItemMode},
    };
    use object_store::memory::InMemory;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        internal::model::reference,
        utils::test::{ChangeDirGuard, ScopedEnvVar},
    };

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
    fn cloud_clone_source_parse_test_accepts_slug_repo_and_selectors() {
        for (input, expected) in [
            (
                "libra+cloud://code.example.com/kepler-ledger",
                CloudPublishSource {
                    clone_domain: "code.example.com".to_string(),
                    target: CloudPublishTarget::Slug("kepler-ledger".to_string()),
                    selector: None,
                },
            ),
            (
                "libra+cloud://code.example.com/repo/rp_8f4c1b",
                CloudPublishSource {
                    clone_domain: "code.example.com".to_string(),
                    target: CloudPublishTarget::RepoId("rp_8f4c1b".to_string()),
                    selector: None,
                },
            ),
            (
                "libra+cloud://code.example.com/kepler-ledger?ref=refs/tags/v1.0.0",
                CloudPublishSource {
                    clone_domain: "code.example.com".to_string(),
                    target: CloudPublishTarget::Slug("kepler-ledger".to_string()),
                    selector: Some(CloudPublishSelector::Ref("refs/tags/v1.0.0".to_string())),
                },
            ),
            (
                "libra+cloud://code.example.com/kepler-ledger?ref=feature/branch",
                CloudPublishSource {
                    clone_domain: "code.example.com".to_string(),
                    target: CloudPublishTarget::Slug("kepler-ledger".to_string()),
                    selector: Some(CloudPublishSelector::Ref("feature/branch".to_string())),
                },
            ),
            (
                "libra+cloud://code.example.com/kepler-ledger?revision=latest",
                CloudPublishSource {
                    clone_domain: "code.example.com".to_string(),
                    target: CloudPublishTarget::Slug("kepler-ledger".to_string()),
                    selector: Some(CloudPublishSelector::Revision("latest".to_string())),
                },
            ),
            (
                "libra+cloud://CODE.EXAMPLE.COM/kepler-ledger?revision=9a1f3e2c",
                CloudPublishSource {
                    clone_domain: "code.example.com".to_string(),
                    target: CloudPublishTarget::Slug("kepler-ledger".to_string()),
                    selector: Some(CloudPublishSelector::Revision("9a1f3e2c".to_string())),
                },
            ),
        ] {
            let parsed = parse_cloud_publish_source(input).unwrap_or_else(|error| {
                panic!("{input} should parse as a valid cloud publish source: {error}")
            });
            assert_eq!(
                parsed, expected,
                "{input} should preserve restore selectors"
            );
        }
    }

    #[test]
    fn cloud_clone_source_parse_test_rejects_invalid_inputs() {
        for (input, needle) in [
            ("libra+cloud://bad_domain/repo", "clone domain"),
            ("libra+cloud://code.example.com/", "slug or repo_id"),
            ("libra+cloud://code.example.com/repo/", "repo_id"),
            ("libra+cloud://code.example.com/bad slug", "slug is invalid"),
            (
                "libra+cloud://code.example.com/slug?ref=main&revision=latest",
                "mutually exclusive",
            ),
            (
                "libra+cloud://code.example.com/slug?revision=ABCDEF",
                "revision selector",
            ),
            (
                "libra+cloud://code.example.com/slug?ref=../main",
                "ref selector",
            ),
            (
                "libra+cloud://code.example.com/slug?foo=bar",
                "unsupported query parameter",
            ),
        ] {
            let error =
                parse_cloud_publish_source(input).expect_err("invalid cloud source rejected");
            assert!(
                error.to_string().contains(needle),
                "{input} should mention {needle:?}, got {error}",
            );
            let cli: CliError = error.into();
            assert_eq!(cli.stable_code(), StableErrorCode::CliInvalidArguments);
        }
    }

    #[test]
    fn cloud_clone_restore_plan_resolves_default_ref_checkout_revision() {
        let source = CloudPublishSource {
            clone_domain: "code.example.com".to_string(),
            target: CloudPublishTarget::Slug("kepler-ledger".to_string()),
            selector: None,
        };
        let site = publish_site_row(
            Some("refs/heads/main"),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        );
        let refs = vec![
            publish_ref_row(
                "refs/heads/main",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
            publish_ref_row(
                "refs/tags/v1.0.0",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
        ];

        let checkout = resolve_cloud_publish_checkout_target(&source, &site, &refs)
            .expect("default ref should resolve to a checkout revision");

        assert_eq!(
            checkout,
            CloudPublishCheckoutTarget {
                revision_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                ref_name: Some("refs/heads/main".to_string()),
                selector_kind: CloudPublishCheckoutSelectorKind::DefaultRef,
            }
        );
    }

    #[test]
    fn cloud_clone_restore_plan_resolves_full_tag_ref_selector() {
        let source = CloudPublishSource {
            clone_domain: "code.example.com".to_string(),
            target: CloudPublishTarget::Slug("kepler-ledger".to_string()),
            selector: Some(CloudPublishSelector::Ref("refs/tags/v1.0.0".to_string())),
        };
        let site = publish_site_row(
            Some("refs/heads/main"),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        );
        let refs = vec![
            publish_ref_row(
                "refs/heads/main",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
            publish_ref_row(
                "refs/tags/v1.0.0",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
        ];

        let checkout = resolve_cloud_publish_checkout_target(&source, &site, &refs)
            .expect("full tag ref selector should resolve to the tag revision");

        assert_eq!(
            checkout,
            CloudPublishCheckoutTarget {
                revision_oid: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
                ref_name: Some("refs/tags/v1.0.0".to_string()),
                selector_kind: CloudPublishCheckoutSelectorKind::Ref,
            }
        );
    }

    #[test]
    fn cloud_clone_restore_plan_rejects_ambiguous_short_ref_selector() {
        let source = CloudPublishSource {
            clone_domain: "code.example.com".to_string(),
            target: CloudPublishTarget::Slug("kepler-ledger".to_string()),
            selector: Some(CloudPublishSelector::Ref("release".to_string())),
        };
        let site = publish_site_row(
            Some("refs/heads/main"),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        );
        let refs = vec![
            publish_ref_row(
                "refs/heads/release",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
            publish_ref_row(
                "refs/tags/release",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
        ];

        let error = resolve_cloud_publish_checkout_target(&source, &site, &refs)
            .expect_err("branch/tag short-name collision must require a full ref");

        match error {
            CloneError::CloudPublishRefAmbiguous {
                selector, matches, ..
            } => {
                assert_eq!(selector, "release");
                assert!(matches.contains("refs/heads/release"));
                assert!(matches.contains("refs/tags/release"));
            }
            other => panic!("expected ambiguous ref error, got {other}"),
        }
    }

    /// Helper for the cloud-clone-option-compatibility regression tests:
    /// build a minimal `CloneArgs` skeleton with a `libra+cloud://` remote
    /// and every flag at its non-cloud default. Each test then flips the
    /// single unsupported flag it cares about.
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

    fn cloud_clone_args_baseline() -> CloneArgs {
        CloneArgs {
            no_single_branch: false,
            origin: None,
            local: false,
            no_local: false,
            reject_shallow: false,
            reference: vec![],
            reference_if_able: vec![],
            shared: false,
            no_shared: false,
            dissociate: false,
            mirror: false,
            filter: None,
            shallow_since: None,
            shallow_exclude: vec![],
            deps_of: vec![],
            deps_depth_limit: None,
            no_checkout: false,
            no_progress: false,
            remote_repo: "libra+cloud://code.example.com/kepler-ledger".to_string(),
            local_path: None,
            branch: None,
            single_branch: false,
            bare: false,
            depth: None,
            tags: false,
            no_tags: false,
        }
    }

    /// Regression for [`docs/development/commands/clone.md`] §"第一批 Cloudflare clone
    /// 只保证完整 non-bare clone" — every Cloudflare-incompatible flag must
    /// surface `CloneError::UnsupportedCloudCloneOption` whose `option` field
    /// names the rejected flag, never silently fall back to a vanilla clone.
    /// The mapping from `UnsupportedCloudCloneOption` to a `CliError` carrying
    /// `StableErrorCode::CliInvalidArguments` is covered separately by
    /// `cloud_clone_unsupported_option_maps_to_cli_invalid_arguments`.
    #[test]
    fn validate_cloud_clone_option_compatibility_accepts_no_extra_flags() {
        let args = cloud_clone_args_baseline();
        validate_cloud_clone_option_compatibility(&args)
            .expect("baseline libra+cloud:// args without extra flags must pass compatibility");
    }

    #[test]
    fn validate_cloud_clone_option_compatibility_rejects_branch_flag() {
        let mut args = cloud_clone_args_baseline();
        args.branch = Some("main".to_string());
        match validate_cloud_clone_option_compatibility(&args)
            .expect_err("--branch must be rejected for libra+cloud:// sources")
        {
            CloneError::UnsupportedCloudCloneOption { option, hint, .. } => {
                assert_eq!(option, "--branch");
                assert!(
                    hint.contains("?ref="),
                    "branch hint should redirect to ?ref=: {hint}"
                );
            }
            other => panic!("expected UnsupportedCloudCloneOption, got {other:?}"),
        }
    }

    #[test]
    fn validate_cloud_clone_option_compatibility_rejects_depth_flag() {
        let mut args = cloud_clone_args_baseline();
        args.depth = Some(1);
        match validate_cloud_clone_option_compatibility(&args)
            .expect_err("--depth must be rejected for libra+cloud:// sources")
        {
            CloneError::UnsupportedCloudCloneOption { option, hint, .. } => {
                assert_eq!(option, "--depth");
                assert!(
                    hint.contains("--depth"),
                    "depth hint should name the flag: {hint}"
                );
            }
            other => panic!("expected UnsupportedCloudCloneOption, got {other:?}"),
        }
    }

    #[test]
    fn validate_cloud_clone_option_compatibility_rejects_single_branch_flag() {
        let mut args = cloud_clone_args_baseline();
        args.single_branch = true;
        match validate_cloud_clone_option_compatibility(&args)
            .expect_err("--single-branch must be rejected for libra+cloud:// sources")
        {
            CloneError::UnsupportedCloudCloneOption { option, hint, .. } => {
                assert_eq!(option, "--single-branch");
                assert!(
                    hint.contains("?ref="),
                    "single-branch hint should redirect to ?ref=: {hint}"
                );
            }
            other => panic!("expected UnsupportedCloudCloneOption, got {other:?}"),
        }
    }

    #[test]
    fn validate_cloud_clone_option_compatibility_rejects_bare_flag() {
        let mut args = cloud_clone_args_baseline();
        args.bare = true;
        match validate_cloud_clone_option_compatibility(&args)
            .expect_err("--bare must be rejected for libra+cloud:// sources")
        {
            CloneError::UnsupportedCloudCloneOption { option, hint, .. } => {
                assert_eq!(option, "--bare");
                assert!(
                    hint.contains("--bare"),
                    "bare hint should name the flag: {hint}"
                );
            }
            other => panic!("expected UnsupportedCloudCloneOption, got {other:?}"),
        }
    }

    #[test]
    fn validate_cloud_clone_option_compatibility_rejects_filter_flag() {
        let mut args = cloud_clone_args_baseline();
        args.filter = Some("blob:none".to_string());
        match validate_cloud_clone_option_compatibility(&args)
            .expect_err("--filter must be rejected for libra+cloud:// sources")
        {
            CloneError::UnsupportedCloudCloneOption { option, hint, .. } => {
                assert_eq!(option, "--filter");
                assert!(
                    hint.contains("--filter"),
                    "filter hint should name the flag: {hint}"
                );
            }
            other => panic!("expected UnsupportedCloudCloneOption, got {other:?}"),
        }
    }

    #[test]
    fn validate_cloud_clone_option_compatibility_rejects_shallow_since_flag() {
        let mut args = cloud_clone_args_baseline();
        args.shallow_since = Some("2020-01-01".to_string());
        match validate_cloud_clone_option_compatibility(&args)
            .expect_err("--shallow-since must be rejected for libra+cloud:// sources")
        {
            CloneError::UnsupportedCloudCloneOption { option, hint, .. } => {
                assert_eq!(option, "--shallow-since");
                assert!(
                    hint.contains("--shallow-since"),
                    "hint should name the flag: {hint}"
                );
            }
            other => panic!("expected UnsupportedCloudCloneOption, got {other:?}"),
        }
    }

    #[test]
    fn validate_cloud_clone_option_compatibility_rejects_shallow_exclude_flag() {
        let mut args = cloud_clone_args_baseline();
        args.shallow_exclude = vec!["v1".to_string()];
        match validate_cloud_clone_option_compatibility(&args)
            .expect_err("--shallow-exclude must be rejected for libra+cloud:// sources")
        {
            CloneError::UnsupportedCloudCloneOption { option, hint, .. } => {
                assert_eq!(option, "--shallow-exclude");
                assert!(
                    hint.contains("--shallow-exclude"),
                    "hint should name the flag: {hint}"
                );
            }
            other => panic!("expected UnsupportedCloudCloneOption, got {other:?}"),
        }
    }

    #[test]
    fn validate_cloud_clone_option_compatibility_rejects_mirror_flag() {
        let mut args = cloud_clone_args_baseline();
        args.mirror = true;
        match validate_cloud_clone_option_compatibility(&args)
            .expect_err("--mirror must be rejected for libra+cloud:// sources")
        {
            CloneError::UnsupportedCloudCloneOption { option, hint, .. } => {
                assert_eq!(option, "--mirror");
                assert!(
                    hint.contains("--mirror"),
                    "mirror hint should name the flag: {hint}"
                );
            }
            other => panic!("expected UnsupportedCloudCloneOption, got {other:?}"),
        }
    }

    /// Verifies the mapping from `UnsupportedCloudCloneOption` into the CLI
    /// error envelope: stable code must be `CliInvalidArguments`, exit code
    /// must be 129 (parameter error), and the structured `option` detail
    /// must round-trip the rejected flag name.
    #[test]
    fn cloud_clone_unsupported_option_maps_to_cli_invalid_arguments() {
        let cli: CliError = CloneError::UnsupportedCloudCloneOption {
            option: "--bare",
            reason: "Cloudflare restore currently targets a non-bare working repository",
            hint: "`--bare` is only supported for Git remotes until libra+cloud:// restore grows \
                   bare-repository support.",
        }
        .into();

        assert_eq!(cli.stable_code(), StableErrorCode::CliInvalidArguments);
        assert_eq!(cli.exit_code(), 129);
        assert_eq!(
            cli.details().get("option").and_then(|v| v.as_str()),
            Some("--bare")
        );
    }

    #[test]
    fn cloud_clone_restore_plan_resolves_revision_latest_from_site_row() {
        let source = CloudPublishSource {
            clone_domain: "code.example.com".to_string(),
            target: CloudPublishTarget::Slug("kepler-ledger".to_string()),
            selector: Some(CloudPublishSelector::Revision("latest".to_string())),
        };
        let site = publish_site_row(
            Some("refs/heads/main"),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        );
        let refs = vec![publish_ref_row(
            "refs/heads/main",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )];

        let checkout = resolve_cloud_publish_checkout_target(&source, &site, &refs)
            .expect("revision=latest should resolve from publish_sites.latest_revision_oid");

        assert_eq!(
            checkout,
            CloudPublishCheckoutTarget {
                revision_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                ref_name: None,
                selector_kind: CloudPublishCheckoutSelectorKind::LatestRevision,
            }
        );
    }

    #[tokio::test]
    async fn cloud_clone_restore_plan_validates_r2_object_availability() {
        let source = CloudPublishSource {
            clone_domain: "code.example.com".to_string(),
            target: CloudPublishTarget::Slug("kepler-ledger".to_string()),
            selector: None,
        };
        let site = publish_site_row(
            Some("refs/heads/main"),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        );
        let object_indexes = vec![object_index_row("1111111111111111111111111111111111111111")];
        let probe = FakeObjectProbe::default();

        validate_cloud_publish_objects_available(&source, &site, &object_indexes, &probe)
            .await
            .expect("all indexed objects should exist in R2");
    }

    #[tokio::test]
    #[serial]
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

    #[tokio::test]
    async fn cloud_clone_restore_plan_fails_when_r2_object_is_missing() {
        let source = CloudPublishSource {
            clone_domain: "code.example.com".to_string(),
            target: CloudPublishTarget::Slug("kepler-ledger".to_string()),
            selector: None,
        };
        let site = publish_site_row(
            Some("refs/heads/main"),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        );
        let object_oid = "1111111111111111111111111111111111111111";
        let object_indexes = vec![object_index_row(object_oid)];
        let probe = FakeObjectProbe {
            missing: std::collections::BTreeSet::from([object_oid.to_string()]),
        };

        let error =
            validate_cloud_publish_objects_available(&source, &site, &object_indexes, &probe)
                .await
                .expect_err("missing R2 objects must block cloud clone restore");

        match error {
            CloneError::CloudPublishObjectMissing {
                object_oid: missing,
                ..
            } => assert_eq!(missing, object_oid),
            other => panic!("expected missing object error, got {other}"),
        }
    }

    #[tokio::test]
    #[serial]
    async fn cloud_clone_restore_test_restores_default_ref_objects_refs_head_and_worktree() {
        let parent = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        let _cwd = ChangeDirGuard::new(parent.path());
        let source = cloud_source();
        let (restore_plan, remote, commit_id) = cloud_restore_fixture(true).await;
        let args = CloneArgs {
            no_single_branch: false,
            origin: None,
            local: false,
            no_local: false,
            reject_shallow: false,
            reference: vec![],
            reference_if_able: vec![],
            shared: false,
            no_shared: false,
            dissociate: false,
            mirror: false,
            filter: None,
            shallow_since: None,
            shallow_exclude: vec![],
            deps_of: vec![],
            deps_depth_limit: None,
            no_checkout: false,
            no_progress: false,
            remote_repo: "libra+cloud://code.example.com/kepler-ledger".to_string(),
            local_path: None,
            branch: None,
            single_branch: false,
            bare: false,
            depth: None,
            tags: false,
            no_tags: false,
        };
        let output = OutputConfig {
            quiet: true,
            ..OutputConfig::default()
        };

        let result = execute_cloud_publish_clone(
            &args,
            &source,
            restore_plan,
            remote,
            parent.path(),
            &output,
        )
        .await
        .expect("cloud clone restore should complete");

        let clone_dir = parent.path().join("kepler-ledger");
        assert_eq!(result.path, clone_dir.to_string_lossy());
        assert_eq!(result.remote_url, args.remote_repo);
        assert_eq!(result.branch.as_deref(), Some("main"));
        assert_eq!(result.source_kind.as_deref(), Some("cloudflare"));
        let cloud_site = result
            .cloud_site
            .as_ref()
            .expect("cloud clone output should include cloud site metadata");
        assert_eq!(cloud_site.clone_domain, "code.example.com");
        assert_eq!(cloud_site.site_id, "site_123");
        assert_eq!(cloud_site.slug, "kepler-ledger");
        assert_eq!(cloud_site.repo_id, "repo_456");
        assert_eq!(cloud_site.ref_name.as_deref(), Some("refs/heads/main"));
        assert_eq!(cloud_site.revision, commit_id.to_string());
        assert_eq!(
            fs::read_to_string(clone_dir.join("README.md")).unwrap(),
            "# cloud\n"
        );

        let _clone_cwd = ChangeDirGuard::new(&clone_dir);
        let head = Head::current_commit_result()
            .await
            .expect("restored HEAD should be readable")
            .expect("restored HEAD should point at a commit");
        assert_eq!(head.to_string(), commit_id.to_string());
        assert_eq!(
            config_value("remote.origin.url").await.as_deref(),
            Some("libra+cloud://code.example.com/kepler-ledger")
        );
        assert_eq!(
            config_value("cloud.origin.site_id").await.as_deref(),
            Some("site_123")
        );

        let db = get_db_conn_instance().await;
        let restored_tag = reference::Entity::find()
            .filter(reference::Column::Kind.eq(reference::ConfigKind::Tag))
            .filter(reference::Column::Name.eq("refs/tags/v1.0.0"))
            .filter(reference::Column::Remote.is_null())
            .one(&db)
            .await
            .expect("restored tag should be queryable")
            .expect("tag metadata should be restored");
        let expected_commit = commit_id.to_string();
        assert_eq!(
            restored_tag.commit.as_deref(),
            Some(expected_commit.as_str())
        );
    }

    #[tokio::test]
    #[serial]
    async fn cloud_clone_no_checkout_skips_worktree_but_restores_refs() {
        let parent = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        let _cwd = ChangeDirGuard::new(parent.path());
        let source = cloud_source();
        let (restore_plan, remote, commit_id) = cloud_restore_fixture(true).await;
        let args = CloneArgs {
            no_single_branch: false,
            origin: None,
            local: false,
            no_local: false,
            reject_shallow: false,
            reference: vec![],
            reference_if_able: vec![],
            shared: false,
            no_shared: false,
            dissociate: false,
            mirror: false,
            filter: None,
            shallow_since: None,
            shallow_exclude: vec![],
            deps_of: vec![],
            deps_depth_limit: None,
            no_checkout: true,
            no_progress: false,
            remote_repo: "libra+cloud://code.example.com/kepler-ledger".to_string(),
            local_path: None,
            branch: None,
            single_branch: false,
            bare: false,
            depth: None,
            tags: false,
            no_tags: false,
        };
        let output = OutputConfig {
            quiet: true,
            ..OutputConfig::default()
        };

        execute_cloud_publish_clone(&args, &source, restore_plan, remote, parent.path(), &output)
            .await
            .expect("cloud clone --no-checkout should complete");

        let clone_dir = parent.path().join("kepler-ledger");
        // --no-checkout: the working tree is NOT populated...
        assert!(
            !clone_dir.join("README.md").exists(),
            "--no-checkout must not write the working-tree file"
        );
        // ...but objects/refs/HEAD and config ARE restored.
        let _clone_cwd = ChangeDirGuard::new(&clone_dir);
        let head = Head::current_commit_result()
            .await
            .expect("restored HEAD should be readable")
            .expect("restored HEAD should point at a commit");
        assert_eq!(head.to_string(), commit_id.to_string());
        // Config (remote + cloud site) is restored regardless of --no-checkout.
        assert_eq!(
            config_value("remote.origin.url").await.as_deref(),
            Some("libra+cloud://code.example.com/kepler-ledger")
        );
        assert_eq!(
            config_value("cloud.origin.site_id").await.as_deref(),
            Some("site_123")
        );
        let db = get_db_conn_instance().await;
        let restored_tag = reference::Entity::find()
            .filter(reference::Column::Kind.eq(reference::ConfigKind::Tag))
            .filter(reference::Column::Name.eq("refs/tags/v1.0.0"))
            .filter(reference::Column::Remote.is_null())
            .one(&db)
            .await
            .expect("restored tag should be queryable")
            .expect("tag metadata should be restored even with --no-checkout");
        assert_eq!(
            restored_tag.commit.as_deref(),
            Some(commit_id.to_string().as_str())
        );
    }

    #[tokio::test]
    #[serial]
    async fn cloud_clone_restore_test_restores_tag_selector_as_detached_head() {
        let parent = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        let _cwd = ChangeDirGuard::new(parent.path());
        let source = CloudPublishSource {
            clone_domain: "code.example.com".to_string(),
            target: CloudPublishTarget::Slug("kepler-ledger".to_string()),
            selector: Some(CloudPublishSelector::Ref("refs/tags/v1.0.0".to_string())),
        };
        let (mut restore_plan, remote, commit_id) = cloud_restore_fixture(true).await;
        restore_plan.checkout = CloudPublishCheckoutTarget {
            revision_oid: commit_id.to_string(),
            ref_name: Some("refs/tags/v1.0.0".to_string()),
            selector_kind: CloudPublishCheckoutSelectorKind::Ref,
        };
        let args = CloneArgs {
            no_single_branch: false,
            origin: None,
            local: false,
            no_local: false,
            reject_shallow: false,
            reference: vec![],
            reference_if_able: vec![],
            shared: false,
            no_shared: false,
            dissociate: false,
            mirror: false,
            filter: None,
            shallow_since: None,
            shallow_exclude: vec![],
            deps_of: vec![],
            deps_depth_limit: None,
            no_checkout: false,
            no_progress: false,
            remote_repo: "libra+cloud://code.example.com/kepler-ledger?ref=refs/tags/v1.0.0"
                .to_string(),
            local_path: None,
            branch: None,
            single_branch: false,
            bare: false,
            depth: None,
            tags: false,
            no_tags: false,
        };
        let output = OutputConfig {
            quiet: true,
            ..OutputConfig::default()
        };

        let result = execute_cloud_publish_clone(
            &args,
            &source,
            restore_plan,
            remote,
            parent.path(),
            &output,
        )
        .await
        .expect("cloud clone tag restore should complete");

        let clone_dir = parent.path().join("kepler-ledger");
        assert_eq!(result.path, clone_dir.to_string_lossy());
        assert_eq!(result.branch, None);
        let cloud_site = result
            .cloud_site
            .as_ref()
            .expect("cloud clone output should include cloud site metadata");
        assert_eq!(cloud_site.ref_name.as_deref(), Some("refs/tags/v1.0.0"));
        assert_eq!(cloud_site.revision, commit_id.to_string());
        assert_eq!(
            fs::read_to_string(clone_dir.join("README.md")).unwrap(),
            "# cloud\n"
        );

        let _clone_cwd = ChangeDirGuard::new(&clone_dir);
        match Head::current_result()
            .await
            .expect("restored HEAD should be readable")
        {
            Head::Detached(detached) => assert_eq!(detached, commit_id),
            other => panic!("tag selector should detach HEAD, got {other:?}"),
        }
        assert_eq!(
            config_value("remote.origin.url").await.as_deref(),
            Some("libra+cloud://code.example.com/kepler-ledger?ref=refs/tags/v1.0.0")
        );
    }

    #[tokio::test]
    #[serial]
    async fn cloud_clone_restore_test_cleans_destination_when_refs_metadata_missing() {
        let parent = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        let _cwd = ChangeDirGuard::new(parent.path());
        let source = cloud_source();
        let (restore_plan, remote, _) = cloud_restore_fixture(false).await;
        let args = CloneArgs {
            no_single_branch: false,
            origin: None,
            local: false,
            no_local: false,
            reject_shallow: false,
            reference: vec![],
            reference_if_able: vec![],
            shared: false,
            no_shared: false,
            dissociate: false,
            mirror: false,
            filter: None,
            shallow_since: None,
            shallow_exclude: vec![],
            deps_of: vec![],
            deps_depth_limit: None,
            no_checkout: false,
            no_progress: false,
            remote_repo: "libra+cloud://code.example.com/kepler-ledger".to_string(),
            local_path: None,
            branch: None,
            single_branch: false,
            bare: false,
            depth: None,
            tags: false,
            no_tags: false,
        };
        let output = OutputConfig {
            quiet: true,
            ..OutputConfig::default()
        };

        let (error, cleanup_warning) = execute_cloud_publish_clone(
            &args,
            &source,
            restore_plan,
            remote,
            parent.path(),
            &output,
        )
        .await
        .expect_err("missing refs metadata must fail cloud clone");

        assert!(cleanup_warning.is_none());
        assert!(
            matches!(
                error,
                CloneError::CloudPublishRefsMetadataRestoreFailed { .. }
            ),
            "expected refs metadata restore failure, got {error}",
        );
        assert!(
            !parent.path().join("kepler-ledger").exists(),
            "failed cloud clone should remove the destination it created"
        );
    }

    #[tokio::test]
    #[serial]
    async fn cloud_clone_restore_test_cleans_destination_when_refs_metadata_has_no_head() {
        let parent = tempdir().unwrap();
        let home = tempdir().unwrap();
        let _home = ScopedEnvVar::set("HOME", home.path());
        let _test_home = ScopedEnvVar::set("LIBRA_TEST_HOME", home.path());
        let _cwd = ChangeDirGuard::new(parent.path());
        let source = cloud_source();
        let (restore_plan, remote, commit_id) = cloud_restore_fixture(true).await;
        let refs = vec![reference::Model {
            id: 0,
            name: Some("main".to_string()),
            kind: reference::ConfigKind::Branch,
            commit: Some(commit_id.to_string()),
            remote: None,
            worktree_id: None,
        }];
        let metadata = serde_json::to_vec(&refs).expect("metadata should serialize");
        remote
            .put_metadata(&metadata)
            .await
            .expect("metadata should overwrite in-memory remote");
        let args = CloneArgs {
            no_single_branch: false,
            origin: None,
            local: false,
            no_local: false,
            reject_shallow: false,
            reference: vec![],
            reference_if_able: vec![],
            shared: false,
            no_shared: false,
            dissociate: false,
            mirror: false,
            filter: None,
            shallow_since: None,
            shallow_exclude: vec![],
            deps_of: vec![],
            deps_depth_limit: None,
            no_checkout: false,
            no_progress: false,
            remote_repo: "libra+cloud://code.example.com/kepler-ledger".to_string(),
            local_path: None,
            branch: None,
            single_branch: false,
            bare: false,
            depth: None,
            tags: false,
            no_tags: false,
        };
        let output = OutputConfig {
            quiet: true,
            ..OutputConfig::default()
        };

        let (error, cleanup_warning) = execute_cloud_publish_clone(
            &args,
            &source,
            restore_plan,
            remote,
            parent.path(),
            &output,
        )
        .await
        .expect_err("incomplete refs metadata must fail cloud clone");

        assert!(cleanup_warning.is_none());
        match error {
            CloneError::CloudPublishRefsMetadataRestoreFailed { message, .. } => {
                assert!(
                    message.contains("metadata does not contain local HEAD reference"),
                    "error should explain missing HEAD: {message}",
                );
            }
            other => panic!("expected refs metadata restore failure, got {other}"),
        }
        assert!(
            !parent.path().join("kepler-ledger").exists(),
            "failed cloud clone should remove the destination it created"
        );
    }

    fn publish_site_row(
        default_ref: Option<&str>,
        latest_revision_oid: Option<&str>,
    ) -> PublishSiteRow {
        PublishSiteRow {
            site_id: "site_123".to_string(),
            repo_id: "repo_456".to_string(),
            clone_domain: "code.example.com".to_string(),
            slug: "kepler-ledger".to_string(),
            display_origin: "https://code.example.com".to_string(),
            name: "Kepler Ledger".to_string(),
            visibility: "public".to_string(),
            status: "active".to_string(),
            worker_name: "libra-publish".to_string(),
            default_ref: default_ref.map(ToString::to_string),
            latest_revision_oid: latest_revision_oid.map(ToString::to_string),
            refs_generation: 7,
            max_preview_bytes: 1024,
            schema_version: 1,
            created_at: "2026-05-13T00:00:00Z".to_string(),
            updated_at: "2026-05-13T00:00:00Z".to_string(),
        }
    }

    fn cloud_source() -> CloudPublishSource {
        CloudPublishSource {
            clone_domain: "code.example.com".to_string(),
            target: CloudPublishTarget::Slug("kepler-ledger".to_string()),
            selector: None,
        }
    }

    async fn cloud_restore_fixture(
        include_metadata: bool,
    ) -> (CloudPublishRestorePlan, RemoteStorage, ObjectHash) {
        let blob = Blob::from_content("# cloud\n");
        let tree = Tree::from_tree_items(vec![TreeItem::new(
            TreeItemMode::Blob,
            blob.id,
            "README.md".to_string(),
        )])
        .expect("tree should build");
        let commit = Commit::from_tree_id(tree.id, Vec::new(), "cloud clone fixture");

        let remote = RemoteStorage::new(Arc::new(InMemory::new()));
        put_remote_object(&remote, &blob).await;
        put_remote_object(&remote, &tree).await;
        put_remote_object(&remote, &commit).await;

        if include_metadata {
            let refs = vec![
                reference::Model {
                    id: 0,
                    name: Some("main".to_string()),
                    kind: reference::ConfigKind::Head,
                    commit: None,
                    remote: None,
                    worktree_id: None,
                },
                reference::Model {
                    id: 0,
                    name: Some("main".to_string()),
                    kind: reference::ConfigKind::Branch,
                    commit: Some(commit.id.to_string()),
                    remote: None,
                    worktree_id: None,
                },
                reference::Model {
                    id: 0,
                    name: Some("refs/tags/v1.0.0".to_string()),
                    kind: reference::ConfigKind::Tag,
                    commit: Some(commit.id.to_string()),
                    remote: None,
                    worktree_id: None,
                },
            ];
            let metadata = serde_json::to_vec(&refs).expect("refs metadata should serialize");
            remote
                .put_metadata(&metadata)
                .await
                .expect("metadata should upload to in-memory remote");
        }

        let plan = CloudPublishRestorePlan {
            site: publish_site_row(Some("refs/heads/main"), Some(&commit.id.to_string())),
            repository: RepositoryRow {
                repo_id: "repo_456".to_string(),
                name: "Kepler Ledger".to_string(),
                created_at: 1778620800,
                updated_at: 1778620800,
            },
            checkout: CloudPublishCheckoutTarget {
                revision_oid: commit.id.to_string(),
                ref_name: Some("refs/heads/main".to_string()),
                selector_kind: CloudPublishCheckoutSelectorKind::DefaultRef,
            },
            revision: PublishRevisionRow {
                site_id: "site_123".to_string(),
                revision_oid: commit.id.to_string(),
                status: "published".to_string(),
                code_manifest_key: None,
                ai_index_key: None,
                file_count: 1,
                ai_object_count: 0,
                ai_bundle_count: 0,
                redaction_mode: "default".to_string(),
                redaction_rules_version: "1".to_string(),
                sync_run_id: "sync_123".to_string(),
                schema_version: 1,
                created_at: "2026-05-13T00:00:00Z".to_string(),
                updated_at: "2026-05-13T00:00:00Z".to_string(),
            },
            object_indexes: vec![
                object_index_row_with_type(
                    &blob.id.to_string(),
                    "blob",
                    blob.to_data().unwrap().len(),
                ),
                object_index_row_with_type(
                    &tree.id.to_string(),
                    "tree",
                    tree.to_data().unwrap().len(),
                ),
                object_index_row_with_type(
                    &commit.id.to_string(),
                    "commit",
                    commit.to_data().unwrap().len(),
                ),
            ],
            ai_objects: Vec::new(),
            ai_versions: Vec::new(),
        };

        (plan, remote, commit.id)
    }

    async fn put_remote_object<T>(remote: &RemoteStorage, object: &T)
    where
        T: ObjectTrait,
    {
        let data = object.to_data().expect("object data should serialize");
        let hash = object.object_hash().expect("object hash should compute");
        remote
            .put(&hash, &data, object.get_type())
            .await
            .expect("object should upload to in-memory remote");
    }

    async fn config_value(key: &str) -> Option<String> {
        ConfigKv::get(key)
            .await
            .expect("config lookup should succeed")
            .map(|entry| entry.value)
    }

    fn object_index_row(o_id: &str) -> ObjectIndexRow {
        object_index_row_with_type(o_id, "commit", 123)
    }

    fn object_index_row_with_type(o_id: &str, o_type: &str, o_size: usize) -> ObjectIndexRow {
        ObjectIndexRow {
            o_id: o_id.to_string(),
            o_type: o_type.to_string(),
            o_size: o_size as i64,
            repo_id: "repo_456".to_string(),
            created_at: 1778620800,
            is_synced: 1,
        }
    }

    #[derive(Default)]
    struct FakeObjectProbe {
        missing: std::collections::BTreeSet<String>,
    }

    #[async_trait]
    impl CloudCloneObjectProbe for FakeObjectProbe {
        async fn exists(&self, hash: &ObjectHash) -> Result<bool, CloneError> {
            Ok(!self.missing.contains(&hash.to_string()))
        }
    }

    fn publish_ref_row(ref_name: &str, revision_oid: &str) -> PublishRefRow {
        let short_name = ref_name
            .strip_prefix("refs/heads/")
            .or_else(|| ref_name.strip_prefix("refs/tags/"))
            .unwrap_or(ref_name)
            .to_string();
        let ref_type = if ref_name.starts_with("refs/tags/") {
            "tag"
        } else {
            "branch"
        };
        PublishRefRow {
            site_id: "site_123".to_string(),
            ref_name: ref_name.to_string(),
            ref_type: ref_type.to_string(),
            short_name,
            target_oid: revision_oid.to_string(),
            revision_oid: revision_oid.to_string(),
            is_default: if ref_name == "refs/heads/main" { 1 } else { 0 },
            sync_run_id: "sync_123".to_string(),
            schema_version: 1,
            updated_at: "2026-05-13T00:00:00Z".to_string(),
        }
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
            CloneError::CloudCloneDomainNotConfigured {
                domain: "alpha".to_string(),
                missing_keys: "D1_TOKEN".to_string(),
            }
            .to_string(),
            "clone domain 'alpha' is not configured for libra+cloud restore",
        );
        assert_eq!(
            CloneError::CloudCloneD1ApiTokenNotConfigured {
                domain: "alpha".to_string(),
            }
            .to_string(),
            "D1 API token is not configured for clone domain 'alpha'",
        );
        assert_eq!(
            CloneError::CloudCloneD1ApiBaseUrlInvalid {
                domain: "alpha".to_string(),
                message: "not a url".to_string(),
            }
            .to_string(),
            "D1 API base URL is invalid for clone domain 'alpha': not a url",
        );
        assert_eq!(
            CloneError::CloudCloneR2CredentialsNotConfigured {
                domain: "alpha".to_string(),
                missing_keys: "AK,SK".to_string(),
            }
            .to_string(),
            "R2 credentials are not configured for clone domain 'alpha'",
        );
        assert_eq!(
            CloneError::CloudCloneR2ClientBuildFailed {
                domain: "alpha".to_string(),
                message: "tls error".to_string(),
            }
            .to_string(),
            "failed to build R2 client for clone domain 'alpha': tls error",
        );
        assert_eq!(
            CloneError::UnsupportedCloudCloneOption {
                option: "--depth",
                reason: "shallow not supported",
                hint: "omit --depth",
            }
            .to_string(),
            "--depth is not supported with libra+cloud:// clone sources: shallow not supported",
        );
        assert_eq!(
            CloneError::CloudPublishSiteLookupFailed {
                domain: "alpha".to_string(),
                target: "demo".to_string(),
                code: 500,
                message: "timeout".to_string(),
            }
            .to_string(),
            "failed to resolve libra+cloud site demo in clone domain 'alpha' \
             via D1 (code 500): timeout",
        );
        assert_eq!(
            CloneError::CloudPublishSiteNotFound {
                domain: "alpha".to_string(),
                target: "demo".to_string(),
            }
            .to_string(),
            "libra+cloud site demo was not found in clone domain 'alpha'",
        );
        assert_eq!(
            CloneError::CloudPublishSiteUnavailable {
                domain: "alpha".to_string(),
                target: "demo".to_string(),
                status: "draining".to_string(),
            }
            .to_string(),
            "libra+cloud site demo in clone domain 'alpha' is not active: draining",
        );
        assert_eq!(
            CloneError::CloudPublishMetadataLookupFailed {
                domain: "alpha".to_string(),
                site_id: "s1".to_string(),
                operation: "refs",
                code: 404,
                message: "not found".to_string(),
            }
            .to_string(),
            "failed to resolve libra+cloud metadata for site s1 in clone domain 'alpha' during refs (code 404): not found",
        );
        assert_eq!(
            CloneError::CloudPublishRepositoryNotFound {
                domain: "alpha".to_string(),
                target: "demo".to_string(),
                repo_id: "r1".to_string(),
            }
            .to_string(),
            "libra+cloud site demo in clone domain 'alpha' has no repositories row for repo_id r1",
        );
        assert_eq!(
            CloneError::CloudPublishRefsMissing {
                domain: "alpha".to_string(),
                target: "demo".to_string(),
            }
            .to_string(),
            "libra+cloud site demo in clone domain 'alpha' has no published refs",
        );
        assert_eq!(
            CloneError::CloudPublishRefNotFound {
                domain: "alpha".to_string(),
                target: "demo".to_string(),
                selector: "main".to_string(),
            }
            .to_string(),
            "libra+cloud ref selector 'main' did not match a published branch or tag for site demo in clone domain 'alpha'",
        );
        assert_eq!(
            CloneError::CloudPublishRefAmbiguous {
                domain: "alpha".to_string(),
                target: "demo".to_string(),
                selector: "feat".to_string(),
                matches: "feat/a, feat/b".to_string(),
            }
            .to_string(),
            "libra+cloud ref selector 'feat' is ambiguous for site demo in clone domain 'alpha'; matches: feat/a, feat/b",
        );
        assert_eq!(
            CloneError::CloudPublishDefaultRefMissing {
                domain: "alpha".to_string(),
                target: "demo".to_string(),
            }
            .to_string(),
            "libra+cloud site demo in clone domain 'alpha' has no default_ref for clone checkout",
        );
        assert_eq!(
            CloneError::CloudPublishLatestRevisionMissing {
                domain: "alpha".to_string(),
                target: "demo".to_string(),
            }
            .to_string(),
            "libra+cloud site demo in clone domain 'alpha' has no latest_revision_oid for revision=latest",
        );
        assert_eq!(
            CloneError::CloudPublishRevisionNotFound {
                domain: "alpha".to_string(),
                target: "demo".to_string(),
                revision_oid: "deadbeef".to_string(),
            }
            .to_string(),
            "published revision deadbeef for libra+cloud site demo in clone domain 'alpha' was not found",
        );
        assert_eq!(
            CloneError::CloudPublishObjectIndexMissing {
                domain: "alpha".to_string(),
                target: "demo".to_string(),
                repo_id: "r1".to_string(),
            }
            .to_string(),
            "libra+cloud site demo in clone domain 'alpha' has no object_index rows for repo_id r1",
        );
        assert_eq!(
            CloneError::CloudPublishObjectMissing {
                domain: "alpha".to_string(),
                target: "demo".to_string(),
                object_oid: "feedface".to_string(),
            }
            .to_string(),
            "R2 object feedface for libra+cloud site demo in clone domain 'alpha' is missing",
        );
        assert_eq!(
            CloneError::CloudPublishObjectRestoreFailed {
                domain: "alpha".to_string(),
                target: "demo".to_string(),
                message: "checksum".to_string(),
            }
            .to_string(),
            "failed to restore R2 objects for libra+cloud site demo in clone domain 'alpha': checksum",
        );
        assert_eq!(
            CloneError::CloudPublishRefsMetadataRestoreFailed {
                domain: "alpha".to_string(),
                target: "demo".to_string(),
                message: "db err".to_string(),
            }
            .to_string(),
            "failed to restore refs metadata for libra+cloud site demo in clone domain 'alpha': db err",
        );
        assert_eq!(
            CloneError::CloudPublishAiRestoreFailed {
                domain: "alpha".to_string(),
                target: "demo".to_string(),
                message: "ai object".to_string(),
            }
            .to_string(),
            "failed to restore AI object model for libra+cloud site demo in clone domain 'alpha': ai object",
        );
        assert_eq!(
            CloneError::CloudPublishCheckoutSetupFailed {
                domain: "alpha".to_string(),
                target: "demo".to_string(),
                message: "head".to_string(),
            }
            .to_string(),
            "failed to configure checkout for libra+cloud site demo in clone domain 'alpha': head",
        );
        assert_eq!(
            CloneError::CloudPublishObjectIndexInvalid {
                domain: "alpha".to_string(),
                target: "demo".to_string(),
                object_oid: "xyz".to_string(),
                reason: "non-hex".to_string(),
            }
            .to_string(),
            "object_index row xyz for libra+cloud site demo in clone domain 'alpha' is not a valid object id: non-hex",
        );
        assert_eq!(
            CloneError::InvalidCloudPublishSource {
                input: "libra+cloud://bad".to_string(),
                reason: "missing site".to_string(),
            }
            .to_string(),
            "invalid libra+cloud clone source 'libra+cloud://bad': missing site",
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
