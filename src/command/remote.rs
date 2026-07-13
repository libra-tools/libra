//! Manages remotes by listing, adding, removing, renaming, mutating URLs, and
//! pruning stale remote-tracking branches.

use std::{
    collections::{HashMap, HashSet},
    io::{self, Write},
};

use clap::Subcommand;
use git_internal::hash::get_hash_kind;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DbErr, EntityTrait, QueryFilter, Set, TransactionTrait,
};
use serde::Serialize;

use crate::{
    command::fetch,
    internal::{
        branch::{Branch, BranchStoreError},
        config::ConfigKv,
        db::get_db_conn_instance,
        head::Head,
        model::{reference, reflog},
        protocol::{DiscRef, set_wire_hash_kind},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
    },
};

/// Whether a URL entry targets the fetch or push side of a remote.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UrlRole {
    Fetch,
    Push,
}

impl std::fmt::Display for UrlRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UrlRole::Fetch => f.write_str("fetch"),
            UrlRole::Push => f.write_str("push"),
        }
    }
}

/// The mutation performed by `set-url`.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SetUrlMode {
    Add,
    Delete,
    Set,
}

impl std::fmt::Display for SetUrlMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetUrlMode::Add => f.write_str("add"),
            SetUrlMode::Delete => f.write_str("delete"),
            SetUrlMode::Set => f.write_str("set"),
        }
    }
}

/// `--help` examples shown in `libra remote --help` output (attached
/// in `src/cli.rs` via `after_help` on the `Remote` subcommand).
///
/// `remote` exposes eight sub-commands (`add` / `remove` / `rename`
/// / `-v` / `show` / `get-url` / `set-url` / `prune`); the banner pins
/// the most common invocation per sub-command (where it carries enough
/// signal beyond the sub-command name) plus a JSON variant so users can
/// map intent to invocation without reading the design doc. Cross-cutting
/// `--help` EXAMPLES rollout per `docs/development/commands/_general.md` item B.
pub const REMOTE_EXAMPLES: &str = "\
EXAMPLES:
    libra remote -v                                List remotes with fetch/push URLs
    libra remote add origin git@example.com:org/repo.git
                                                   Register a new remote
    libra remote add -f origin git@example.com:org/repo.git
                                                   Register a remote and fetch from it
    libra remote add -t main --tags origin git@example.com:org/repo.git
                                                   Track only main and fetch all tags
    libra remote add --mirror backup git@example.com:org/repo.git
                                                   Register a mirror remote (writes remote.<name>.mirror=true)
    libra remote rename origin upstream            Rename an existing remote
    libra remote remove upstream                   Drop a remote and its tracking refs
    libra remote get-url --all origin              Print every URL configured for origin
    libra remote set-url --push origin https://example.com/org/repo.git
                                                   Replace the push URL only
    libra remote prune --dry-run origin            Preview which tracking refs would be removed
    libra remote update                            Fetch all configured remotes (or named ones)
    libra remote update -p                         Fetch all remotes, then prune stale tracking refs
    libra remote set-branches origin main          Track only the named branch(es)
    libra remote set-head origin main              Point the remote's default branch at main
    libra remote set-head origin --auto            Query the remote and set HEAD automatically
    libra remote show origin                       Query the remote and show tracked/new/stale branches
    libra remote show --no-query origin            Show cached remote-tracking data without contacting the remote
    libra remote --json -v                         Structured JSON output for agents";

#[derive(Subcommand, Debug)]
pub enum RemoteCmds {
    /// Add a remote
    Add {
        /// The name of the remote
        name: String,
        /// The URL of the remote
        url: String,
        /// Immediately fetch from the new remote after adding it.
        #[clap(short = 'f', long = "fetch")]
        fetch: bool,
        /// Track only the given branch(es): write a specific
        /// `remote.<name>.fetch` refspec per branch instead of the default
        /// wildcard. Repeatable.
        #[clap(short = 't', long = "track", value_name = "BRANCH")]
        track: Vec<String>,
        /// Point the remote's HEAD at <BRANCH> (`refs/remotes/<name>/HEAD`).
        #[clap(short = 'm', long = "master", value_name = "BRANCH")]
        master: Option<String>,
        /// Configure `remote.<name>.tagOpt = --tags` (fetch all tags).
        #[clap(long = "tags", conflicts_with = "no_tags")]
        tags: bool,
        /// Configure `remote.<name>.tagOpt = --no-tags` (fetch no tags).
        #[clap(long = "no-tags")]
        no_tags: bool,
        /// Mark the remote as a mirror: write the `remote.<name>.mirror=true`
        /// marker (like Git's `remote add --mirror=fetch`). Incompatible with
        /// `-t`/`--track`. NARROWING vs Git: the marker is informational —
        /// Libra does not write a `+refs/*:refs/*` refspec because `libra fetch`
        /// is not yet mirror-aware (matching `libra clone --mirror`).
        #[clap(long = "mirror", conflicts_with = "track")]
        mirror: bool,
    },
    /// Remove a remote
    Remove {
        /// The name of the remote
        name: String,
    },
    /// Rename a remote
    Rename {
        /// The current name of the remote
        old: String,
        /// The new name of the remote
        new: String,
    },
    /// List remotes verbosely
    #[command(name = "-v")]
    List,
    /// List configured remote names, or show details for one remote.
    Show {
        /// Remote name to inspect. Omit to list configured remote names.
        name: Option<String>,
        /// Do not contact the remote; show cached remote-tracking refs only
        /// (status `cached`). By default `show` queries the remote and classifies
        /// branches as tracked / new / stale.
        #[arg(short = 'n', long = "no-query")]
        no_query: bool,
        /// Include additional detail where available
        #[arg(short, long)]
        verbose: bool,
    },
    /// Print URLs for the given remote.
    ///
    /// Examples:{n}{n}  libra remote get-url origin              # print the fetch URL (first){n}  libra remote get-url --push origin       # print push URLs{n}  libra remote get-url --all origin        # print all configured URLs
    GetUrl {
        /// Print push URLs instead of fetch URL
        #[arg(long)]
        push: bool,
        /// Print all URLs
        #[arg(long)]
        all: bool,
        /// Remote name
        name: String,
    },
    /// Set or modify URLs for the given remote.
    ///
    /// Examples:{n}{n}  libra remote set-url origin newurl              # replace first url{n}  libra remote set-url --all origin newurl        # replace all urls{n}  libra remote set-url --add origin newurl        # add a new url{n}  libra remote set-url --delete origin urlpattern # delete matching url(s)
    SetUrl {
        /// Add the new URL instead of replacing
        #[arg(long)]
        add: bool,
        /// Delete the URL instead of adding/replacing
        #[arg(long)]
        delete: bool,
        /// Operate on push URLs (pushurl) instead of fetch URLs (url)
        #[arg(long)]
        push: bool,
        /// Apply to all matching entries
        #[arg(long)]
        all: bool,
        /// Remote name
        name: String,
        /// URL value (or pattern for --delete)
        value: String,
    },

    /// Delete stale remote-tracking branches.
    ///
    /// Examples:{n}{n}  libra remote prune origin              # prune stale branches for origin{n}  libra remote prune --dry-run origin   # preview what would be pruned
    Prune {
        /// Remote name
        name: String,
        /// Dry run - show what would be pruned without actually deleting
        #[arg(long)]
        dry_run: bool,
    },
    /// Fetch updates from one or more remotes (all configured remotes when none
    /// are named). A name matching a `remotes.<group>` config is expanded to
    /// that group's member remotes.
    ///
    /// Examples:{n}{n}  libra remote update                    # fetch all remotes{n}  libra remote update origin upstream    # fetch the named remotes
    Update {
        /// Remotes or remote groups to update (default: all remotes).
        #[arg(value_name = "GROUP")]
        groups: Vec<String>,
        /// After fetching, prune remote-tracking branches that no longer exist
        /// on the remote (Git's `remote update -p`).
        #[arg(short = 'p', long = "prune")]
        prune: bool,
    },
    /// Set the branches tracked by a remote (rewrites `remote.<name>.fetch`).
    ///
    /// Examples:{n}{n}  libra remote set-branches origin main          # track only main{n}  libra remote set-branches --add origin dev     # also track dev
    SetBranches {
        /// Add to the tracked branches instead of replacing them
        #[arg(long)]
        add: bool,
        /// Remote name
        name: String,
        /// Branch name(s) to track
        #[arg(required = true, num_args = 1..)]
        branches: Vec<String>,
    },
    /// Set or delete the default branch for a remote (`refs/remotes/<name>/HEAD`).
    ///
    /// Examples:{n}{n}  libra remote set-head origin main   # point remote HEAD at main{n}  libra remote set-head origin --auto # query the remote and set HEAD automatically{n}  libra remote set-head origin -d    # delete the remote HEAD ref
    SetHead {
        /// Query the remote and set its HEAD to the branch the remote points at
        #[arg(short = 'a', long = "auto", conflicts_with_all = ["delete", "branch"])]
        auto: bool,
        /// Delete the remote HEAD ref
        #[arg(short = 'd', long = "delete", conflicts_with = "auto")]
        delete: bool,
        /// Remote name
        name: String,
        /// Branch to set as the remote HEAD
        #[arg(conflicts_with_all = ["auto", "delete"])]
        branch: Option<String>,
    },
}

#[derive(Debug, thiserror::Error)]
enum RemoteError {
    #[error("remote '{name}' already exists")]
    AlreadyExists { name: String },

    #[error("SSH key namespace for remote '{name}' already exists")]
    SshKeyNamespaceExists { name: String },

    #[error("no such remote: {name}")]
    NotFound { name: String },

    #[error("no URL configured for remote '{name}'")]
    NoUrlConfigured { name: String },

    #[error("no matching {role} URL found for remote '{name}': {pattern}")]
    UrlPatternNotMatched {
        name: String,
        role: UrlRole,
        pattern: String,
    },

    #[error("failed to read remote configuration: {detail}")]
    ConfigRead { detail: String },

    #[error("failed to update remote configuration: {detail}")]
    ConfigWrite { detail: String },

    #[error("failed to list remote-tracking branches: {detail}")]
    BranchList { detail: String },

    #[error("corrupt remote-tracking branch '{name}': {detail}")]
    BranchCorrupt { name: String, detail: String },

    #[error("failed to prune remote-tracking branch '{name}': {detail}")]
    BranchDelete { name: String, detail: String },

    #[error("remote object format '{remote}' does not match local '{local}'")]
    ObjectFormatMismatch { remote: String, local: String },

    #[error("no such remote-tracking branch '{remote}/{branch}'")]
    RemoteTrackingBranchNotFound { remote: String, branch: String },

    #[error("failed to query remote '{remote}': {detail}")]
    Discovery { remote: String, detail: String },

    #[error("could not determine the default branch for remote '{remote}'")]
    NoRemoteHead { remote: String },

    #[error(transparent)]
    Fetch(#[from] fetch::FetchError),
}

impl From<RemoteError> for CliError {
    fn from(error: RemoteError) -> Self {
        match error {
            RemoteError::AlreadyExists { name } => {
                CliError::fatal(format!("remote '{name}' already exists"))
                    .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                    .with_hint("use 'libra remote -v' to inspect configured remotes")
            }
            RemoteError::SshKeyNamespaceExists { name } => CliError::conflict(format!(
                "SSH key namespace for remote '{name}' already exists"
            ))
            .with_stable_code(StableErrorCode::ConflictOperationBlocked)
            .with_hint(format!(
                "remove or rename vault.ssh.{name}.* config entries before renaming a remote to '{name}'"
            )),
            RemoteError::NotFound { name } => CliError::fatal(format!("no such remote: {name}"))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("use 'libra remote -v' to inspect configured remotes"),
            RemoteError::NoUrlConfigured { name } => {
                CliError::fatal(format!("no URL configured for remote '{name}'"))
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_hint("use 'libra remote get-url --all <name>' to inspect configured URLs")
            }
            RemoteError::UrlPatternNotMatched {
                name,
                role,
                pattern,
            } => CliError::fatal(format!(
                "no matching {role} URL found for remote '{name}': {pattern}"
            ))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
            .with_hint("use 'libra remote get-url --all <name>' to inspect configured URLs"),
            RemoteError::ConfigRead { detail } => {
                CliError::fatal(format!("failed to read remote configuration: {detail}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            }
            RemoteError::BranchList { detail } => {
                CliError::fatal(format!("failed to list remote-tracking branches: {detail}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            }
            RemoteError::BranchCorrupt { name, detail } => {
                CliError::fatal(format!("corrupt remote-tracking branch '{name}': {detail}"))
                    .with_stable_code(StableErrorCode::RepoCorrupt)
            }
            RemoteError::ConfigWrite { detail } => {
                CliError::fatal(format!("failed to update remote configuration: {detail}"))
                    .with_stable_code(StableErrorCode::IoWriteFailed)
            }
            RemoteError::BranchDelete { name, detail } => CliError::fatal(format!(
                "failed to prune remote-tracking branch '{name}': {detail}"
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed),
            RemoteError::ObjectFormatMismatch { remote, local } => CliError::fatal(format!(
                "remote object format '{remote}' does not match local '{local}'"
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid),
            RemoteError::RemoteTrackingBranchNotFound { remote, branch } => {
                CliError::fatal(format!("no such remote-tracking branch '{remote}/{branch}'"))
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_hint("fetch the remote first, or run 'libra remote -v' to inspect remotes")
            }
            RemoteError::Discovery { remote, detail } => {
                CliError::fatal(format!("failed to query remote '{remote}': {detail}"))
                    .with_stable_code(StableErrorCode::NetworkUnavailable)
                    .with_hint(format!(
                        "ensure the remote is reachable, or run 'libra remote show --no-query {remote}' to show cached data"
                    ))
            }
            RemoteError::NoRemoteHead { remote } => {
                CliError::fatal(format!(
                    "could not determine the default branch for remote '{remote}'"
                ))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("the remote advertised no branches; specify a branch explicitly with 'libra remote set-head <name> <branch>'")
            }
            RemoteError::Fetch(source) => CliError::from(source),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RemoteListEntry {
    pub name: String,
    pub fetch_urls: Vec<String>,
    pub push_urls: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RemotePruneEntry {
    pub remote_ref: String,
    pub branch: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RemoteBranchStatus {
    pub branch: String,
    pub status: String,
    pub local_oid: Option<String>,
    pub remote_oid: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RemotePullConfig {
    pub local_branch: String,
    pub remote_branch: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SetHeadMode {
    Set,
    Delete,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum RemoteOutput {
    Add {
        name: String,
        url: String,
    },
    Remove {
        name: String,
    },
    Rename {
        old_name: String,
        new_name: String,
    },
    List {
        verbose: bool,
        remotes: Vec<RemoteListEntry>,
    },
    Urls {
        name: String,
        push: bool,
        all: bool,
        urls: Vec<String>,
    },
    SetUrl {
        name: String,
        role: UrlRole,
        mode: SetUrlMode,
        urls: Vec<String>,
        removed: usize,
    },
    Prune {
        name: String,
        dry_run: bool,
        stale_branches: Vec<RemotePruneEntry>,
    },
    Update {
        remotes: Vec<String>,
        /// Stale remote-tracking branches pruned by `update -p`. Serialized
        /// only when non-empty so a plain `remote update` (no `-p`) keeps its
        /// original `{action, remotes}` JSON shape; `#[serde(default)]` keeps
        /// deserialization of the old shape working.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pruned: Vec<RemotePruneEntry>,
    },
    Show {
        name: String,
        fetch_urls: Vec<String>,
        push_urls: Vec<String>,
        head_branch: Option<String>,
        remote_branches: Vec<RemoteBranchStatus>,
        pull_config: Vec<RemotePullConfig>,
        push_config: Vec<String>,
        queried: bool,
    },
    SetBranches {
        name: String,
        added: bool,
        fetch_refspecs: Vec<String>,
    },
    SetHead {
        name: String,
        mode: SetHeadMode,
        target: Option<String>,
    },
}

pub async fn execute(command: RemoteCmds) {
    if let Err(error) = execute_safe(command, &OutputConfig::default()).await {
        error.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting.
pub async fn execute_safe(command: RemoteCmds, output: &OutputConfig) -> CliResult<()> {
    // Runtime usage validation for the new subcommands (parameter errors map to
    // `command_usage` / 129, not a `RemoteError`).
    validate_remote_usage(&command)?;
    let result = run_remote(command, output).await.map_err(CliError::from)?;
    render_remote_output(&result, output)
}

/// Reject usage errors (e.g. invalid tracking-branch names) before any work
/// runs, mapping them to `command_usage` (exit 129).
fn validate_remote_usage(command: &RemoteCmds) -> CliResult<()> {
    match command {
        RemoteCmds::SetBranches { branches, .. } => {
            for branch in branches {
                validate_tracking_branch_name(branch)?;
            }
        }
        RemoteCmds::SetHead {
            branch: Some(branch),
            ..
        } => {
            validate_tracking_branch_name(branch)?;
        }
        // `remote add -t <branch>` / `-m <branch>` interpolate the branch name
        // into a fetch refspec / the remote-HEAD ref, so they are validated with
        // the same rules as set-branches/set-head before anything is persisted.
        RemoteCmds::Add { track, master, .. } => {
            for branch in track {
                validate_tracking_branch_name(branch)?;
            }
            if let Some(branch) = master {
                validate_tracking_branch_name(branch)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Validate a user-supplied short branch name before it is interpolated into a
/// refspec or a `refs/remotes/<name>/<branch>` ref.
fn validate_tracking_branch_name(name: &str) -> CliResult<()> {
    let invalid = name.is_empty()
        || name.len() > 255
        || name.starts_with('/')
        || name.ends_with('/')
        || name.starts_with("refs/")
        || name.contains("..")
        || name.contains("//")
        || name.chars().any(|c| c.is_control() || c == ' ');
    if invalid {
        return Err(
            CliError::command_usage(format!("invalid branch name '{name}'"))
                .with_hint("use a plain branch name such as 'main'"),
        );
    }
    Ok(())
}

async fn run_remote(
    command: RemoteCmds,
    output: &OutputConfig,
) -> Result<RemoteOutput, RemoteError> {
    match command {
        RemoteCmds::Add {
            name,
            url,
            fetch,
            track,
            master,
            tags,
            no_tags,
            mirror,
        } => {
            run_add_remote(
                AddRemoteArgs {
                    name,
                    url,
                    fetch,
                    track,
                    master,
                    tags,
                    no_tags,
                    mirror,
                },
                output,
            )
            .await
        }
        RemoteCmds::Remove { name } => run_remove_remote(name).await,
        RemoteCmds::Rename { old, new } => run_rename_remote(old, new).await,
        RemoteCmds::List => run_list_remotes(true).await,
        RemoteCmds::Show {
            name,
            no_query,
            verbose: _,
        } => match name {
            Some(name) => run_show_remote(name, no_query).await,
            None => run_list_remotes(false).await,
        },
        RemoteCmds::GetUrl { push, all, name } => run_get_url(name, push, all).await,
        RemoteCmds::SetUrl {
            add,
            delete,
            push,
            all,
            name,
            value,
        } => run_set_url(name, value, push, add, delete, all).await,
        RemoteCmds::Prune { name, dry_run } => run_prune_remote(name, dry_run).await,
        RemoteCmds::Update { groups, prune } => run_remote_update(groups, prune, output).await,
        RemoteCmds::SetBranches {
            add,
            name,
            branches,
        } => run_set_branches(name, branches, add).await,
        RemoteCmds::SetHead {
            auto,
            delete,
            name,
            branch,
        } => run_set_head(name, auto, delete, branch).await,
    }
}

/// Arguments for `remote add`, including its cold-configuration flags.
struct AddRemoteArgs {
    name: String,
    url: String,
    fetch: bool,
    track: Vec<String>,
    master: Option<String>,
    tags: bool,
    no_tags: bool,
    mirror: bool,
}

async fn run_add_remote(
    args: AddRemoteArgs,
    output: &OutputConfig,
) -> Result<RemoteOutput, RemoteError> {
    let AddRemoteArgs {
        name,
        url,
        fetch,
        track,
        master,
        tags,
        no_tags,
        mirror,
    } = args;

    if remote_exists(&name).await? {
        return Err(RemoteError::AlreadyExists { name });
    }

    let write_err = |error: anyhow::Error| RemoteError::ConfigWrite {
        detail: error.to_string(),
    };

    ConfigKv::set(&format!("remote.{name}.url"), &url, false)
        .await
        .map_err(write_err)?;

    // `--mirror`: record the informational `remote.<name>.mirror=true` marker
    // (matching Git's `remote add --mirror=fetch` and `libra clone --mirror`).
    // We deliberately do NOT write a `+refs/*:refs/*` fetch refspec: Libra's
    // fetch is not yet mirror-aware, so the marker is informational only.
    if mirror {
        ConfigKv::set(&format!("remote.{name}.mirror"), "true", false)
            .await
            .map_err(write_err)?;
    }

    // `-t <branch>`: track only the named branch(es) by writing a specific fetch
    // refspec per branch instead of the default wildcard (same format as
    // `remote set-branches`).
    for branch in &track {
        let spec = format!("+refs/heads/{branch}:refs/remotes/{name}/{branch}");
        ConfigKv::add(&format!("remote.{name}.fetch"), &spec, false)
            .await
            .map_err(write_err)?;
    }

    // `--tags`/`--no-tags`: record the tag-fetch preference as `remote.<name>.tagOpt`
    // — the exact key `libra fetch`/`clone` read (config keys are case-sensitive).
    if tags || no_tags {
        let tagopt = if tags { "--tags" } else { "--no-tags" };
        ConfigKv::set(&format!("remote.{name}.tagOpt"), tagopt, false)
            .await
            .map_err(write_err)?;
    }

    // `-m <branch>`: point the remote's HEAD at the given branch. Unlike
    // `remote set-head`, this is written unconditionally at add time (the
    // tracking ref does not exist yet, matching Git's `remote add -m`).
    if let Some(branch) = &master {
        let db = get_db_conn_instance().await;
        let txn_name = name.clone();
        let txn_branch = branch.clone();
        db.transaction::<_, (), DbErr>(move |txn| {
            Box::pin(async move {
                Head::update_result_with_conn(txn, Head::Branch(txn_branch), Some(&txn_name))
                    .await
                    .map_err(|e| DbErr::Custom(e.to_string()))?;
                Ok(())
            })
        })
        .await
        .map_err(|e| RemoteError::ConfigWrite {
            detail: e.to_string(),
        })?;
    }

    // `-f`/`--fetch`: pull from the new remote right after registering it. The
    // remote stays registered even if the fetch fails.
    if fetch {
        fetch_remote_by_name(&name, output).await?;
    }

    Ok(RemoteOutput::Add { name, url })
}

async fn run_remove_remote(name: String) -> Result<RemoteOutput, RemoteError> {
    ensure_remote_exists(&name).await?;
    ConfigKv::remove_remote(&name)
        .await
        .map_err(|error| RemoteError::ConfigWrite {
            detail: error.to_string(),
        })?;
    Ok(RemoteOutput::Remove { name })
}

async fn run_rename_remote(old: String, new: String) -> Result<RemoteOutput, RemoteError> {
    ensure_remote_exists(&old).await?;
    if remote_exists(&new).await? {
        return Err(RemoteError::AlreadyExists { name: new });
    }
    if ssh_key_namespace_exists(&new).await? {
        return Err(RemoteError::SshKeyNamespaceExists { name: new });
    }

    let db = get_db_conn_instance().await;
    let txn_old = old.clone();
    let txn_new = new.clone();
    db.transaction::<_, (), DbErr>(move |txn| {
        Box::pin(async move {
            ConfigKv::rename_remote_with_conn(txn, &txn_old, &txn_new)
                .await
                .map_err(|error| DbErr::Custom(error.to_string()))?;

            let old_tracking_prefix = format!("refs/remotes/{txn_old}/");
            let new_tracking_prefix = format!("refs/remotes/{txn_new}/");
            let references = reference::Entity::find()
                .filter(reference::Column::Remote.eq(&txn_old))
                .all(txn)
                .await?;
            for row in references {
                let renamed_name = row.name.as_ref().and_then(|name| {
                    name.starts_with(&old_tracking_prefix)
                        .then(|| name.replacen(&old_tracking_prefix, &new_tracking_prefix, 1))
                });
                let mut active: reference::ActiveModel = row.into();
                active.remote = Set(Some(txn_new.clone()));
                if let Some(name) = renamed_name {
                    active.name = Set(Some(name));
                }
                active.update(txn).await?;
            }

            let reflogs = reflog::Entity::find()
                .filter(reflog::Column::RefName.starts_with(&old_tracking_prefix))
                .all(txn)
                .await?;
            for row in reflogs {
                let renamed = row
                    .ref_name
                    .replacen(&old_tracking_prefix, &new_tracking_prefix, 1);
                let mut active: reflog::ActiveModel = row.into();
                active.ref_name = Set(renamed);
                active.update(txn).await?;
            }
            Ok(())
        })
    })
    .await
    .map_err(|error| {
        let detail = error.to_string();
        if detail.contains("SSH key namespace for remote") {
            RemoteError::SshKeyNamespaceExists { name: new.clone() }
        } else {
            RemoteError::ConfigWrite { detail }
        }
    })?;
    Ok(RemoteOutput::Rename {
        old_name: old,
        new_name: new,
    })
}

async fn run_list_remotes(verbose: bool) -> Result<RemoteOutput, RemoteError> {
    let remote_names = list_remote_names().await?;

    let mut entries = Vec::with_capacity(remote_names.len());
    for name in remote_names {
        entries.push(load_remote_entry(&name).await?);
    }

    Ok(RemoteOutput::List {
        verbose,
        remotes: entries,
    })
}

/// Discover all remote names by scanning `remote.<name>.*` config keys.
/// Unlike `ConfigKv::all_remote_configs()` (which only recognises remotes with
/// a `.url` entry), this finds any remote that has *any* configuration key.
async fn list_remote_names() -> Result<Vec<String>, RemoteError> {
    let entries =
        ConfigKv::get_by_prefix("remote.")
            .await
            .map_err(|error| RemoteError::ConfigRead {
                detail: error.to_string(),
            })?;
    let mut names = HashSet::new();
    for entry in entries {
        // key format: "remote.<name>.<subkey>" — use `rsplit_once` so that
        // dotted remote names (e.g. "remote.corp.prod.url") are parsed as
        // name="corp.prod", matching `ConfigKv::all_remote_configs`.
        if let Some(rest) = entry.key.strip_prefix("remote.")
            && let Some((name, _subkey)) = rest.rsplit_once('.')
            && !name.is_empty()
        {
            names.insert(name.to_owned());
        }
    }
    let mut names: Vec<String> = names.into_iter().collect();
    names.sort();
    Ok(names)
}

async fn run_get_url(name: String, push: bool, all: bool) -> Result<RemoteOutput, RemoteError> {
    ensure_remote_exists(&name).await?;
    let fetch_urls = load_config_urls(&name, "url").await?;
    let configured_push_urls = load_config_urls(&name, "pushurl").await?;
    let push_urls = effective_push_urls(&fetch_urls, &configured_push_urls);

    let source = if push { &push_urls } else { &fetch_urls };
    let urls: Vec<String> = if all {
        source.clone()
    } else {
        source.iter().take(1).cloned().collect()
    };

    if urls.is_empty() {
        return Err(RemoteError::NoUrlConfigured { name });
    }

    Ok(RemoteOutput::Urls {
        name,
        push,
        all,
        urls,
    })
}

async fn run_set_url(
    name: String,
    value: String,
    push: bool,
    add: bool,
    delete: bool,
    // `--all` and default replace both perform unset-all-then-set, so the
    // behavior is identical today.  We accept the flag for CLI compatibility
    // with Git but do not branch on it.
    #[allow(unused_variables)] all: bool,
) -> Result<RemoteOutput, RemoteError> {
    ensure_remote_exists(&name).await?;

    let key = if push { "pushurl" } else { "url" };
    let role = if push { UrlRole::Push } else { UrlRole::Fetch };
    let full_key = format!("remote.{name}.{key}");

    let mode = if add {
        ConfigKv::add(&full_key, &value, false)
            .await
            .map_err(|error| RemoteError::ConfigWrite {
                detail: error.to_string(),
            })?;
        SetUrlMode::Add
    } else if delete {
        let entries =
            ConfigKv::get_all(&full_key)
                .await
                .map_err(|error| RemoteError::ConfigRead {
                    detail: error.to_string(),
                })?;
        let removed = entries
            .iter()
            .filter(|entry| entry.value.contains(&value))
            .count();
        if removed == 0 {
            return Err(RemoteError::UrlPatternNotMatched {
                name,
                role,
                pattern: value,
            });
        }

        ConfigKv::unset_all(&full_key)
            .await
            .map_err(|error| RemoteError::ConfigWrite {
                detail: error.to_string(),
            })?;
        for entry in entries
            .into_iter()
            .filter(|entry| !entry.value.contains(&value))
        {
            ConfigKv::add(&full_key, &entry.value, entry.encrypted)
                .await
                .map_err(|error| RemoteError::ConfigWrite {
                    detail: error.to_string(),
                })?;
        }

        let urls = load_config_urls(&name, key).await?;
        return Ok(RemoteOutput::SetUrl {
            name,
            role,
            mode: SetUrlMode::Delete,
            urls,
            removed,
        });
    } else {
        ConfigKv::unset_all(&full_key)
            .await
            .map_err(|error| RemoteError::ConfigWrite {
                detail: error.to_string(),
            })?;
        ConfigKv::set(&full_key, &value, false)
            .await
            .map_err(|error| RemoteError::ConfigWrite {
                detail: error.to_string(),
            })?;
        SetUrlMode::Set
    };

    let urls = load_config_urls(&name, key).await?;
    Ok(RemoteOutput::SetUrl {
        name,
        role,
        mode,
        urls,
        removed: 0,
    })
}

/// Resolve the set of remotes for `remote update`: with no arguments, every
/// configured remote (in config order); otherwise each argument is either a
/// `remotes.<group>` config (expanded to its space-separated members) or a
/// single remote name. The result preserves first-seen order and de-duplicates.
async fn resolve_update_remotes(groups: Vec<String>) -> Result<Vec<String>, RemoteError> {
    let groups = if groups.is_empty() {
        match ConfigKv::get("remotes.default")
            .await
            .map_err(|error| RemoteError::ConfigRead {
                detail: error.to_string(),
            })? {
            Some(config) if !config.value.trim().is_empty() => {
                config.value.split_whitespace().map(String::from).collect()
            }
            _ => return list_remote_names().await,
        }
    } else {
        groups
    };
    let mut resolved = Vec::new();
    let mut seen = HashSet::new();
    for entry in groups {
        let names: Vec<String> =
            match ConfigKv::get(&format!("remotes.{entry}"))
                .await
                .map_err(|error| RemoteError::ConfigRead {
                    detail: error.to_string(),
                })? {
                Some(cfg) => cfg.value.split_whitespace().map(String::from).collect(),
                None => vec![entry],
            };
        for name in names {
            if seen.insert(name.clone()) {
                resolved.push(name);
            }
        }
    }
    Ok(resolved)
}

/// `remote update [-p|--prune] [<group>|<remote>...]`: fetch from each resolved
/// remote, then optionally prune. An unknown remote name is an error; a fetch
/// failure aborts the run before any pruning happens.
async fn run_remote_update(
    groups: Vec<String>,
    prune: bool,
    output: &OutputConfig,
) -> Result<RemoteOutput, RemoteError> {
    let remotes = resolve_update_remotes(groups).await?;
    // First pass: fetch every resolved remote. A fetch failure aborts the run
    // HERE, before any tracking ref is deleted, so `-p` can never delete refs
    // for an early remote and then strand that destructive side effect behind
    // an error raised while fetching a later remote.
    let mut updated = Vec::new();
    for name in &remotes {
        ensure_remote_exists(name).await?;
        fetch_remote_by_name(name, output).await?;
        updated.push(name.clone());
    }
    // Second pass: `-p`/`--prune` drops local remote-tracking branches that no
    // longer exist on the remote (reuses `run_prune_remote`). Reached only once
    // every fetch above succeeded.
    let mut pruned = Vec::new();
    if prune {
        for name in &updated {
            if let RemoteOutput::Prune { stale_branches, .. } =
                run_prune_remote(name.clone(), false).await?
            {
                pruned.extend(stale_branches);
            }
        }
    }
    Ok(RemoteOutput::Update {
        remotes: updated,
        pruned,
    })
}

/// Fetch from a configured remote by name. Shared by `remote update` and
/// `remote add -f`.
async fn fetch_remote_by_name(name: &str, output: &OutputConfig) -> Result<(), RemoteError> {
    let remote_config = ConfigKv::remote_config(name)
        .await
        .map_err(|error| RemoteError::ConfigRead {
            detail: error.to_string(),
        })?
        .ok_or_else(|| RemoteError::NoUrlConfigured {
            name: name.to_string(),
        })?;
    fetch::fetch_repository_safe(remote_config, None, false, None, None, output).await?;
    Ok(())
}

/// Build the set of branch names a remote currently advertises, normalised into
/// the local remote-tracking display form used by [`classify_stale_tracking_branches`]:
/// `<branch>` for `refs/heads/<branch>` and `mr/<x>` for `refs/mr/<x>`. Peeled
/// (`^{}`) and tag refs are ignored — they are not tracked under
/// `refs/remotes/<name>/*`. Shared by `remote prune` and `fetch --prune` so both
/// derive the live-branch set identically.
pub(crate) fn remote_advertised_branch_names(refs: &[DiscRef]) -> HashSet<String> {
    refs.iter()
        .filter_map(|reference| {
            reference
                ._ref
                .strip_prefix("refs/heads/")
                .map(String::from)
                .or_else(|| {
                    reference
                        ._ref
                        .strip_prefix("refs/mr/")
                        .map(|mr| format!("mr/{mr}"))
                })
        })
        .collect()
}

/// Classify which locally-stored remote-tracking branches under
/// `refs/remotes/<name>/*` are stale — i.e. the remote no longer advertises a
/// matching `refs/heads/<branch>` (or `refs/mr/<x>`). The cached
/// `refs/remotes/<name>/HEAD` is never reported. Pure (no deletion); shared by
/// `remote prune` and `fetch --prune` so both classify staleness identically.
pub(crate) fn classify_stale_tracking_branches(
    name: &str,
    remote_branch_names: &HashSet<String>,
    local_remote_branches: &[Branch],
) -> Vec<RemotePruneEntry> {
    let head_ref = format!("refs/remotes/{name}/HEAD");
    let prefix = format!("refs/remotes/{name}/");
    let mut stale_branches = Vec::new();
    for local_branch in local_remote_branches {
        if local_branch.name == head_ref {
            continue;
        }
        let Some(branch_name) = local_branch.name.strip_prefix(&prefix) else {
            continue;
        };
        if remote_branch_names.contains(branch_name) {
            continue;
        }
        stale_branches.push(RemotePruneEntry {
            remote_ref: local_branch.name.clone(),
            branch: format!("{name}/{branch_name}"),
        });
    }
    stale_branches
}

async fn run_prune_remote(name: String, dry_run: bool) -> Result<RemoteOutput, RemoteError> {
    ensure_remote_exists(&name).await?;
    let remote_config = ConfigKv::remote_config(&name)
        .await
        .map_err(|error| RemoteError::ConfigRead {
            detail: error.to_string(),
        })?
        .ok_or_else(|| RemoteError::NoUrlConfigured { name: name.clone() })?;

    let (_remote_client, discovery) =
        fetch::discover_remote_with_name(&remote_config.url, Some(&remote_config.name)).await?;

    let local_kind = get_hash_kind();
    if discovery.hash_kind != local_kind {
        return Err(RemoteError::ObjectFormatMismatch {
            remote: discovery.hash_kind.to_string(),
            local: local_kind.to_string(),
        });
    }

    set_wire_hash_kind(discovery.hash_kind);

    let remote_branch_names = remote_advertised_branch_names(&discovery.refs);

    let local_remote_branches =
        Branch::list_branches_result(Some(&name))
            .await
            .map_err(|error| match error {
                BranchStoreError::Query(detail) => RemoteError::BranchList { detail },
                BranchStoreError::Corrupt { name, detail } => {
                    RemoteError::BranchCorrupt { name, detail }
                }
                other => RemoteError::BranchList {
                    detail: other.to_string(),
                },
            })?;

    let stale_branches =
        classify_stale_tracking_branches(&name, &remote_branch_names, &local_remote_branches);

    if !dry_run {
        for entry in &stale_branches {
            Branch::delete_branch_result(&entry.remote_ref, Some(&name))
                .await
                .map_err(|error| match error {
                    BranchStoreError::Delete { name, detail } => {
                        RemoteError::BranchDelete { name, detail }
                    }
                    BranchStoreError::Corrupt { name, detail } => {
                        RemoteError::BranchCorrupt { name, detail }
                    }
                    BranchStoreError::Query(detail) => RemoteError::BranchList { detail },
                    other => RemoteError::ConfigWrite {
                        detail: other.to_string(),
                    },
                })?;
        }
    }

    Ok(RemoteOutput::Prune {
        name,
        dry_run,
        stale_branches,
    })
}

/// A remote is considered to exist if **any** `remote.<name>.*` key is
/// present, not only `remote.<name>.url`.  This handles the edge case where
/// `set-url --delete` removed the last fetch URL but other keys (e.g.
/// `pushurl`, vault SSH keys) still remain.
///
/// Uses `rsplit_once('.')` name extraction to avoid prefix collisions with
/// dotted remote names (e.g. querying "corp" must not match a key belonging
/// to remote "corp.prod").
async fn remote_exists(name: &str) -> Result<bool, RemoteError> {
    let prefix = format!("remote.{name}.");
    let entries =
        ConfigKv::get_by_prefix(&prefix)
            .await
            .map_err(|error| RemoteError::ConfigRead {
                detail: error.to_string(),
            })?;
    // Verify that at least one entry actually parses as belonging to this
    // exact remote name, not a longer dotted name that shares the prefix.
    Ok(entries.iter().any(|e| {
        e.key
            .strip_prefix("remote.")
            .and_then(|rest| rest.rsplit_once('.'))
            .is_some_and(|(parsed_name, _)| parsed_name == name)
    }))
}

async fn ssh_key_namespace_exists(name: &str) -> Result<bool, RemoteError> {
    let prefix = format!("vault.ssh.{name}.");
    ConfigKv::get_by_prefix(&prefix)
        .await
        .map(|entries| !entries.is_empty())
        .map_err(|error| RemoteError::ConfigRead {
            detail: error.to_string(),
        })
}

async fn ensure_remote_exists(name: &str) -> Result<(), RemoteError> {
    if remote_exists(name).await? {
        Ok(())
    } else {
        Err(RemoteError::NotFound {
            name: name.to_string(),
        })
    }
}

/// Load a remote's URL configuration.  Tolerates missing fetch URLs so that
/// remotes that only have `pushurl` (e.g. after `set-url --delete` removed the
/// last fetch URL) are still visible in listings and accessible to `get-url
/// --push`.
async fn load_remote_entry(name: &str) -> Result<RemoteListEntry, RemoteError> {
    ensure_remote_exists(name).await?;
    let fetch_urls = load_config_urls(name, "url").await?;
    let configured_push_urls = load_config_urls(name, "pushurl").await?;
    let push_urls = effective_push_urls(&fetch_urls, &configured_push_urls);

    Ok(RemoteListEntry {
        name: name.to_string(),
        fetch_urls,
        push_urls,
    })
}

async fn load_config_urls(name: &str, key: &str) -> Result<Vec<String>, RemoteError> {
    ConfigKv::get_all(&format!("remote.{name}.{key}"))
        .await
        .map_err(|error| RemoteError::ConfigRead {
            detail: error.to_string(),
        })
        .map(|entries| entries.into_iter().map(|entry| entry.value).collect())
}

fn effective_push_urls(fetch_urls: &[String], push_urls: &[String]) -> Vec<String> {
    if push_urls.is_empty() {
        fetch_urls.to_vec()
    } else {
        push_urls.to_vec()
    }
}

/// Contact the remote and return its advertised branch heads, capability
/// strings, and HEAD ref — the inputs to `fetch::resolve_remote_default_branch`
/// and to online `remote show` classification.
async fn discover_remote_refs(
    name: &str,
) -> Result<(Vec<DiscRef>, Vec<String>, Option<DiscRef>), RemoteError> {
    let entry = load_remote_entry(name).await?;
    let url = entry
        .fetch_urls
        .first()
        .ok_or_else(|| RemoteError::NoUrlConfigured {
            name: name.to_string(),
        })?;
    let (_, discovery) = fetch::discover_remote_with_name(url, Some(name))
        .await
        .map_err(|error| RemoteError::Discovery {
            remote: name.to_string(),
            detail: error.to_string(),
        })?;
    let ref_heads = discovery
        .refs
        .iter()
        .filter(|reference| reference._ref.starts_with("refs/heads/"))
        .cloned()
        .collect();
    let remote_head = discovery
        .refs
        .iter()
        .find(|reference| reference._ref == "HEAD")
        .cloned();
    Ok((ref_heads, discovery.capabilities, remote_head))
}

/// Detailed `remote show <name>`. By default it contacts the remote (like
/// `git remote show`) to report the live remote HEAD and classify branches as
/// `tracked` / `new` / `stale` against the local remote-tracking refs. With
/// `--no-query` it stays fully offline: the cached remote HEAD
/// (`refs/remotes/<name>/HEAD`) and the cached tracking branches (status
/// `cached`). Both modes also report the configured fetch/push URLs and the
/// local `branch.<b>.remote`/`.merge` pull configuration.
async fn run_show_remote(name: String, no_query: bool) -> Result<RemoteOutput, RemoteError> {
    ensure_remote_exists(&name).await?;
    let entry = load_remote_entry(&name).await?;
    let local_tracking = load_local_tracking_refs(&name).await?;
    let pull_config = load_pull_config(&name).await?;
    let push_config = load_config_urls(&name, "pushurl").await?;

    let (head_branch, remote_branches, queried) = if no_query {
        (
            load_cached_remote_head(&name).await?,
            classify_cached_remote_branches(&local_tracking),
            false,
        )
    } else {
        let (ref_heads, capabilities, remote_head) = discover_remote_refs(&name).await?;
        let head_branch =
            fetch::resolve_remote_default_branch(&capabilities, &ref_heads, remote_head.as_ref());
        let remote_branches = classify_remote_branches_online(&ref_heads, &local_tracking);
        (head_branch, remote_branches, true)
    };

    Ok(RemoteOutput::Show {
        name,
        fetch_urls: entry.fetch_urls,
        push_urls: entry.push_urls,
        head_branch,
        remote_branches,
        pull_config,
        push_config,
        queried,
    })
}

/// Read the cached remote HEAD branch from `refs/remotes/<name>/HEAD` (a `Head`
/// row). A detached or absent remote HEAD yields `None`.
async fn load_cached_remote_head(name: &str) -> Result<Option<String>, RemoteError> {
    let db = get_db_conn_instance().await;
    Head::remote_current_result_with_conn(&db, name)
        .await
        .map_err(|error| RemoteError::BranchList {
            detail: error.to_string(),
        })
        .map(|head| match head {
            Some(Head::Branch(branch)) => Some(branch),
            Some(Head::Detached(_)) | None => None,
        })
}

/// Collect the cached remote-tracking branch OIDs for `<name>` as
/// `short branch name -> commit OID`, skipping the synthetic `HEAD` ref.
async fn load_local_tracking_refs(name: &str) -> Result<HashMap<String, String>, RemoteError> {
    let prefix = format!("refs/remotes/{name}/");
    let head_ref = format!("{prefix}HEAD");
    let branches = Branch::list_branches_result(Some(name))
        .await
        .map_err(|error| RemoteError::BranchList {
            detail: error.to_string(),
        })?;
    let mut refs = HashMap::new();
    for branch in branches {
        if branch.name == head_ref {
            continue;
        }
        if let Some(short) = branch.name.strip_prefix(&prefix) {
            refs.insert(short.to_string(), branch.commit.to_string());
        }
    }
    Ok(refs)
}

/// Classify branches online by comparing the remote's advertised heads against
/// the local remote-tracking refs:
/// - `tracked`: advertised by the remote and already tracked locally (carries
///   both the remote and local OIDs),
/// - `new`: advertised by the remote but not yet tracked locally (a later
///   `fetch` will create it) — remote OID only,
/// - `stale`: tracked locally but no longer advertised by the remote (a `remote
///   prune` would drop it) — local OID only.
///
/// Output is sorted by branch name for deterministic rendering.
fn classify_remote_branches_online(
    ref_heads: &[DiscRef],
    local_tracking: &HashMap<String, String>,
) -> Vec<RemoteBranchStatus> {
    let mut out = Vec::new();
    let mut remote_names = HashSet::new();
    for reference in ref_heads {
        let Some(branch) = reference._ref.strip_prefix("refs/heads/") else {
            continue;
        };
        remote_names.insert(branch.to_string());
        let local_oid = local_tracking.get(branch).cloned();
        let status = if local_oid.is_some() {
            "tracked"
        } else {
            "new"
        };
        out.push(RemoteBranchStatus {
            branch: branch.to_string(),
            status: status.to_string(),
            local_oid,
            remote_oid: Some(reference._hash.clone()),
        });
    }
    for (branch, local_oid) in local_tracking {
        if !remote_names.contains(branch) {
            out.push(RemoteBranchStatus {
                branch: branch.clone(),
                status: "stale".to_string(),
                local_oid: Some(local_oid.clone()),
                remote_oid: None,
            });
        }
    }
    out.sort_by(|a, b| a.branch.cmp(&b.branch));
    out
}

/// Classify cached remote-tracking branches (offline). Every cached branch is
/// reported with the `cached` status and its local OID.
fn classify_cached_remote_branches(
    local_tracking: &HashMap<String, String>,
) -> Vec<RemoteBranchStatus> {
    let mut names = local_tracking.keys().cloned().collect::<Vec<_>>();
    names.sort();
    names
        .into_iter()
        .map(|branch| {
            let local_oid = local_tracking.get(&branch).cloned();
            RemoteBranchStatus {
                branch,
                status: "cached".to_string(),
                local_oid,
                remote_oid: None,
            }
        })
        .collect()
}

/// Read the `branch.<b>.remote` / `branch.<b>.merge` pull configuration that
/// points at this remote, in a single config scan (avoids N+1 queries).
async fn load_pull_config(name: &str) -> Result<Vec<RemotePullConfig>, RemoteError> {
    let entries =
        ConfigKv::get_by_prefix("branch.")
            .await
            .map_err(|error| RemoteError::ConfigRead {
                detail: error.to_string(),
            })?;
    let mut branch_remotes = HashMap::new();
    let mut branch_merges = HashMap::new();
    for entry in entries {
        let Some(rest) = entry.key.strip_prefix("branch.") else {
            continue;
        };
        let Some((branch, suffix)) = rest.rsplit_once('.') else {
            continue;
        };
        match suffix {
            "remote" => {
                branch_remotes.insert(branch.to_string(), entry.value);
            }
            "merge" => {
                branch_merges.insert(
                    branch.to_string(),
                    entry
                        .value
                        .strip_prefix("refs/heads/")
                        .unwrap_or(&entry.value)
                        .to_string(),
                );
            }
            _ => {}
        }
    }
    let mut configs = branch_remotes
        .into_iter()
        .filter_map(|(local_branch, remote)| {
            if remote != name {
                return None;
            }
            branch_merges
                .get(&local_branch)
                .map(|remote_branch| RemotePullConfig {
                    local_branch,
                    remote_branch: remote_branch.clone(),
                })
        })
        .collect::<Vec<_>>();
    configs.sort_by(|left, right| left.local_branch.cmp(&right.local_branch));
    Ok(configs)
}

/// `remote set-branches [--add] <name> <branch>...` — rewrite (or append to)
/// `remote.<name>.fetch` as `+refs/heads/<b>:refs/remotes/<name>/<b>` in a single
/// `ConfigKv` transaction.
async fn run_set_branches(
    name: String,
    branches: Vec<String>,
    add: bool,
) -> Result<RemoteOutput, RemoteError> {
    ensure_remote_exists(&name).await?;

    let refspecs: Vec<String> = branches
        .iter()
        .map(|branch| format!("+refs/heads/{branch}:refs/remotes/{name}/{branch}"))
        .collect();

    let key = format!("remote.{name}.fetch");
    let db = get_db_conn_instance().await;
    let txn_key = key.clone();
    let txn_refspecs = refspecs.clone();
    db.transaction::<_, (), DbErr>(move |txn| {
        Box::pin(async move {
            if !add {
                ConfigKv::unset_all_with_conn(txn, &txn_key)
                    .await
                    .map_err(|e| DbErr::Custom(e.to_string()))?;
            }
            for spec in &txn_refspecs {
                ConfigKv::add_with_conn(txn, &txn_key, spec, false)
                    .await
                    .map_err(|e| DbErr::Custom(e.to_string()))?;
            }
            Ok(())
        })
    })
    .await
    .map_err(|e| RemoteError::ConfigWrite {
        detail: e.to_string(),
    })?;

    Ok(RemoteOutput::SetBranches {
        name,
        added: add,
        fetch_refspecs: refspecs,
    })
}

/// `remote set-head <name> (<branch> | -d/--delete)` — write or delete the remote
/// HEAD ref `refs/remotes/<name>/HEAD` (a `Head` row, not a `Branch` row). The
/// explicit-branch mode requires the tracking branch to already exist. `--auto`
/// is rejected earlier in `validate_remote_usage`.
async fn run_set_head(
    name: String,
    auto: bool,
    delete: bool,
    branch: Option<String>,
) -> Result<RemoteOutput, RemoteError> {
    ensure_remote_exists(&name).await?;
    let db = get_db_conn_instance().await;

    if delete {
        let txn_name = name.clone();
        db.transaction::<_, (), DbErr>(move |txn| {
            Box::pin(async move {
                // Remote HEAD is a `Head` row (refs/remotes/<name>/HEAD), not a
                // `Branch` row — delete it directly. Absent row is a no-op.
                reference::Entity::delete_many()
                    .filter(reference::Column::Kind.eq(reference::ConfigKind::Head))
                    .filter(reference::Column::Remote.eq(txn_name))
                    .exec(txn)
                    .await?;
                Ok(())
            })
        })
        .await
        .map_err(|e| RemoteError::ConfigWrite {
            detail: e.to_string(),
        })?;
        return Ok(RemoteOutput::SetHead {
            name,
            mode: SetHeadMode::Delete,
            target: None,
        });
    }

    // Resolve the target branch: `--auto` queries the remote for its HEAD
    // (symref capability, else OID match, else main/master/first); otherwise the
    // explicit `<branch>` argument is required.
    let branch = if auto {
        let (ref_heads, capabilities, remote_head) = discover_remote_refs(&name).await?;
        fetch::resolve_remote_default_branch(&capabilities, &ref_heads, remote_head.as_ref())
            .ok_or_else(|| RemoteError::NoRemoteHead {
                remote: name.clone(),
            })?
    } else {
        branch.ok_or_else(|| RemoteError::RemoteTrackingBranchNotFound {
            remote: name.clone(),
            branch: String::new(),
        })?
    };

    // The tracking branch must already exist locally. It is stored under the
    // full ref `refs/remotes/<name>/<branch>` with the `remote` column = name.
    let full_ref = format!("refs/remotes/{name}/{branch}");
    let exists = Branch::find_branch_result(&full_ref, Some(&name))
        .await
        .map_err(|e| RemoteError::BranchList {
            detail: e.to_string(),
        })?
        .is_some();
    if !exists {
        return Err(RemoteError::RemoteTrackingBranchNotFound {
            remote: name,
            branch,
        });
    }

    let txn_name = name.clone();
    let txn_branch = branch.clone();
    db.transaction::<_, (), DbErr>(move |txn| {
        Box::pin(async move {
            Head::update_result_with_conn(txn, Head::Branch(txn_branch), Some(&txn_name))
                .await
                .map_err(|e| DbErr::Custom(e.to_string()))?;
            Ok(())
        })
    })
    .await
    .map_err(|e| RemoteError::ConfigWrite {
        detail: e.to_string(),
    })?;

    Ok(RemoteOutput::SetHead {
        name,
        mode: SetHeadMode::Set,
        target: Some(branch),
    })
}

fn redact_url_list(urls: &[String]) -> Vec<String> {
    urls.iter()
        .map(|url| fetch::redact_url_credentials(url))
        .collect()
}

/// Return a copy of the output with every URL credential redacted, so the JSON
/// envelope never leaks userinfo. Variants without URLs are cloned unchanged.
fn redacted_remote_output(result: &RemoteOutput) -> RemoteOutput {
    match result {
        RemoteOutput::Add { name, url } => RemoteOutput::Add {
            name: name.clone(),
            url: fetch::redact_url_credentials(url),
        },
        RemoteOutput::List { verbose, remotes } => RemoteOutput::List {
            verbose: *verbose,
            remotes: remotes
                .iter()
                .map(|remote| RemoteListEntry {
                    name: remote.name.clone(),
                    fetch_urls: redact_url_list(&remote.fetch_urls),
                    push_urls: redact_url_list(&remote.push_urls),
                })
                .collect(),
        },
        RemoteOutput::Urls {
            name,
            push,
            all,
            urls,
        } => RemoteOutput::Urls {
            name: name.clone(),
            push: *push,
            all: *all,
            urls: redact_url_list(urls),
        },
        RemoteOutput::SetUrl {
            name,
            role,
            mode,
            urls,
            removed,
        } => RemoteOutput::SetUrl {
            name: name.clone(),
            role: *role,
            mode: *mode,
            urls: redact_url_list(urls),
            removed: *removed,
        },
        RemoteOutput::Show {
            name,
            fetch_urls,
            push_urls,
            head_branch,
            remote_branches,
            pull_config,
            push_config,
            queried,
        } => RemoteOutput::Show {
            name: name.clone(),
            fetch_urls: redact_url_list(fetch_urls),
            push_urls: redact_url_list(push_urls),
            head_branch: head_branch.clone(),
            remote_branches: remote_branches.clone(),
            pull_config: pull_config.clone(),
            push_config: redact_url_list(push_config),
            queried: *queried,
        },
        RemoteOutput::Remove { .. }
        | RemoteOutput::Rename { .. }
        | RemoteOutput::Prune { .. }
        | RemoteOutput::Update { .. }
        | RemoteOutput::SetBranches { .. }
        | RemoteOutput::SetHead { .. } => result.clone(),
    }
}

fn render_remote_output(result: &RemoteOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        let redacted = redacted_remote_output(result);
        return emit_json_data("remote", &redacted, output);
    }

    if output.quiet {
        return Ok(());
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let write_err =
        |error: io::Error| CliError::io(format!("failed to write remote output: {error}"));

    match result {
        RemoteOutput::Add { name, url } => writeln!(
            writer,
            "Added remote '{name}' -> {}",
            fetch::redact_url_credentials(url)
        )
        .map_err(write_err),
        RemoteOutput::Remove { name } => {
            writeln!(writer, "Removed remote '{name}'").map_err(write_err)
        }
        RemoteOutput::Rename { old_name, new_name } => {
            writeln!(writer, "Renamed remote '{old_name}' to '{new_name}'").map_err(write_err)
        }
        RemoteOutput::List { verbose, remotes } => {
            if *verbose {
                for remote in remotes {
                    for url in &remote.fetch_urls {
                        writeln!(
                            writer,
                            "{}\t{} (fetch)",
                            remote.name,
                            fetch::redact_url_credentials(url)
                        )
                        .map_err(write_err)?;
                    }
                    for url in &remote.push_urls {
                        writeln!(
                            writer,
                            "{}\t{} (push)",
                            remote.name,
                            fetch::redact_url_credentials(url)
                        )
                        .map_err(write_err)?;
                    }
                }
            } else {
                for remote in remotes {
                    writeln!(writer, "{}", remote.name).map_err(write_err)?;
                }
            }
            Ok(())
        }
        RemoteOutput::Urls { urls, .. } => {
            for url in urls {
                writeln!(writer, "{}", fetch::redact_url_credentials(url)).map_err(write_err)?;
            }
            Ok(())
        }
        RemoteOutput::SetUrl {
            name,
            role,
            mode,
            urls,
            removed,
        } => match mode {
            SetUrlMode::Add => writeln!(
                writer,
                "Added {role} URL for remote '{name}': {}",
                fetch::redact_url_credentials(&urls.last().cloned().unwrap_or_default())
            )
            .map_err(write_err),
            SetUrlMode::Delete => writeln!(
                writer,
                "Removed {removed} {role} URL(s) from remote '{name}'"
            )
            .map_err(write_err),
            SetUrlMode::Set => writeln!(
                writer,
                "Set {role} URL for remote '{name}' to {}",
                fetch::redact_url_credentials(&urls.first().cloned().unwrap_or_default())
            )
            .map_err(write_err),
        },
        RemoteOutput::Prune {
            name: _,
            dry_run,
            stale_branches,
        } => {
            for entry in stale_branches {
                if *dry_run {
                    writeln!(writer, " * [would prune] {}", entry.branch).map_err(write_err)?;
                } else {
                    writeln!(writer, " * [pruned] {}", entry.branch).map_err(write_err)?;
                }
            }

            if stale_branches.is_empty() {
                writeln!(writer, "Everything up-to-date").map_err(write_err)?;
            } else if *dry_run {
                writeln!(
                    writer,
                    "\nWould prune {} stale remote-tracking branch(es).",
                    stale_branches.len()
                )
                .map_err(write_err)?;
            } else {
                writeln!(
                    writer,
                    "\nPruned {} stale remote-tracking branch(es).",
                    stale_branches.len()
                )
                .map_err(write_err)?;
            }
            Ok(())
        }
        RemoteOutput::Update { remotes, pruned } => {
            // The per-remote fetch already streamed its own progress; emit a
            // short confirmation line per updated remote (or a notice when
            // there were none to update).
            if remotes.is_empty() {
                writeln!(writer, "No remotes to update").map_err(write_err)?;
            } else {
                for name in remotes {
                    writeln!(writer, "Updated {name}").map_err(write_err)?;
                }
            }
            // `-p`/`--prune`: report any stale remote-tracking branches removed.
            for entry in pruned {
                writeln!(writer, " * [pruned] {}", entry.branch).map_err(write_err)?;
            }
            Ok(())
        }
        RemoteOutput::Show {
            name,
            fetch_urls,
            push_urls,
            head_branch,
            remote_branches,
            pull_config,
            push_config,
            queried,
        } => {
            writeln!(writer, "* remote {name}").map_err(write_err)?;
            for url in fetch_urls {
                writeln!(
                    writer,
                    "  Fetch URL: {}",
                    fetch::redact_url_credentials(url)
                )
                .map_err(write_err)?;
            }
            for url in push_urls {
                writeln!(writer, "  Push URL: {}", fetch::redact_url_credentials(url))
                    .map_err(write_err)?;
            }
            writeln!(
                writer,
                "  HEAD branch: {}",
                head_branch.as_deref().unwrap_or("(unknown)")
            )
            .map_err(write_err)?;
            if !queried {
                writeln!(writer, "  Remote branch data: cached").map_err(write_err)?;
            }
            writeln!(writer, "  Remote branches:").map_err(write_err)?;
            if remote_branches.is_empty() {
                writeln!(writer, "    (none)").map_err(write_err)?;
            } else {
                for branch in remote_branches {
                    let suffix = match branch.status.as_str() {
                        "new" => {
                            format!(" (next fetch will store in remotes/{name})")
                        }
                        "stale" => " (use 'libra remote prune' to remove)".to_string(),
                        _ => String::new(),
                    };
                    writeln!(writer, "    {} {}{}", branch.branch, branch.status, suffix)
                        .map_err(write_err)?;
                }
            }
            writeln!(writer, "  Local branches configured for 'git pull':").map_err(write_err)?;
            if pull_config.is_empty() {
                writeln!(writer, "    (none)").map_err(write_err)?;
            } else {
                for config in pull_config {
                    writeln!(
                        writer,
                        "    {} merges with remote {}",
                        config.local_branch, config.remote_branch
                    )
                    .map_err(write_err)?;
                }
            }
            writeln!(writer, "  Local refs configured for 'git push':").map_err(write_err)?;
            if push_config.is_empty() {
                writeln!(writer, "    (none)").map_err(write_err)?;
            } else {
                for refspec in push_config {
                    writeln!(writer, "    {}", fetch::redact_url_credentials(refspec))
                        .map_err(write_err)?;
                }
            }
            Ok(())
        }
        RemoteOutput::SetBranches {
            name,
            added,
            fetch_refspecs,
        } => {
            let verb = if *added {
                "Now tracking"
            } else {
                "Set to track"
            };
            writeln!(
                writer,
                "{verb} {} branch(es) for remote '{name}'.",
                fetch_refspecs.len()
            )
            .map_err(write_err)?;
            Ok(())
        }
        RemoteOutput::SetHead { name, mode, target } => {
            match (mode, target) {
                (SetHeadMode::Delete, _) => {
                    writeln!(writer, "Deleted remote HEAD for '{name}'.").map_err(write_err)?;
                }
                (SetHeadMode::Set, Some(branch)) => {
                    writeln!(writer, "{name}/HEAD set to {branch}.").map_err(write_err)?;
                }
                (SetHeadMode::Set, None) => {}
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the `Display` format for [`RemoteError`] variants whose
    /// pattern is fully owned by this enum (i.e., the `#[error(...)]`
    /// attribute is fully formed with `{field}` interpolations rather
    /// than `{0}` source forwarding to upstream Display).
    ///
    /// The `#[error(transparent)] Fetch` variant forwards to
    /// `fetch::FetchError` which has its own pin test
    /// (`fetch_error_display_pins_static_message_variants`), so it's
    /// intentionally skipped here.
    #[test]
    fn remote_error_display_pins_each_owned_variant() {
        assert_eq!(
            RemoteError::AlreadyExists {
                name: "origin".to_string(),
            }
            .to_string(),
            "remote 'origin' already exists",
        );
        assert_eq!(
            RemoteError::SshKeyNamespaceExists {
                name: "upstream".to_string(),
            }
            .to_string(),
            "SSH key namespace for remote 'upstream' already exists",
        );
        assert_eq!(
            RemoteError::NotFound {
                name: "upstream".to_string(),
            }
            .to_string(),
            "no such remote: upstream",
        );
        assert_eq!(
            RemoteError::NoUrlConfigured {
                name: "origin".to_string(),
            }
            .to_string(),
            "no URL configured for remote 'origin'",
        );
        assert_eq!(
            RemoteError::UrlPatternNotMatched {
                name: "origin".to_string(),
                role: UrlRole::Push,
                pattern: "https://*".to_string(),
            }
            .to_string(),
            "no matching push URL found for remote 'origin': https://*",
        );
        assert_eq!(
            RemoteError::ConfigRead {
                detail: "db locked".to_string(),
            }
            .to_string(),
            "failed to read remote configuration: db locked",
        );
        assert_eq!(
            RemoteError::ConfigWrite {
                detail: "disk full".to_string(),
            }
            .to_string(),
            "failed to update remote configuration: disk full",
        );
        assert_eq!(
            RemoteError::BranchList {
                detail: "query failed".to_string(),
            }
            .to_string(),
            "failed to list remote-tracking branches: query failed",
        );
        assert_eq!(
            RemoteError::BranchCorrupt {
                name: "refs/remotes/origin/main".to_string(),
                detail: "invalid hash".to_string(),
            }
            .to_string(),
            "corrupt remote-tracking branch 'refs/remotes/origin/main': invalid hash",
        );
        assert_eq!(
            RemoteError::BranchDelete {
                name: "refs/remotes/origin/stale".to_string(),
                detail: "row locked".to_string(),
            }
            .to_string(),
            "failed to prune remote-tracking branch 'refs/remotes/origin/stale': row locked",
        );
        assert_eq!(
            RemoteError::ObjectFormatMismatch {
                remote: "sha1".to_string(),
                local: "sha256".to_string(),
            }
            .to_string(),
            "remote object format 'sha1' does not match local 'sha256'",
        );
        assert_eq!(
            RemoteError::RemoteTrackingBranchNotFound {
                remote: "origin".to_string(),
                branch: "dev".to_string(),
            }
            .to_string(),
            "no such remote-tracking branch 'origin/dev'",
        );
    }
}
