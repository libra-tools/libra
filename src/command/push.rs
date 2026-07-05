//! Push command wiring that reads remote configuration, negotiates with servers, and sends local refs and pack data for update.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::Write,
    path::Path,
    str::FromStr,
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use clap::Parser;
use git_internal::{
    errors::GitError,
    hash::{HashKind, ObjectHash, get_hash_kind},
    internal::{
        metadata::{EntryMeta, MetaAttached},
        object::{
            blob::Blob,
            commit::Commit,
            tag::Tag as GitTagObject,
            tree::{Tree, TreeItemMode},
        },
        pack::{encode::PackEncoder, entry::Entry},
    },
};
use sea_orm::{TransactionError, TransactionTrait};
use serde::Serialize;
use tokio::sync::mpsc;
use url::Url;

use crate::{
    command::{branch, fetch::RemoteClient, lfs_schema::LfsUploadSummary},
    git_protocol::{ServiceType::ReceivePack, add_pkt_line_string, read_pkt_line},
    info_println,
    internal::{
        ai::automation::{VCS_EVENT_POST_PUSH, dispatch_current_repo_vcs_event_to_history},
        branch::{Branch, BranchStoreError},
        config::ConfigKv,
        db::get_db_conn_instance,
        head::Head,
        protocol::{
            ProtocolClient, get_wire_hash_kind, lfs_client::LFSClient, set_wire_hash_kind,
            ssh_client::is_ssh_spec,
        },
        reflog::{Reflog, ReflogAction, ReflogContext},
        tag,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode, emit_warning},
        object_ext::{BlobExt, CommitExt, TreeExt},
        output::{OutputConfig, ProgressMode, ProgressReporter, emit_json_data},
        text::levenshtein,
    },
};

const ISSUE_URL: &str = "https://github.com/libra-tools/libra/issues";

/// Total timeout for push reference discovery and initial connection setup.
const PUSH_CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
/// Idle timeout for push send-pack / receive-pack operations.
const PUSH_IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// Push local refs and objects to a remote repository.
///
/// See `libra push --help` for the same examples rendered through clap.
///
/// `--help` examples shown in `libra push --help` output. The list
/// covers the six most common scenarios (default upstream push, named
/// remote/branch push, `-u` upstream setup, forced overwrite, dry-run,
/// JSON for agents) so a user does not need to read the design doc to
/// remember the canonical flags. Cross-cutting `--help` EXAMPLES rollout
/// per `docs/development/commands/_general.md` item B.
pub const PUSH_EXAMPLES: &str = "\
EXAMPLES:
    libra push                          Push current branch to tracking remote
    libra push origin main              Push main branch to origin
    libra push origin main feature:release
                                        Push multiple refspecs in one request
    libra push origin :feature          Delete the remote feature branch
    libra push -d origin feature        Delete the remote feature branch (short form)
    libra push --tags origin            Push local tags
    libra push --mirror --dry-run origin
                                        Preview a mirror sync without writing
    libra push -u origin feature-x      Push and set upstream tracking
    libra push --force origin main      Force push (overwrites remote history)
    libra push --force-with-lease origin main
                                        Force push only if the remote still matches your tracking ref
    libra push --porcelain origin main  Machine-readable per-ref status lines
    libra push --dry-run                Preview what would be pushed without sending
    libra push --json                   Structured JSON output for agents";

#[derive(Parser, Debug)]
#[command(after_help = PUSH_EXAMPLES)]
pub struct PushArgs {
    /// repository, e.g. origin
    repository: Option<String>,
    /// refs to push, e.g. master or local_branch:remote_branch
    #[clap(value_name = "REFSPEC")]
    refspecs: Vec<String>,

    /// Record the upstream tracking ref so future pushes/pulls default to it
    #[clap(long, short = 'u', requires("repository"))]
    set_upstream: bool,

    /// force push to remote repository
    #[clap(long, short = 'f')]
    pub force: bool,

    /// Delete the named remote refs (each REFSPEC is rewritten to a `:<ref>`
    /// deletion request).
    #[clap(long, short = 'd')]
    pub delete: bool,

    /// Force the update only if the remote ref still matches the expected (lease) OID.
    /// Forms: bare `--force-with-lease`, `=<refname>`, or `=<refname>:<expect>`
    #[clap(long, num_args = 0..=1, require_equals = true, conflicts_with = "force")]
    pub force_with_lease: Option<Option<String>>,

    /// With --force-with-lease (All/Ref forms): additionally require the
    /// remote-tracking tip to be integrated locally (reachable from the
    /// pushed branch's reflog) — Git's safety refinement against pushing
    /// over fetched-but-unseen updates. Silent no-op with the exact
    /// `=<ref>:<oid>` lease form or without a lease (Git parity).
    #[clap(long)]
    pub force_if_includes: bool,

    /// Accepted for compatibility; thin-pack encoding is not implemented (no-op)
    #[clap(long, conflicts_with = "no_thin")]
    pub thin: bool,

    /// Accepted for compatibility; thin-pack encoding is not implemented (no-op)
    #[clap(long = "no-thin", conflicts_with = "thin")]
    pub no_thin: bool,

    /// Request an atomic push: the remote applies all ref updates together or
    /// none at all. Errors if the remote does not advertise the `atomic`
    /// capability.
    #[clap(long)]
    pub atomic: bool,

    /// Transmit a server-side option (repeatable). Errors if the remote does not
    /// advertise the `push-options` capability.
    #[clap(long = "push-option", short = 'o', value_name = "OPTION")]
    pub push_option: Vec<String>,

    /// Also push annotated tags that point at commits being pushed and are
    /// missing on the remote.
    #[clap(long)]
    pub follow_tags: bool,

    /// GPG-sign the push with a push certificate. Errors if the remote does not
    /// advertise the `push-cert` capability or no signing key is configured.
    #[clap(long)]
    pub signed: bool,

    /// Machine-readable single-line-per-ref output; conflicts with `--json`/`--machine`
    #[clap(long)]
    pub porcelain: bool,

    /// Do everything except actually send the updates
    #[clap(long, short = 'n')]
    pub dry_run: bool,

    /// Push all local tag refs under refs/tags/*
    #[clap(long, requires("repository"))]
    pub tags: bool,

    /// Mirror all local refs/heads/* and refs/tags/* to the remote, deleting remote-only refs
    #[clap(long, requires("repository"))]
    pub mirror: bool,

    /// Bypass the `pre-push` hook. Accepted for Git parity and is a no-op:
    /// Libra's push does not run a client-side `pre-push` hook, so there is
    /// nothing to bypass.
    #[clap(long = "no-verify")]
    pub no_verify: bool,

    /// Do not show the progress meter (the "Compressing objects" / "Writing
    /// objects" reporters) on stderr, matching `git push --no-progress`.
    #[clap(long = "no-progress")]
    pub no_progress: bool,
}

impl PushArgs {
    /// Build a programmatic push invocation for wrappers that pin the remote
    /// and exact refspecs instead of accepting the full `libra push` flag set.
    pub(crate) fn for_refspecs(repository: String, refspecs: Vec<String>) -> Self {
        Self {
            repository: Some(repository),
            refspecs,
            set_upstream: false,
            force: false,
            delete: false,
            force_with_lease: None,
            force_if_includes: false,
            thin: false,
            no_thin: false,
            atomic: false,
            push_option: Vec::new(),
            follow_tags: false,
            signed: false,
            porcelain: false,
            dry_run: false,
            tags: false,
            mirror: false,
            no_verify: false,
            no_progress: false,
        }
    }

    /// Like [`Self::for_refspecs`] but with a `--force-with-lease=<spec>`
    /// lease attached (`<refname>` or `<refname>:<expect>` forms). Used by
    /// `libra agent push --force-rewrite`, whose Libra-managed
    /// `refs/libra/traces` ref is rewritten wholesale by `agent clean`
    /// prunes: the lease permits the non-fast-forward update while still
    /// failing closed when the remote moved past the expected tip.
    pub(crate) fn for_refspecs_with_lease(
        repository: String,
        refspecs: Vec<String>,
        force_with_lease: String,
    ) -> Self {
        Self {
            force_with_lease: Some(Some(force_with_lease)),
            ..Self::for_refspecs(repository, refspecs)
        }
    }
}

// ---------------------------------------------------------------------------
// Structured error types
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum PushError {
    #[error("HEAD is detached; cannot determine what to push")]
    DetachedHead,

    #[error("no configured push destination")]
    NoRemoteConfigured,

    #[error("remote '{name}' not found")]
    RemoteNotFound {
        name: String,
        suggestion: Option<String>,
    },

    #[error("invalid refspec '{0}'")]
    InvalidRefspec(String),

    #[error("{0}")]
    InvalidArguments(String),

    #[error("source ref '{0}' not found")]
    SourceRefNotFound(String),

    #[error("pushing to local file repositories is not supported")]
    UnsupportedLocalFileRemote,

    #[error("invalid remote URL '{url}': {detail}")]
    InvalidRemoteUrl { url: String, detail: String },

    #[error("authentication failed for '{url}'")]
    AuthenticationFailed { url: String },

    #[error("failed to discover references from '{url}': {detail}")]
    DiscoveryFailed { url: String, detail: String },

    #[error("network timeout during {phase} after {seconds}s")]
    Timeout { phase: String, seconds: u64 },

    #[error(
        "updates were rejected for '{remote_ref}': the remote-tracking tip {tracking_oid} has \
         not been integrated locally (remote ref updated since checkout)"
    )]
    ForceIfIncludesRejected {
        remote_ref: String,
        tracking_oid: String,
    },

    #[error("cannot push to '{remote_ref}': non-fast-forward update")]
    NonFastForward {
        local_ref: String,
        remote_ref: String,
    },

    #[error("remote object format '{remote}' does not match local '{local}'")]
    HashKindMismatch { remote: String, local: String },

    #[error("failed to collect objects for push: {0}")]
    ObjectCollection(String),

    #[error("pack encoding failed: {0}")]
    PackEncoding(String),

    #[error("remote rejected push: unpack failed")]
    RemoteUnpackFailed,

    #[error("remote rejected ref update for '{refname}': {reason}")]
    RemoteRefUpdateFailed { refname: String, reason: String },

    #[error("network error: {0}")]
    Network(String),

    #[error("LFS upload failed for '{path}': {detail}")]
    LfsUploadFailed {
        path: String,
        oid: String,
        detail: String,
    },

    #[error("failed to update local tracking ref: {0}")]
    TrackingRefUpdate(String),

    #[error("failed to read repository state: {0}")]
    RepoState(String),

    #[error("the receiving end does not support --atomic push")]
    AtomicUnsupported,

    #[error("the receiving end does not support push options")]
    PushOptionsUnsupported,

    #[error("the receiving end does not support signed push")]
    PushSignUnsupported,

    #[error("--signed requires a configured signing key, but none was found")]
    PushSignNoKey,

    #[error("failed to create push certificate signature: {0}")]
    PushSignFailed(String),
}

impl From<PushError> for CliError {
    fn from(error: PushError) -> Self {
        match &error {
            PushError::DetachedHead => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("checkout a branch before pushing")
                .with_hint("use 'libra switch <branch>' to switch"),
            PushError::NoRemoteConfigured => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("use 'libra remote add <name> <url>' to configure a remote")
                .with_hint("or specify the remote explicitly: 'libra push <remote> <branch>'"),
            PushError::RemoteNotFound { suggestion, .. } => {
                let mut err = CliError::fatal(error.to_string())
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_hint("use 'libra remote -v' to see configured remotes");
                if let Some(s) = suggestion {
                    err = err.with_priority_hint(format!("did you mean '{s}'?"));
                }
                err
            }
            PushError::InvalidRefspec(..) => CliError::command_usage(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("use '<name>' or '<src>:<dst>'"),
            PushError::InvalidArguments(..) => CliError::command_usage(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments),
            PushError::SourceRefNotFound(..) => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("verify the local branch/ref exists before pushing"),
            PushError::UnsupportedLocalFileRemote => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint(
                    "use fetch/clone for local-path repositories; push currently supports network remotes only",
                ),
            PushError::InvalidRemoteUrl { .. } => CliError::command_usage(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("check the remote URL with 'libra remote get-url <name>'"),
            PushError::AuthenticationFailed { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::AuthMissingCredentials)
                .with_hint("check SSH key or HTTP credentials")
                .with_hint("use 'libra config --list' to verify auth settings"),
            PushError::DiscoveryFailed { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::NetworkUnavailable)
                .with_hint("check the remote URL and network connectivity"),
            PushError::Timeout { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::NetworkUnavailable)
                .with_hint("check network connectivity and retry"),
            PushError::ForceIfIncludesRejected { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                .with_hint("integrate the remote update first: 'libra pull' (or fetch + rebase)")
                .with_hint(
                    "drop --force-if-includes only if discarding the remote update is intended",
                ),
            PushError::NonFastForward { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                .with_hint("pull and integrate remote changes first: 'libra pull'")
                .with_hint("or use --force to overwrite (data loss risk)"),
            PushError::HashKindMismatch { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::NetworkProtocol),
            PushError::ObjectCollection(..) => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::InternalInvariant)
                .with_hint(format!("this is a bug; please report it at {ISSUE_URL}")),
            PushError::PackEncoding(..) => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::InternalInvariant)
                .with_hint(format!("this is a bug; please report it at {ISSUE_URL}")),
            PushError::RemoteUnpackFailed => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::NetworkProtocol)
                .with_hint("the remote server failed to process the pack; retry or check server logs"),
            PushError::RemoteRefUpdateFailed { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::NetworkProtocol)
                .with_hint("the remote rejected the update; check branch protection rules"),
            PushError::Network(..) => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::NetworkUnavailable)
                .with_hint("check network connectivity and retry"),
            PushError::LfsUploadFailed { path, oid, .. } => {
                let mut err = CliError::fatal(error.to_string())
                    .with_stable_code(StableErrorCode::NetworkUnavailable)
                    .with_hint("check LFS endpoint configuration");
                if oid != "(unknown)" {
                    err = err.with_detail("oid", oid.clone());
                }
                if path != "(unknown)" {
                    err = err.with_detail("path", path.clone());
                }
                err
            }
            PushError::TrackingRefUpdate(..) => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoWriteFailed)
            }
            PushError::RepoState(..) => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint("try 'libra status' to verify repository state"),
            PushError::AtomicUnsupported => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::NetworkProtocol)
                .with_hint("retry without --atomic, or push to a remote that supports atomic updates"),
            PushError::PushOptionsUnsupported => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::NetworkProtocol)
                .with_hint("retry without -o/--push-option, or push to a remote that supports push options"),
            PushError::PushSignUnsupported => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::NetworkProtocol)
                .with_hint("retry without --signed, or push to a remote that supports signed push"),
            PushError::PushSignNoKey => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::AuthMissingCredentials)
                .with_hint("configure a signing key (see 'libra config user.signingkey' / vault setup)"),
            PushError::PushSignFailed(_) => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::InternalInvariant),
        }
    }
}

/// Decide whether to advertise the `atomic` capability. With `--atomic`, the
/// remote must have advertised `atomic` during reference discovery; otherwise
/// the push is refused up-front (matching Git's client behaviour).
fn resolve_atomic_capability(
    atomic_requested: bool,
    server_capabilities: &[String],
) -> Result<bool, PushError> {
    if !atomic_requested {
        return Ok(false);
    }
    if server_capabilities.iter().any(|cap| cap == "atomic") {
        Ok(true)
    } else {
        Err(PushError::AtomicUnsupported)
    }
}

/// Decide whether to send a push-options section. When options are supplied the
/// remote must have advertised the `push-options` capability during discovery;
/// otherwise the push is refused up-front (matching Git).
fn resolve_push_options_capability(
    options: &[String],
    server_capabilities: &[String],
) -> Result<bool, PushError> {
    if options.is_empty() {
        return Ok(false);
    }
    if server_capabilities.iter().any(|cap| cap == "push-options") {
        Ok(true)
    } else {
        Err(PushError::PushOptionsUnsupported)
    }
}

/// Encode the push-options protocol section: one pkt-line per option followed by
/// a flush-pkt, appended after the ref-update command flush and before the pack.
fn encode_push_options(options: &[String], data: &mut BytesMut) {
    for option in options {
        add_pkt_line_string(data, option.clone());
    }
    data.extend_from_slice(b"0000");
}

/// Resolve the push-certificate nonce for `--signed`. The remote must advertise
/// `push-cert[=<nonce>]` during discovery; otherwise the push is refused.
fn resolve_push_cert_nonce(
    signed: bool,
    server_capabilities: &[String],
) -> Result<Option<String>, PushError> {
    if !signed {
        return Ok(None);
    }
    for cap in server_capabilities {
        if let Some(nonce) = cap.strip_prefix("push-cert=") {
            return Ok(Some(nonce.to_string()));
        }
        if cap == "push-cert" {
            return Ok(Some(String::new()));
        }
    }
    Err(PushError::PushSignUnsupported)
}

/// Build the signed payload of a push certificate (Git's `certificate
/// version 0.1` text): a header block, a blank line, then one `<old> <new>
/// <ref>` command per line. This exact text is what gets GPG-signed.
fn build_push_certificate(
    pusher: &str,
    pushee: &str,
    nonce: &str,
    commands: &[(String, String, String)],
) -> String {
    let mut cert = String::new();
    cert.push_str("certificate version 0.1\n");
    cert.push_str(&format!("pusher {pusher}\n"));
    cert.push_str(&format!("pushee {pushee}\n"));
    cert.push_str(&format!("nonce {nonce}\n"));
    cert.push('\n');
    for (old, new, refname) in commands {
        cert.push_str(&format!("{old} {new} {refname}\n"));
    }
    cert
}

/// Frame a signed push certificate into the send-pack stream: the `push-cert`
/// announcement (carrying the capability list), the signed certificate body,
/// the armored signature, and the `push-cert-end` terminator, then a flush.
fn encode_push_cert_section(
    capability: &str,
    certificate: &str,
    armored_signature: &str,
    data: &mut BytesMut,
) {
    add_pkt_line_string(data, format!("push-cert\0{capability}\n"));
    for line in certificate.lines() {
        add_pkt_line_string(data, format!("{line}\n"));
    }
    for line in armored_signature.lines() {
        add_pkt_line_string(data, format!("{line}\n"));
    }
    add_pkt_line_string(data, "push-cert-end\n".to_string());
    data.extend_from_slice(b"0000");
}

// ---------------------------------------------------------------------------
// Structured output types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PushRefUpdateKind {
    Update,
    Delete,
}

#[derive(Debug, Clone, Serialize)]
pub struct PushRefUpdate {
    pub kind: PushRefUpdateKind,
    pub local_ref: String,
    pub remote_ref: String,
    pub old_oid: Option<String>,
    pub new_oid: String,
    pub forced: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PushOutput {
    /// Push target remote name
    pub remote: String,
    /// Push target URL
    pub url: String,
    /// Ref updates performed
    pub updates: Vec<PushRefUpdate>,
    /// Number of objects pushed
    pub objects_pushed: usize,
    /// Bytes of pack data pushed
    pub bytes_pushed: u64,
    /// Number of LFS files uploaded
    pub lfs_files_uploaded: usize,
    pub lfs_upload: LfsUploadSummary,
    /// Whether this was a dry-run
    pub dry_run: bool,
    /// Whether everything was already up-to-date
    pub up_to_date: bool,
    /// Upstream tracking branch set (if --set-upstream)
    pub upstream_set: Option<String>,
    /// Warning messages (e.g. force push)
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Refspec parsing
// ---------------------------------------------------------------------------

/// Parsed refspec before repository state resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedRefspec {
    /// Update a remote ref from a local source ref.
    Update { src: String, dst: String },
    /// Delete a remote ref by sending a zero object id.
    Delete { dst: String },
}

/// Parse a refspec string into an update or deletion request.
///
/// Supported forms:
/// - `<name>` — push local `<name>` to remote `<name>`
/// - `<src>:<dst>` — push local `<src>` to remote `<dst>`
/// - `:<dst>` — delete remote `<dst>`
///
/// Empty destinations (e.g. `src:`) are rejected.
fn parse_refspec(refspec: &str) -> Result<ParsedRefspec, PushError> {
    if refspec.is_empty() {
        return Err(PushError::InvalidRefspec(refspec.to_string()));
    }

    // Only 0 or 1 colon is valid; reject multi-colon forms like "a:b:c"
    if refspec.matches(':').count() > 1 {
        return Err(PushError::InvalidRefspec(refspec.to_string()));
    }

    if let Some((src, dst)) = refspec.split_once(':') {
        if dst.is_empty() {
            return Err(PushError::InvalidRefspec(refspec.to_string()));
        }
        if src.is_empty() {
            Ok(ParsedRefspec::Delete {
                dst: dst.to_string(),
            })
        } else {
            Ok(ParsedRefspec::Update {
                src: src.to_string(),
                dst: dst.to_string(),
            })
        }
    } else {
        Ok(ParsedRefspec::Update {
            src: refspec.to_string(),
            dst: refspec.to_string(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalRefKind {
    Branch,
    Tag,
}

#[derive(Debug, Clone)]
struct ResolvedLocalRef {
    full_ref: String,
    oid: ObjectHash,
    kind: LocalRefKind,
}

#[derive(Debug, Clone)]
struct RefUpdatePlan {
    update: PushRefUpdate,
    old_oid: ObjectHash,
    new_oid: Option<ObjectHash>,
    local_kind: Option<LocalRefKind>,
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

pub async fn execute(args: PushArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting.
///
/// # Side Effects
/// - Reads current branch and remote configuration.
/// - Negotiates with the remote server and uploads pack data/ref updates.
/// - May update upstream tracking configuration when `--set-upstream` is used.
/// - Renders push status in human or JSON form.
///
/// # Errors
/// Returns [`CliError`] when arguments are incomplete, HEAD is detached, remote
/// configuration is missing, authentication/network negotiation fails, pack data
/// cannot be read, or upstream config cannot be written.
pub async fn execute_safe(args: PushArgs, output: &OutputConfig) -> CliResult<()> {
    let mut args = args;
    // `-d`/`--delete`: rewrite each positional REFSPEC into a `:<ref>` deletion
    // request, reusing the existing deletion path.
    args.refspecs = apply_delete_flag(
        std::mem::take(&mut args.refspecs),
        args.delete,
        args.set_upstream,
        args.tags,
        args.mirror,
    )
    .map_err(CliError::from)?;

    validate_push_args(&args).map_err(CliError::from)?;

    // `--porcelain` is mutually exclusive with any JSON mode. The global
    // `--json`/`--machine` flags live on `Cli`, not `PushArgs`, so clap cannot
    // express this — enforce it here (`is_json()` covers both, since `--machine`
    // implies `--json=ndjson`).
    if args.porcelain && output.is_json() {
        return Err(CliError::command_usage(
            "--porcelain cannot be combined with --json or --machine",
        )
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint("choose one machine-readable format: --porcelain or --json/--machine"));
    }

    let porcelain = args.porcelain;
    let result = run_push(args, output).await.map_err(CliError::from)?;
    render_push_output(&result, output, porcelain)?;
    if !result.dry_run && !result.up_to_date && !result.updates.is_empty() {
        dispatch_current_repo_vcs_event_to_history(VCS_EVENT_POST_PUSH).await;
    }
    Ok(())
}

/// Apply `-d`/`--delete`: turn each plain ref name into a `:<ref>` deletion
/// refspec. `--delete` requires at least one ref, rejects refspecs that already
/// carry a `:`, and cannot be combined with `--set-upstream`/`--tags`/`--mirror`.
fn apply_delete_flag(
    refspecs: Vec<String>,
    delete: bool,
    set_upstream: bool,
    tags: bool,
    mirror: bool,
) -> Result<Vec<String>, PushError> {
    if !delete {
        return Ok(refspecs);
    }
    if set_upstream || tags || mirror {
        return Err(PushError::InvalidArguments(
            "--delete cannot be combined with --set-upstream, --tags, or --mirror".to_string(),
        ));
    }
    if refspecs.is_empty() {
        return Err(PushError::InvalidArguments(
            "--delete requires at least one ref to delete".to_string(),
        ));
    }
    refspecs
        .into_iter()
        .map(|refspec| {
            if refspec.contains(':') {
                Err(PushError::InvalidArguments(format!(
                    "--delete does not accept a ':' refspec: '{refspec}'"
                )))
            } else {
                Ok(format!(":{refspec}"))
            }
        })
        .collect()
}

fn validate_push_args(args: &PushArgs) -> Result<(), PushError> {
    if args.repository.is_none() && (!args.refspecs.is_empty() || args.tags || args.mirror) {
        return Err(PushError::InvalidArguments(
            "repository is required when specifying refspecs, --tags, or --mirror".to_string(),
        ));
    }
    if args.repository.is_some() && args.refspecs.is_empty() && !args.tags && !args.mirror {
        return Err(PushError::InvalidArguments(
            "repository-only push requires a refspec, --tags, or --mirror".to_string(),
        ));
    }
    if args.set_upstream && (args.refspecs.len() != 1 || args.tags) {
        return Err(PushError::InvalidArguments(
            "--set-upstream requires exactly one branch refspec".to_string(),
        ));
    }
    if args.mirror && (!args.refspecs.is_empty() || args.tags || args.set_upstream) {
        return Err(PushError::InvalidArguments(
            "--mirror cannot be combined with refspecs, --tags, or --set-upstream".to_string(),
        ));
    }

    Ok(())
}

async fn validate_local_refspecs(args: &PushArgs, current_branch: &str) -> Result<(), PushError> {
    if args.mirror {
        return Ok(());
    }

    if args.refspecs.is_empty() && !args.tags {
        resolve_local_ref(current_branch).await?;
    }

    for refspec in &args.refspecs {
        match parse_refspec(refspec)? {
            ParsedRefspec::Update { src, dst } => {
                let local_ref = resolve_local_ref(&src).await?;
                normalize_destination_ref(&dst, local_ref.kind)?;
            }
            ParsedRefspec::Delete { dst } => {
                normalize_delete_ref(&dst)?;
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Pure execution
// ---------------------------------------------------------------------------

/// Pure execution entry point. Does NOT render output — returns [`PushOutput`]
/// on success for the caller to render.
pub async fn run_push(args: PushArgs, output: &OutputConfig) -> Result<PushOutput, PushError> {
    validate_push_args(&args)?;

    let current_branch = match Head::current().await {
        Head::Branch(name) => name,
        Head::Detached(_) => return Err(PushError::DetachedHead),
    };
    let repository = match args.repository.clone() {
        Some(repo) => repo,
        None => {
            let remote = ConfigKv::get_remote(&current_branch).await.ok().flatten();
            match remote {
                Some(remote) => remote,
                None => return Err(PushError::NoRemoteConfigured),
            }
        }
    };

    let repo_url = match ConfigKv::get_remote_url(&repository).await {
        Ok(url) => url,
        Err(_) => {
            // Cross-Cutting F: fuzzy match for remote name suggestion
            let suggestion = suggest_remote_name(&repository).await;
            return Err(PushError::RemoteNotFound {
                name: repository.clone(),
                suggestion,
            });
        }
    };

    // Local file path remotes are not supported for push
    if is_local_file_remote(&repo_url) {
        return Err(PushError::UnsupportedLocalFileRemote);
    }

    validate_local_refspecs(&args, &current_branch).await?;

    // Determine transport: SSH or HTTPS
    let is_ssh = is_ssh_spec(&repo_url);

    let remote_client =
        RemoteClient::from_spec_with_remote(&repo_url, Some(&repository)).map_err(|e| {
            PushError::InvalidRemoteUrl {
                url: repo_url.clone(),
                detail: e.to_string(),
            }
        })?;
    let remote_client = remote_client
        .with_network_timeouts(PUSH_CONNECT_TIMEOUT, PUSH_IDLE_TIMEOUT)
        .map_err(|e| PushError::Network(format!("failed to configure remote transport: {e}")))?;

    let discovery = tokio::time::timeout(
        PUSH_CONNECT_TIMEOUT,
        remote_client.discovery_reference(ReceivePack),
    )
    .await
    .map_err(|_| PushError::Timeout {
        phase: "discovery".to_string(),
        seconds: PUSH_CONNECT_TIMEOUT.as_secs(),
    })?
    .map_err(|e| match e {
        GitError::UnAuthorized(_) => PushError::AuthenticationFailed {
            url: repo_url.clone(),
        },
        GitError::NetworkError(detail) => {
            let lower = detail.to_lowercase();
            if lower.contains("timeout") || lower.contains("timed out") {
                PushError::Timeout {
                    phase: "discovery".to_string(),
                    seconds: PUSH_CONNECT_TIMEOUT.as_secs(),
                }
            } else {
                PushError::DiscoveryFailed {
                    url: repo_url.clone(),
                    detail,
                }
            }
        }
        other => PushError::DiscoveryFailed {
            url: repo_url.clone(),
            detail: other.to_string(),
        },
    })?;

    let local_kind = get_hash_kind();
    if discovery.hash_kind != local_kind {
        return Err(PushError::HashKindMismatch {
            remote: discovery.hash_kind.to_string(),
            local: local_kind.to_string(),
        });
    }
    set_wire_hash_kind(discovery.hash_kind);
    let remote_refs = remote_ref_map(&discovery.refs);

    let mut warnings = Vec::new();
    // `--force-with-lease` permits a non-fast-forward update into the plan; the
    // lease check below is the real gate (so the plain non-ff rejection in
    // `add_update_ref_plan` does not pre-empt it).
    let effective_force = args.force || args.force_with_lease.is_some();
    let mut plans = if args.mirror {
        build_mirror_update_plan(&remote_refs, &mut warnings).await?
    } else {
        let mut plans = Vec::new();
        let mut seen_remote_refs = HashSet::new();

        if args.refspecs.is_empty() && !args.tags {
            let tracked_ref = ConfigKv::get(&format!("branch.{current_branch}.merge"))
                .await
                .ok()
                .flatten()
                .map(|e| e.value)
                .unwrap_or_else(|| format!("refs/heads/{current_branch}"));
            add_update_ref_plan(
                resolve_local_ref(&current_branch).await?,
                tracked_ref,
                &remote_refs,
                effective_force,
                &mut warnings,
                &mut seen_remote_refs,
                &mut plans,
            )?;
        }

        for refspec in &args.refspecs {
            match parse_refspec(refspec)? {
                ParsedRefspec::Update { src, dst } => {
                    let local_ref = resolve_local_ref(&src).await?;
                    let remote_ref = normalize_destination_ref(&dst, local_ref.kind)?;
                    add_update_ref_plan(
                        local_ref,
                        remote_ref,
                        &remote_refs,
                        effective_force,
                        &mut warnings,
                        &mut seen_remote_refs,
                        &mut plans,
                    )?;
                }
                ParsedRefspec::Delete { dst } => {
                    let remote_ref = normalize_delete_ref(&dst)?;
                    add_delete_ref_plan(
                        remote_ref,
                        &remote_refs,
                        &mut seen_remote_refs,
                        &mut plans,
                    )?;
                }
            }
        }

        if args.tags {
            add_all_tag_update_plans(
                &remote_refs,
                effective_force,
                &mut warnings,
                &mut seen_remote_refs,
                &mut plans,
            )
            .await?;
        }

        if args.follow_tags {
            // Tips of the refs already scheduled for update drive reachability.
            let pushed_tips: Vec<ObjectHash> = plans
                .iter()
                .filter(|plan| plan.update.kind == PushRefUpdateKind::Update)
                .filter_map(|plan| ObjectHash::from_str(&plan.update.new_oid).ok())
                .collect();
            for tag_ref in collect_follow_tag_refs(&pushed_tips, &remote_refs).await? {
                let remote_ref = tag_ref.full_ref.clone();
                add_update_ref_plan(
                    tag_ref,
                    remote_ref,
                    &remote_refs,
                    effective_force,
                    &mut warnings,
                    &mut seen_remote_refs,
                    &mut plans,
                )?;
            }
        }

        plans
    };

    validate_set_upstream_plan(&args, &plans)?;
    let upstream_plan = if args.set_upstream && !args.dry_run {
        plans.first().cloned()
    } else {
        None
    };
    plans.retain(|plan| plan.update.old_oid.as_deref() != Some(&plan.update.new_oid));

    if plans.is_empty() {
        let upstream_set = if let Some(plan) = upstream_plan.as_ref() {
            Some(set_upstream_from_push_plan(&repository, plan, output).await?)
        } else {
            None
        };
        return Ok(PushOutput {
            remote: repository.clone(),
            url: repo_url,
            updates: vec![],
            objects_pushed: 0,
            bytes_pushed: 0,
            lfs_files_uploaded: 0,
            lfs_upload: LfsUploadSummary { files_uploaded: 0 },
            dry_run: args.dry_run,
            up_to_date: true,
            upstream_set,
            warnings,
        });
    }

    // `--force-with-lease`: validate the lease against the server's advertised
    // OIDs (from discovery) before collecting objects, encoding a pack, uploading
    // LFS, or sending anything. A failed lease aborts with no remote/local change.
    if let Some(lease) = parse_force_with_lease(&args.force_with_lease) {
        let tracking = collect_lease_tracking_oids(&repository, &plans).await;
        validate_force_with_lease(&lease, &plans, &tracking)?;
        // `--force-if-includes` (lore.md 2.10): a lease REFINEMENT — beyond
        // "the remote still points where my tracking ref says", require the
        // tracking tip to have been INTEGRATED locally (reachable from a
        // local reflog entry of the pushed branch, per Git's semantics).
        // Active only for the All/Ref lease forms; with the Exact form (or
        // no lease at all) it is Git's documented silent no-op.
        if args.force_if_includes && !matches!(lease, ForceWithLease::Exact { .. }) {
            validate_force_if_includes(&lease, &plans, &tracking).await?;
        }
    }

    let obj_result = collect_push_objects(&plans).await?;
    let objs = obj_result.objs;
    warnings.extend(obj_result.warnings);
    let obj_count = objs.len();

    // Dry-run: compute what would be pushed but do not send
    if args.dry_run {
        return Ok(PushOutput {
            remote: repository.clone(),
            url: repo_url,
            updates: plans.iter().map(|plan| plan.update.clone()).collect(),
            objects_pushed: obj_count,
            bytes_pushed: 0,
            lfs_files_uploaded: 0,
            lfs_upload: LfsUploadSummary { files_uploaded: 0 },
            dry_run: true,
            up_to_date: false,
            upstream_set: None,
            warnings,
        });
    }

    // `--atomic` / `-o` / `--signed`: only advertise these capabilities if the
    // remote supports them, otherwise refuse before sending anything (Git).
    let use_atomic = resolve_atomic_capability(args.atomic, &discovery.capabilities)?;
    let use_push_options =
        resolve_push_options_capability(&args.push_option, &discovery.capabilities)?;
    let push_cert_nonce = resolve_push_cert_nonce(args.signed, &discovery.capabilities)?;

    let mut data = BytesMut::new();
    let mut capabilities = vec!["report-status"];
    if get_wire_hash_kind() == HashKind::Sha256 {
        capabilities.push("object-format=sha256");
    }
    if use_atomic {
        capabilities.push("atomic");
    }
    if use_push_options {
        capabilities.push("push-options");
    }
    if push_cert_nonce.is_some() {
        capabilities.push("push-cert");
    }
    let capability = capabilities.join(" ");
    let zero_oid = ObjectHash::zero_str(get_hash_kind());

    // Build the `<old> <new> <ref>` command tuples shared by both wire forms.
    let commands: Vec<(String, String, String)> = plans
        .iter()
        .map(|plan| {
            let old_oid = plan
                .update
                .old_oid
                .as_deref()
                .unwrap_or(&zero_oid)
                .to_string();
            let new_oid = match plan.update.kind {
                PushRefUpdateKind::Update => plan.update.new_oid.clone(),
                PushRefUpdateKind::Delete => zero_oid.clone(),
            };
            (old_oid, new_oid, plan.update.remote_ref.clone())
        })
        .collect();

    if let Some(nonce) = push_cert_nonce {
        // Signed push: build, GPG-sign, and frame a push certificate.
        let identity = crate::command::commit::resolve_committer_identity()
            .await
            .map_err(|e| PushError::PushSignFailed(e.to_string()))?;
        let pusher = format!(
            "{} <{}> {} +0000",
            identity.name,
            identity.email,
            chrono::Utc::now().timestamp()
        );
        let certificate = build_push_certificate(&pusher, &repo_url, &nonce, &commands);
        let unseal_key = crate::internal::vault::load_unseal_key()
            .await
            .ok_or(PushError::PushSignNoKey)?;
        let sig_hex = crate::internal::vault::pgp_sign(
            &crate::utils::util::storage_path(),
            &unseal_key,
            certificate.as_bytes(),
        )
        .await
        .map_err(|e| PushError::PushSignFailed(e.to_string()))?;
        let armored = crate::internal::vault::signature_to_armored(&sig_hex)
            .map_err(|e| PushError::PushSignFailed(e.to_string()))?;
        encode_push_cert_section(&capability, &certificate, &armored, &mut data);
    } else {
        for (index, (old_oid, new_oid, remote_ref)) in commands.iter().enumerate() {
            let suffix = if index == 0 {
                format!("\0{capability}")
            } else {
                String::new()
            };
            add_pkt_line_string(
                &mut data,
                format!("{old_oid} {new_oid} {remote_ref}{suffix}\n"),
            );
        }
        data.extend_from_slice(b"0000");
        // Push-options section follows the command flush, before the pack.
        if use_push_options {
            encode_push_options(&args.push_option, &mut data);
        }
    }
    tracing::debug!("{:?}", data);

    // Upload LFS files (only for HTTP remotes)
    let mut lfs_files_uploaded = 0;
    if !is_ssh && !objs.is_empty() {
        let url = Url::parse(&repo_url).map_err(|e| PushError::InvalidRemoteUrl {
            url: repo_url.clone(),
            detail: e.to_string(),
        })?;
        let lfs_client = LFSClient::from_url(&url);
        lfs_files_uploaded =
            lfs_client
                .push_objects(&objs)
                .await
                .map_err(|error| PushError::LfsUploadFailed {
                    path: error.path.unwrap_or_else(|| "(unknown)".to_string()),
                    oid: error.oid.unwrap_or_else(|| "(unknown)".to_string()),
                    detail: error.detail,
                })?;
    }

    let mut pack_data = Vec::new();
    if !objs.is_empty() && args.thin {
        // `--thin` (lore.md 2.10): REF_DELTA entries against SERVER-KNOWN
        // bases. Base harvesting is deliberately conservative: only blobs
        // modified at the same path between a plan's advertised old tip
        // (non-zero old_oid = the server told us it has it) and the pushed
        // tip qualify. Anything without a net wire win ships full; the
        // receiving `git receive-pack` completes the pack via
        // index-pack --fix-thin / unpack-objects.
        let bases = harvest_thin_bases(&plans, &objs).await;
        let progress_output = progress_output_config(output, args.no_progress);
        let progress = ProgressReporter::new(
            "Compressing objects (thin)",
            Some(objs.len() as u64),
            &progress_output,
        );
        let mut entries = Vec::with_capacity(objs.len());
        let mut delta_count = 0usize;
        for (i, obj) in objs.iter().enumerate() {
            let mut entry = crate::utils::thin_pack::ThinPackEntry {
                hash: obj.hash,
                obj_type: obj.obj_type,
                data: obj.data.clone(),
                delta_base: None,
            };
            // Size caps guard both CPU (byte-level matching) and the wire
            // (correctness margin far below the 16 MiB copy ceiling — our
            // ops are capped at 64 KiB regardless).
            const DELTA_SIZE_CAP: usize = 8 * 1024 * 1024;
            if obj.obj_type == git_internal::internal::object::types::ObjectType::Blob
                && obj.data.len() <= DELTA_SIZE_CAP
                && let Some(base_hash) = bases.get(&obj.hash)
                && let Ok(base_data) = load_object_data(base_hash)
                && base_data.len() <= DELTA_SIZE_CAP
            {
                let delta = crate::utils::thin_pack::delta_encode(&base_data, &obj.data);
                // Net win only: the delta plus the raw base hash must beat
                // the full payload with margin.
                if delta.len() + base_hash.to_data().len() < obj.data.len() * 3 / 4 {
                    entry.data = delta;
                    entry.delta_base = Some(*base_hash);
                    delta_count += 1;
                }
            }
            entries.push(entry);
            progress.tick((i + 1) as u64);
        }
        progress.finish();
        pack_data = crate::utils::thin_pack::encode_thin_pack(&entries)
            .map_err(|e| PushError::PackEncoding(format!("thin pack encoding failed: {e}")))?;
        if delta_count > 0 && !output.quiet {
            info_println!(
                output,
                "thin pack: {delta_count} delta object(s) against server-known bases"
            );
        }
    } else if !objs.is_empty() {
        let (entry_tx, entry_rx) = mpsc::channel::<MetaAttached<Entry, EntryMeta>>(1_000_000);
        let (stream_tx, mut stream_rx) = mpsc::channel(1_000_000);

        let encoder = PackEncoder::new(objs.len(), 0, stream_tx);
        encoder
            .encode_async(entry_rx)
            .await
            .map_err(|e| PushError::PackEncoding(e.to_string()))?;

        let progress_output = progress_output_config(output, args.no_progress);
        let progress = ProgressReporter::new(
            "Compressing objects",
            Some(objs.len() as u64),
            &progress_output,
        );
        for (i, obj) in objs.iter().cloned().enumerate() {
            let meta_entry = MetaAttached {
                inner: obj,
                meta: EntryMeta::default(),
            };
            if let Err(e) = entry_tx.send(meta_entry).await {
                return Err(PushError::PackEncoding(format!(
                    "failed to send entry: {e}"
                )));
            }
            progress.tick((i + 1) as u64);
        }
        drop(entry_tx);
        progress.finish();

        let progress = ProgressReporter::new("Writing objects", None, &progress_output);
        while let Some(chunk) = stream_rx.recv().await {
            pack_data.extend(chunk);
            progress.tick(pack_data.len() as u64);
        }
        progress.finish();
    }

    let bytes_pushed = pack_data.len() as u64;
    data.extend_from_slice(&pack_data);

    // Send pack via the appropriate transport. SSH wraps each read/write/wait
    // operation with the push idle timeout, and HTTPS uses reqwest's
    // connect_timeout + read_timeout.
    match &remote_client {
        RemoteClient::Ssh(ssh_client) => {
            let response_bytes = ssh_client
                .send_pack(data.freeze())
                .await
                .map_err(|e| classify_transport_error("send-pack", e))?;
            validate_receive_pack_response(response_bytes, &plans)?;
        }
        RemoteClient::Http(http_client) => {
            let res = http_client.send_pack(data.freeze()).await.map_err(|e| {
                // reqwest's Display can embed the credentialed remote URL; redact
                // before it reaches an error surfaced to the user (lore.md §0.2).
                classify_transport_error(
                    "send-pack",
                    std::io::Error::other(crate::utils::redact::redact_url_credentials(
                        &e.to_string(),
                    )),
                )
            })?;
            if res.status() != 200 {
                return Err(PushError::Network(format!(
                    "unexpected server response (status {})",
                    res.status()
                )));
            }
            let data = res.bytes().await.map_err(|e| {
                classify_transport_error(
                    "receive-pack",
                    std::io::Error::other(crate::utils::redact::redact_url_credentials(
                        &e.to_string(),
                    )),
                )
            })?;
            validate_receive_pack_response(data, &plans)?;
        }
        _ => {
            return Err(PushError::UnsupportedLocalFileRemote);
        }
    }

    update_remote_tracking_refs(&repository, &plans).await?;

    let upstream_set = if let Some(plan) = upstream_plan.as_ref() {
        Some(set_upstream_from_push_plan(&repository, plan, output).await?)
    } else {
        None
    };

    Ok(PushOutput {
        remote: repository,
        url: repo_url,
        updates: plans.iter().map(|plan| plan.update.clone()).collect(),
        objects_pushed: obj_count,
        bytes_pushed,
        lfs_files_uploaded,
        lfs_upload: LfsUploadSummary {
            files_uploaded: lfs_files_uploaded,
        },
        dry_run: false,
        up_to_date: false,
        upstream_set,
        warnings,
    })
}

fn validate_set_upstream_plan(args: &PushArgs, plans: &[RefUpdatePlan]) -> Result<(), PushError> {
    if !args.set_upstream {
        return Ok(());
    }

    let [plan] = plans else {
        return Err(PushError::InvalidRefspec(
            "--set-upstream requires exactly one branch refspec".to_string(),
        ));
    };
    if plan.local_kind != Some(LocalRefKind::Branch)
        || plan.update.kind != PushRefUpdateKind::Update
    {
        return Err(PushError::InvalidRefspec(
            "--set-upstream only supports branch update refspecs".to_string(),
        ));
    }

    Ok(())
}

async fn set_upstream_from_push_plan(
    repository: &str,
    plan: &RefUpdatePlan,
    output: &OutputConfig,
) -> Result<String, PushError> {
    if plan.local_kind != Some(LocalRefKind::Branch)
        || plan.update.kind != PushRefUpdateKind::Update
    {
        return Err(PushError::InvalidRefspec(
            "--set-upstream only supports branch update refspecs".to_string(),
        ));
    }

    let local_branch = plan
        .update
        .local_ref
        .strip_prefix("refs/heads/")
        .unwrap_or(&plan.update.local_ref);
    let remote_branch = plan
        .update
        .remote_ref
        .strip_prefix("refs/heads/")
        .unwrap_or(&plan.update.remote_ref);
    let upstream = format!("{repository}/{remote_branch}");
    let silent_output = silent_output_config(output);
    branch::set_upstream_safe_with_output(local_branch, &upstream, &silent_output)
        .await
        .map_err(|e| PushError::TrackingRefUpdate(e.message().to_string()))?;
    Ok(upstream)
}

fn remote_ref_map(refs: &[crate::internal::protocol::DiscRef]) -> HashMap<String, String> {
    refs.iter()
        .filter(|reference| !reference._ref.ends_with("^{}"))
        .map(|reference| (reference._ref.clone(), reference._hash.clone()))
        .collect()
}

fn validate_ref_name(refname: &str) -> bool {
    let Some(short) = refname.strip_prefix("refs/") else {
        return false;
    };
    if short.is_empty()
        || short.starts_with('/')
        || short.ends_with('/')
        || short.ends_with('.')
        || short.ends_with(".lock")
        || short.contains("//")
        || short.contains("..")
        || short.contains("@{")
    {
        return false;
    }
    !short.chars().any(|c| {
        c.is_ascii_control()
            || c.is_whitespace()
            || matches!(c, ':' | '\\' | '~' | '^' | '?' | '*' | '[')
    })
}

fn ensure_valid_ref(refname: String, original: &str) -> Result<String, PushError> {
    if validate_ref_name(&refname) {
        Ok(refname)
    } else {
        Err(PushError::InvalidRefspec(original.to_string()))
    }
}

fn normalize_branch_ref(input: &str) -> Result<String, PushError> {
    if input.starts_with("refs/heads/") {
        ensure_valid_ref(input.to_string(), input)
    } else if input.starts_with("refs/") {
        Err(PushError::InvalidRefspec(input.to_string()))
    } else {
        ensure_valid_ref(format!("refs/heads/{input}"), input)
    }
}

fn normalize_tag_ref(input: &str) -> Result<String, PushError> {
    if input.starts_with("refs/tags/") {
        ensure_valid_ref(input.to_string(), input)
    } else if input.starts_with("refs/") {
        Err(PushError::InvalidRefspec(input.to_string()))
    } else {
        ensure_valid_ref(format!("refs/tags/{input}"), input)
    }
}

fn normalize_destination_ref(input: &str, source_kind: LocalRefKind) -> Result<String, PushError> {
    if input.starts_with("refs/") {
        return ensure_valid_ref(input.to_string(), input);
    }
    match source_kind {
        LocalRefKind::Branch => normalize_branch_ref(input),
        LocalRefKind::Tag => normalize_tag_ref(input),
    }
}

fn normalize_delete_ref(input: &str) -> Result<String, PushError> {
    if input.starts_with("refs/") {
        ensure_valid_ref(input.to_string(), input)
    } else {
        normalize_branch_ref(input)
    }
}

async fn resolve_local_ref(input: &str) -> Result<ResolvedLocalRef, PushError> {
    if input.starts_with("refs/heads/") {
        let short_name = input
            .strip_prefix("refs/heads/")
            .unwrap_or(input)
            .to_string();
        return resolve_branch_ref(&short_name, input).await;
    }
    if input.starts_with("refs/tags/") {
        let short_name = input
            .strip_prefix("refs/tags/")
            .unwrap_or(input)
            .to_string();
        return resolve_tag_ref(&short_name, input).await;
    }
    if input.starts_with("refs/") {
        return Err(PushError::InvalidRefspec(input.to_string()));
    }

    if let Some(branch) = Branch::find_branch_result(input, None)
        .await
        .map_err(|error| PushError::RepoState(error.to_string()))?
    {
        return Ok(ResolvedLocalRef {
            full_ref: normalize_branch_ref(input)?,
            oid: branch.commit,
            kind: LocalRefKind::Branch,
        });
    }
    if let Some(target) = tag::find_tag_ref(input)
        .await
        .map_err(|error| PushError::RepoState(error.to_string()))?
        .and_then(|reference| reference.target)
    {
        let oid = ObjectHash::from_str(&target).map_err(|error| {
            PushError::RepoState(format!("invalid tag target '{input}': {error}"))
        })?;
        return Ok(ResolvedLocalRef {
            full_ref: normalize_tag_ref(input)?,
            oid,
            kind: LocalRefKind::Tag,
        });
    }
    Err(PushError::SourceRefNotFound(input.to_string()))
}

async fn resolve_branch_ref(
    short_name: &str,
    original: &str,
) -> Result<ResolvedLocalRef, PushError> {
    let branch = Branch::find_branch_result(short_name, None)
        .await
        .map_err(|error| PushError::RepoState(error.to_string()))?
        .ok_or_else(|| PushError::SourceRefNotFound(original.to_string()))?;
    Ok(ResolvedLocalRef {
        full_ref: normalize_branch_ref(short_name)?,
        oid: branch.commit,
        kind: LocalRefKind::Branch,
    })
}

async fn resolve_tag_ref(short_name: &str, original: &str) -> Result<ResolvedLocalRef, PushError> {
    let target = tag::find_tag_ref(short_name)
        .await
        .map_err(|error| PushError::RepoState(error.to_string()))?
        .and_then(|reference| reference.target)
        .ok_or_else(|| PushError::SourceRefNotFound(original.to_string()))?;
    let oid = ObjectHash::from_str(&target).map_err(|error| {
        PushError::RepoState(format!("invalid tag target '{short_name}': {error}"))
    })?;
    Ok(ResolvedLocalRef {
        full_ref: normalize_tag_ref(short_name)?,
        oid,
        kind: LocalRefKind::Tag,
    })
}

/// One-time warning emitted when `--force-if-includes` is paired with
/// `--force-with-lease` (the flag is accepted but not yet implemented).
/// `--force-if-includes` (lore.md 2.10): for every leased Update plan whose
/// expectation came from a TRACKING ref, require the tracking tip to be
/// integrated locally — the tip must be the plan's new OID, an ancestor of
/// it, or REACHABLE from a reflog entry of the pushed local branch (Git's
/// remote.c contract: reachability, not entry equality — a merged-then-
/// rewound tip still counts as seen). Conservative directions: an empty
/// reflog, an unloadable tip, or no reachability proof all REJECT (never
/// fail-open). Divergences (documented): no timestamp cutoff (we check the
/// whole reflog — strictly a git performance heuristic), and rejection is a
/// whole-push error rather than a per-ref porcelain row.
async fn validate_force_if_includes(
    lease: &ForceWithLease,
    plans: &[RefUpdatePlan],
    tracking: &HashMap<String, Option<String>>,
) -> Result<(), PushError> {
    for plan in plans {
        let remote_ref = &plan.update.remote_ref;
        match lease {
            ForceWithLease::All => {}
            ForceWithLease::Ref(name) if lease_ref_matches(remote_ref, name) => {}
            _ => continue,
        }
        // Delete plans have no local integration question.
        let Some(new_oid) = plan.new_oid else {
            continue;
        };
        let Some(Some(tracking_oid)) = tracking.get(remote_ref) else {
            continue; // no tracking expectation — the lease already handled it
        };
        let Ok(tip) = ObjectHash::from_str(tracking_oid) else {
            return Err(PushError::ForceIfIncludesRejected {
                remote_ref: remote_ref.clone(),
                tracking_oid: tracking_oid.clone(),
            });
        };
        // CONSERVATIVE: the tip must load as a commit before ANY acceptance
        // path (equality included) — a corrupt/partial local store must
        // reject, never fail open (Codex P1).
        if Commit::try_load(&tip).is_none() {
            return Err(PushError::ForceIfIncludesRejected {
                remote_ref: remote_ref.clone(),
                tracking_oid: tracking_oid.clone(),
            });
        }
        // Fast paths: pushing the tip itself, or a descendant of it.
        if tip == new_oid || is_ancestor(&tip, &new_oid) {
            continue;
        }
        // Reflog integration: one reverse walk from the union of the pushed
        // branch's reflog OIDs (new first, then old), shared visited set —
        // the tip must be REACHABLE from some entry, not merely equal.
        let branch_ref = &plan.update.local_ref;
        let db = crate::internal::db::get_db_conn_instance().await;
        let entries = crate::internal::reflog::Reflog::find_all(&db, branch_ref)
            .await
            .unwrap_or_default();
        let mut starts: Vec<ObjectHash> = Vec::new();
        for entry in &entries {
            for oid_text in [&entry.new_oid, &entry.old_oid] {
                if let Ok(oid) = ObjectHash::from_str(oid_text) {
                    starts.push(oid);
                }
            }
        }
        if !reachable_from_any(&tip, &starts) {
            return Err(PushError::ForceIfIncludesRejected {
                remote_ref: remote_ref.clone(),
                tracking_oid: tracking_oid.clone(),
            });
        }
    }
    Ok(())
}

/// Whether `target` is reachable (ancestor-or-equal) from ANY of `starts`,
/// with one shared visited set across the whole walk.
fn reachable_from_any(target: &ObjectHash, starts: &[ObjectHash]) -> bool {
    let mut queue: VecDeque<ObjectHash> = VecDeque::new();
    let mut visited: HashSet<ObjectHash> = HashSet::new();
    for start in starts {
        if visited.insert(*start) {
            queue.push_back(*start);
        }
    }
    while let Some(commit_id) = queue.pop_front() {
        if &commit_id == target {
            return true;
        }
        let Some(commit) = Commit::try_load(&commit_id) else {
            continue;
        };
        for parent in commit.parent_commit_ids {
            if visited.insert(parent) {
                queue.push_back(parent);
            }
        }
    }
    false
}

/// Parsed `--force-with-lease[=<refname>[:<expect>]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ForceWithLease {
    /// Bare `--force-with-lease`: every pushed ref must still match its tracking ref.
    All,
    /// `--force-with-lease=<refname>`: only this ref must match its tracking ref.
    Ref(String),
    /// `--force-with-lease=<refname>:<expect>`: this ref must match an explicit OID.
    Exact { refname: String, expect: String },
}

/// Decode the raw clap value into a [`ForceWithLease`] (None when the flag is absent).
fn parse_force_with_lease(raw: &Option<Option<String>>) -> Option<ForceWithLease> {
    match raw {
        None => None,
        Some(None) => Some(ForceWithLease::All),
        Some(Some(spec)) => match spec.split_once(':') {
            Some((refname, expect)) => Some(ForceWithLease::Exact {
                refname: refname.to_string(),
                expect: expect.to_string(),
            }),
            None => Some(ForceWithLease::Ref(spec.clone())),
        },
    }
}

/// Whether a full remote ref (`refs/heads/main`) is named by a lease `<refname>`
/// (which may be short `main` or fully qualified `refs/heads/main`).
fn lease_ref_matches(remote_ref: &str, name: &str) -> bool {
    remote_ref == name
        || remote_ref.strip_prefix("refs/heads/") == Some(name)
        || remote_ref.strip_prefix("refs/tags/") == Some(name)
}

/// Lease OID equality, tolerant of an abbreviated `<expect>` value (git allows a
/// short hash). `None` means the ref is absent on that side.
fn lease_oid_matches(expected: Option<&str>, server: Option<&str>) -> bool {
    match (expected, server) {
        (None, None) => true,
        (Some(expected), Some(server)) => {
            server == expected || server.starts_with(expected) || expected.starts_with(server)
        }
        _ => false,
    }
}

/// Validate the lease against the build plans: the server's currently-advertised
/// OID (`plan.update.old_oid`) must equal the expected OID (the local tracking
/// ref OID for `All`/`Ref`, or the explicit OID for `Exact`). A mismatch means
/// the remote moved since we last saw it — reject before sending any pack.
///
/// `tracking` maps each plan's remote ref to its local tracking OID (or `None`).
fn validate_force_with_lease(
    lease: &ForceWithLease,
    plans: &[RefUpdatePlan],
    tracking: &HashMap<String, Option<String>>,
) -> Result<(), PushError> {
    for plan in plans {
        let remote_ref = &plan.update.remote_ref;
        let expected: Option<String> = match lease {
            ForceWithLease::All => tracking.get(remote_ref).cloned().flatten(),
            ForceWithLease::Ref(name) => {
                if !lease_ref_matches(remote_ref, name) {
                    continue;
                }
                tracking.get(remote_ref).cloned().flatten()
            }
            ForceWithLease::Exact { refname, expect } => {
                if !lease_ref_matches(remote_ref, refname) {
                    continue;
                }
                Some(expect.clone())
            }
        };
        if !lease_oid_matches(expected.as_deref(), plan.update.old_oid.as_deref()) {
            return Err(PushError::NonFastForward {
                local_ref: plan.update.local_ref.clone(),
                remote_ref: remote_ref.clone(),
            });
        }
    }
    Ok(())
}

/// Read each plan's local remote-tracking ref OID (`refs/remotes/<remote>/<branch>`),
/// used as the expected value for `--force-with-lease` forms ① and ②.
async fn collect_lease_tracking_oids(
    repository: &str,
    plans: &[RefUpdatePlan],
) -> HashMap<String, Option<String>> {
    let mut map = HashMap::new();
    for plan in plans {
        let tracking_oid = if let Some(branch) = plan.update.remote_ref.strip_prefix("refs/heads/")
        {
            // Tracking refs are stored under TWO naming conventions (clone
            // writes short names, fetch writes full refs/remotes paths) —
            // consult both, or a lease after a fetch-only update would see
            // no expectation at all (pre-existing bug surfaced by the 2.10
            // interop tests).
            let short = Branch::find_branch_result(branch, Some(repository))
                .await
                .ok()
                .flatten();
            let full = if short.is_none() {
                Branch::find_branch_result(
                    &format!("refs/remotes/{repository}/{branch}"),
                    Some(repository),
                )
                .await
                .ok()
                .flatten()
            } else {
                None
            };
            short.or(full).map(|b| b.commit.to_string())
        } else {
            None
        };
        map.insert(plan.update.remote_ref.clone(), tracking_oid);
    }
    map
}

fn add_update_ref_plan(
    local_ref: ResolvedLocalRef,
    remote_ref: String,
    remote_refs: &HashMap<String, String>,
    force: bool,
    warnings: &mut Vec<String>,
    seen_remote_refs: &mut HashSet<String>,
    plans: &mut Vec<RefUpdatePlan>,
) -> Result<(), PushError> {
    if !seen_remote_refs.insert(remote_ref.clone()) {
        return Err(PushError::InvalidRefspec(format!(
            "duplicate destination ref '{remote_ref}'"
        )));
    }

    let zero_oid = zero_object_hash();
    let remote_hash = remote_refs
        .get(&remote_ref)
        .cloned()
        .unwrap_or_else(|| ObjectHash::zero_str(get_hash_kind()));
    let old_oid = ObjectHash::from_str(&remote_hash)
        .map_err(|_| PushError::RepoState(format!("invalid remote hash: {remote_hash}")))?;

    let can_update = match local_ref.kind {
        LocalRefKind::Branch => old_oid == zero_oid || is_ancestor(&old_oid, &local_ref.oid),
        LocalRefKind::Tag => old_oid == zero_oid || old_oid == local_ref.oid,
    };
    if !can_update && !force {
        return Err(PushError::NonFastForward {
            local_ref: local_ref.full_ref,
            remote_ref,
        });
    }
    let forced = !can_update && force;
    if forced
        && !warnings
            .iter()
            .any(|w| w == "force push overwrites remote history")
    {
        warnings.push("force push overwrites remote history".to_string());
    }

    plans.push(RefUpdatePlan {
        update: PushRefUpdate {
            kind: PushRefUpdateKind::Update,
            local_ref: local_ref.full_ref,
            remote_ref,
            old_oid: (old_oid != zero_oid).then_some(remote_hash),
            new_oid: local_ref.oid.to_string(),
            forced,
        },
        old_oid,
        new_oid: Some(local_ref.oid),
        local_kind: Some(local_ref.kind),
    });
    Ok(())
}

fn add_delete_ref_plan(
    remote_ref: String,
    remote_refs: &HashMap<String, String>,
    seen_remote_refs: &mut HashSet<String>,
    plans: &mut Vec<RefUpdatePlan>,
) -> Result<(), PushError> {
    if !seen_remote_refs.insert(remote_ref.clone()) {
        return Err(PushError::InvalidRefspec(format!(
            "duplicate destination ref '{remote_ref}'"
        )));
    }
    let Some(remote_hash) = remote_refs.get(&remote_ref).cloned() else {
        return Ok(());
    };
    let old_oid = ObjectHash::from_str(&remote_hash)
        .map_err(|_| PushError::RepoState(format!("invalid remote hash: {remote_hash}")))?;
    plans.push(RefUpdatePlan {
        update: PushRefUpdate {
            kind: PushRefUpdateKind::Delete,
            local_ref: String::new(),
            remote_ref,
            old_oid: Some(remote_hash),
            new_oid: ObjectHash::zero_str(get_hash_kind()),
            forced: false,
        },
        old_oid,
        new_oid: None,
        local_kind: None,
    });
    Ok(())
}

async fn add_all_tag_update_plans(
    remote_refs: &HashMap<String, String>,
    force: bool,
    warnings: &mut Vec<String>,
    seen_remote_refs: &mut HashSet<String>,
    plans: &mut Vec<RefUpdatePlan>,
) -> Result<(), PushError> {
    let tags = tag::list()
        .await
        .map_err(|error| PushError::RepoState(error.to_string()))?;
    for local_tag in tags {
        let oid = tag_object_hash(&local_tag.object);
        let full_ref = normalize_tag_ref(&local_tag.name)?;
        add_update_ref_plan(
            ResolvedLocalRef {
                full_ref: full_ref.clone(),
                oid,
                kind: LocalRefKind::Tag,
            },
            full_ref,
            remote_refs,
            force,
            warnings,
            seen_remote_refs,
            plans,
        )?;
    }
    Ok(())
}

/// Decide whether `--follow-tags` should push a given tag: only annotated tags
/// whose target is reachable from a ref being pushed and which the remote does
/// not already have.
fn follow_tag_should_push(
    is_annotated: bool,
    target_reachable: bool,
    full_ref: &str,
    remote_refs: &HashMap<String, String>,
) -> bool {
    is_annotated && target_reachable && !remote_refs.contains_key(full_ref)
}

/// Collect the annotated tags that `--follow-tags` should add to the push: those
/// whose target commit is reachable from one of `pushed_tips` and that are
/// missing on the remote.
async fn collect_follow_tag_refs(
    pushed_tips: &[ObjectHash],
    remote_refs: &HashMap<String, String>,
) -> Result<Vec<ResolvedLocalRef>, PushError> {
    let tags = tag::list()
        .await
        .map_err(|error| PushError::RepoState(error.to_string()))?;
    let mut selected = Vec::new();
    for local_tag in tags {
        let (is_annotated, target) = match &local_tag.object {
            tag::TagObject::Tag(tag_object) => (true, tag_object.object_hash),
            other => (false, tag_object_hash(other)),
        };
        let target_reachable = pushed_tips.iter().any(|tip| is_ancestor(&target, tip));
        let full_ref = normalize_tag_ref(&local_tag.name)?;
        if follow_tag_should_push(is_annotated, target_reachable, &full_ref, remote_refs) {
            selected.push(ResolvedLocalRef {
                full_ref,
                oid: tag_object_hash(&local_tag.object),
                kind: LocalRefKind::Tag,
            });
        }
    }
    Ok(selected)
}

async fn build_mirror_update_plan(
    remote_refs: &HashMap<String, String>,
    warnings: &mut Vec<String>,
) -> Result<Vec<RefUpdatePlan>, PushError> {
    let mut plans = Vec::new();
    let mut seen_remote_refs = HashSet::new();
    let mut local_refs = HashSet::new();

    let branches = Branch::list_branches_result(None)
        .await
        .map_err(|error| PushError::RepoState(error.to_string()))?;
    for branch in branches {
        let full_ref = normalize_branch_ref(&branch.name)?;
        local_refs.insert(full_ref.clone());
        add_update_ref_plan(
            ResolvedLocalRef {
                full_ref: full_ref.clone(),
                oid: branch.commit,
                kind: LocalRefKind::Branch,
            },
            full_ref,
            remote_refs,
            true,
            warnings,
            &mut seen_remote_refs,
            &mut plans,
        )?;
    }

    let tags = tag::list()
        .await
        .map_err(|error| PushError::RepoState(error.to_string()))?;
    for local_tag in tags {
        let full_ref = normalize_tag_ref(&local_tag.name)?;
        let oid = tag_object_hash(&local_tag.object);
        local_refs.insert(full_ref.clone());
        add_update_ref_plan(
            ResolvedLocalRef {
                full_ref: full_ref.clone(),
                oid,
                kind: LocalRefKind::Tag,
            },
            full_ref,
            remote_refs,
            true,
            warnings,
            &mut seen_remote_refs,
            &mut plans,
        )?;
    }

    for remote_ref in remote_refs.keys() {
        if !(remote_ref.starts_with("refs/heads/") || remote_ref.starts_with("refs/tags/")) {
            continue;
        }
        if local_refs.contains(remote_ref) {
            continue;
        }
        add_delete_ref_plan(
            remote_ref.clone(),
            remote_refs,
            &mut seen_remote_refs,
            &mut plans,
        )?;
    }

    if plans
        .iter()
        .any(|plan| plan.update.kind == PushRefUpdateKind::Delete)
        && !warnings
            .iter()
            .any(|w| w == "mirror push will delete remote-only refs")
    {
        warnings.push("mirror push will delete remote-only refs".to_string());
    }

    Ok(plans)
}

fn tag_object_hash(object: &tag::TagObject) -> ObjectHash {
    match object {
        tag::TagObject::Commit(commit) => commit.id,
        tag::TagObject::Tag(tag) => tag.id,
        tag::TagObject::Tree(tree) => tree.id,
        tag::TagObject::Blob(blob) => blob.id,
    }
}

async fn collect_push_objects(plans: &[RefUpdatePlan]) -> Result<IncrementalObjsResult, PushError> {
    let mut combined = IncrementalObjsResult {
        objs: HashSet::new(),
        warnings: Vec::new(),
    };
    for plan in plans {
        let Some(new_oid) = plan.new_oid else {
            continue;
        };
        let result = collect_objects_for_ref(new_oid, plan.old_oid, plan.local_kind).await?;
        combined.objs.extend(result.objs);
        combined.warnings.extend(result.warnings);
    }
    Ok(combined)
}

async fn collect_objects_for_ref(
    new_oid: ObjectHash,
    old_oid: ObjectHash,
    kind: Option<LocalRefKind>,
) -> Result<IncrementalObjsResult, PushError> {
    match tag::load_object_trait(&new_oid)
        .await
        .map_err(|error| PushError::ObjectCollection(error.to_string()))?
    {
        tag::TagObject::Commit(commit) => {
            let remote_base = if kind == Some(LocalRefKind::Tag) {
                zero_object_hash()
            } else {
                old_oid
            };
            Ok(incremental_objs(commit.id, remote_base))
        }
        tag::TagObject::Tag(tag_object) => collect_tag_object_chain(tag_object).await,
        tag::TagObject::Tree(tree) => {
            let mut warnings = Vec::new();
            Ok(IncrementalObjsResult {
                objs: diff_tree_objs(None, &tree.id, &mut warnings),
                warnings,
            })
        }
        tag::TagObject::Blob(blob) => Ok(IncrementalObjsResult {
            objs: HashSet::from([blob.into()]),
            warnings: Vec::new(),
        }),
    }
}

async fn collect_tag_object_chain(
    mut tag_object: GitTagObject,
) -> Result<IncrementalObjsResult, PushError> {
    let mut result = IncrementalObjsResult {
        objs: HashSet::new(),
        warnings: Vec::new(),
    };
    let mut seen_tags = HashSet::new();

    loop {
        if !seen_tags.insert(tag_object.id) {
            return Err(PushError::ObjectCollection(format!(
                "detected cycle while collecting tag object '{}'",
                tag_object.id
            )));
        }

        let target_oid = tag_object.object_hash;
        result.objs.insert(tag_object.into());

        match tag::load_object_trait(&target_oid)
            .await
            .map_err(|error| PushError::ObjectCollection(error.to_string()))?
        {
            tag::TagObject::Commit(commit) => {
                let commit_result = incremental_objs(commit.id, zero_object_hash());
                result.objs.extend(commit_result.objs);
                result.warnings.extend(commit_result.warnings);
                return Ok(result);
            }
            tag::TagObject::Tree(tree) => {
                result
                    .objs
                    .extend(diff_tree_objs(None, &tree.id, &mut result.warnings));
                return Ok(result);
            }
            tag::TagObject::Blob(blob) => {
                result.objs.insert(blob.into());
                return Ok(result);
            }
            tag::TagObject::Tag(next_tag_object) => {
                tag_object = next_tag_object;
            }
        }
    }
}

fn validate_receive_pack_response(
    mut response_data: Bytes,
    plans: &[RefUpdatePlan],
) -> Result<(), PushError> {
    let (_, pkt_line) = read_pkt_line(&mut response_data);
    if pkt_line != "unpack ok\n" {
        return Err(PushError::RemoteUnpackFailed);
    }

    let expected_refs: HashSet<_> = plans
        .iter()
        .map(|plan| plan.update.remote_ref.as_str())
        .collect();
    let mut seen_refs = HashSet::new();
    loop {
        let (len, pkt_line) = read_pkt_line(&mut response_data);
        if len == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&pkt_line).trim().to_string();
        if let Some(refname) = line.strip_prefix("ok ") {
            seen_refs.insert(refname.to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("ng ") {
            let (refname, reason) = rest
                .split_once(' ')
                .unwrap_or((rest, "remote rejected update"));
            return Err(PushError::RemoteRefUpdateFailed {
                refname: refname.to_string(),
                reason: reason.to_string(),
            });
        }
        return Err(PushError::Network(format!(
            "unexpected receive-pack status line: {line}"
        )));
    }

    for expected in expected_refs {
        if !seen_refs.contains(expected) {
            return Err(PushError::RemoteRefUpdateFailed {
                refname: expected.to_string(),
                reason: "missing status from remote".to_string(),
            });
        }
    }
    Ok(())
}

async fn update_remote_tracking_refs(
    repository: &str,
    plans: &[RefUpdatePlan],
) -> Result<(), PushError> {
    for plan in plans {
        let Some(remote_branch) = plan.update.remote_ref.strip_prefix("refs/heads/") else {
            continue;
        };
        let remote_tracking_branch = format!("refs/remotes/{repository}/{remote_branch}");
        match plan.update.kind {
            PushRefUpdateKind::Update => {
                update_remote_tracking(&remote_tracking_branch, &plan.update.new_oid, repository)
                    .await
                    .map_err(|e| PushError::TrackingRefUpdate(e.message().to_string()))?
            }
            PushRefUpdateKind::Delete => {
                match Branch::delete_branch_result(&remote_tracking_branch, Some(repository)).await
                {
                    Ok(()) | Err(BranchStoreError::NotFound(_)) => {}
                    Err(error) => return Err(PushError::TrackingRefUpdate(error.to_string())),
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render push output according to OutputConfig (human / JSON / machine).
fn render_push_output(
    result: &PushOutput,
    output: &OutputConfig,
    porcelain: bool,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("push", result, output);
    }

    if porcelain {
        return render_push_porcelain(result);
    }

    if result.up_to_date {
        info_println!(output, "Everything up-to-date");
        return Ok(());
    }

    if output.quiet {
        emit_push_warnings(&result.warnings);
        return Ok(());
    }

    let stdout = std::io::stdout();
    let mut w = stdout.lock();

    writeln!(w, "To {}", result.url)
        .map_err(|e| CliError::io(format!("failed to write push output: {e}")))?;

    for update in &result.updates {
        let remote_short_name = update
            .remote_ref
            .strip_prefix("refs/heads/")
            .or_else(|| update.remote_ref.strip_prefix("refs/tags/"))
            .unwrap_or(&update.remote_ref);
        let local_short_name = update
            .local_ref
            .strip_prefix("refs/heads/")
            .or_else(|| update.local_ref.strip_prefix("refs/tags/"))
            .unwrap_or(&update.local_ref);

        if update.kind == PushRefUpdateKind::Delete {
            if result.dry_run {
                writeln!(w, " - [deleted]         {} (dry run)", remote_short_name)
                    .map_err(|e| CliError::io(format!("failed to write push output: {e}")))?;
            } else {
                writeln!(w, " - [deleted]         {}", remote_short_name)
                    .map_err(|e| CliError::io(format!("failed to write push output: {e}")))?;
            }
            continue;
        }

        match &update.old_oid {
            None => {
                let kind_label = if update.remote_ref.starts_with("refs/tags/") {
                    "tag"
                } else {
                    "branch"
                };
                if result.dry_run {
                    writeln!(
                        w,
                        " * [new {kind_label}]      {} -> {} (dry run)",
                        local_short_name, remote_short_name
                    )
                    .map_err(|e| CliError::io(format!("failed to write push output: {e}")))?;
                } else {
                    writeln!(
                        w,
                        " * [new {kind_label}]      {} -> {}",
                        local_short_name, remote_short_name
                    )
                    .map_err(|e| CliError::io(format!("failed to write push output: {e}")))?;
                }
            }
            Some(old_oid) => {
                let old_short = &old_oid[..7.min(old_oid.len())];
                let new_short = &update.new_oid[..7.min(update.new_oid.len())];
                if update.forced {
                    writeln!(
                        w,
                        " + {}...{} {} -> {} (forced update)",
                        old_short, new_short, local_short_name, remote_short_name
                    )
                    .map_err(|e| CliError::io(format!("failed to write push output: {e}")))?;
                } else if result.dry_run {
                    writeln!(
                        w,
                        "   {}..{}  {} -> {} (dry run)",
                        old_short, new_short, local_short_name, remote_short_name
                    )
                    .map_err(|e| CliError::io(format!("failed to write push output: {e}")))?;
                } else {
                    writeln!(
                        w,
                        "   {}..{}  {} -> {}",
                        old_short, new_short, local_short_name, remote_short_name
                    )
                    .map_err(|e| CliError::io(format!("failed to write push output: {e}")))?;
                }
            }
        }
    }

    if result.lfs_files_uploaded > 0 {
        let files_word = if result.lfs_files_uploaded == 1 {
            "file"
        } else {
            "files"
        };
        writeln!(
            w,
            " {} {} changed via LFS",
            result.lfs_files_uploaded, files_word
        )
        .map_err(|e| CliError::io(format!("failed to write push output: {e}")))?;
    }

    if result.objects_pushed > 0 {
        if result.dry_run {
            writeln!(w, " {} objects would be pushed", result.objects_pushed)
                .map_err(|e| CliError::io(format!("failed to write push output: {e}")))?;
        } else {
            let size_str = format_bytes(result.bytes_pushed);
            writeln!(
                w,
                " {} objects pushed ({})",
                result.objects_pushed, size_str
            )
            .map_err(|e| CliError::io(format!("failed to write push output: {e}")))?;
        }
    }

    emit_push_warnings(&result.warnings);

    // Print upstream tracking info
    if let Some(upstream) = &result.upstream_set {
        let branch_name = result
            .updates
            .first()
            .map(|u| {
                u.local_ref
                    .strip_prefix("refs/heads/")
                    .unwrap_or(&u.local_ref)
            })
            .unwrap_or("?");
        writeln!(w, "branch '{}' set up to track '{}'", branch_name, upstream)
            .map_err(|e| CliError::io(format!("failed to write push output: {e}")))?;
    }

    Ok(())
}

/// Render `git push --porcelain`-style output: a `To <url>` header (credential
/// redacted) followed by one tab-separated line per ref
/// (`<flag>\t<from>:<to>\t<summary>`). On the success path every ref was
/// accepted, so rejected (`!`) lines never appear here — failures are reported
/// on stderr via the typed error path.
fn render_push_porcelain(result: &PushOutput) -> CliResult<()> {
    let stdout = std::io::stdout();
    let mut w = stdout.lock();
    let url = crate::command::fetch::redact_url_credentials(&result.url);
    writeln!(w, "To {url}")
        .map_err(|e| CliError::io(format!("failed to write push output: {e}")))?;

    for update in &result.updates {
        let (flag, summary) = porcelain_ref_fields(update);
        let from = if update.kind == PushRefUpdateKind::Delete {
            ""
        } else {
            update.local_ref.as_str()
        };
        writeln!(w, "{flag}\t{from}:{}\t{summary}", update.remote_ref)
            .map_err(|e| CliError::io(format!("failed to write push output: {e}")))?;
    }

    if result.updates.is_empty() {
        writeln!(w, "Done")
            .map_err(|e| CliError::io(format!("failed to write push output: {e}")))?;
    }
    Ok(())
}

/// Compute the porcelain `(flag, summary)` for one ref update. Flags follow
/// `git push --porcelain`: ` ` ok, `+` forced, `-` deleted, `*` new ref,
/// `=` up-to-date.
fn porcelain_ref_fields(update: &PushRefUpdate) -> (char, String) {
    if update.kind == PushRefUpdateKind::Delete {
        return ('-', "[deleted]".to_string());
    }
    match &update.old_oid {
        None => {
            let label = if update.remote_ref.starts_with("refs/tags/") {
                "[new tag]"
            } else {
                "[new branch]"
            };
            ('*', label.to_string())
        }
        Some(old_oid) => {
            let old_short = &old_oid[..7.min(old_oid.len())];
            let new_short = &update.new_oid[..7.min(update.new_oid.len())];
            if update.forced {
                ('+', format!("{old_short}...{new_short} (forced update)"))
            } else {
                (' ', format!("{old_short}..{new_short}"))
            }
        }
    }
}

/// Classify a transport-layer I/O error into a typed `PushError`.
///
/// Transport errors that mention "timed out" (from SSH idle timeout or reqwest
/// read_timeout) are mapped to `PushError::Timeout` with the originating phase.
/// All other errors become `PushError::Network`.
fn classify_transport_error(phase: &str, e: std::io::Error) -> PushError {
    let detail = e.to_string();
    let lower = detail.to_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") {
        PushError::Timeout {
            phase: phase.to_string(),
            seconds: PUSH_IDLE_TIMEOUT.as_secs(),
        }
    } else {
        PushError::Network(format!("{phase} failed: {detail}"))
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn progress_output_config(output: &OutputConfig, no_progress: bool) -> OutputConfig {
    let mut config = output.clone();
    // `--no-progress` (or JSON mode) forces the "Compressing/Writing objects"
    // reporters off, matching `git push --no-progress`.
    if config.is_json() || no_progress {
        config.progress = ProgressMode::None;
        config.progress_preference = crate::utils::output::ProgressPreference::None;
    }
    config
}

fn silent_output_config(output: &OutputConfig) -> OutputConfig {
    let mut config = output.clone();
    config.quiet = true;
    config.progress = ProgressMode::None;
    config.progress_preference = crate::utils::output::ProgressPreference::None;
    config
}

/// Cross-Cutting F: suggest the closest configured remote name using edit distance.
async fn suggest_remote_name(input: &str) -> Option<String> {
    let entries = ConfigKv::get_by_prefix("remote.").await.ok()?;
    let mut names: HashSet<String> = HashSet::new();
    for entry in &entries {
        // Keys look like "remote.origin.url", "remote.origin.fetch", etc.
        if let Some(rest) = entry.key.strip_prefix("remote.")
            && let Some(name) = rest.split('.').next()
        {
            names.insert(name.to_string());
        }
    }
    let mut best: Option<(String, usize)> = None;
    for name in &names {
        let dist = levenshtein(input, name);
        // Only suggest if edit distance is at most 2 and less than the input length
        if dist <= 2 && dist < input.len() && best.as_ref().is_none_or(|(_, d)| dist < *d) {
            best = Some((name.clone(), dist));
        }
    }
    best.map(|(name, _)| name)
}

fn emit_push_warnings(warnings: &[String]) {
    for warning in warnings {
        emit_warning(warning);
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Updates the remote tracking branch reference and records a Push action in the reflog.
///
/// This operation is performed atomically within a database transaction to keep the branch
/// pointer and reflog entry consistent.
async fn update_remote_tracking(
    remote_tracking_branch: &str,
    commit_hash: &str,
    remote_name: &str,
) -> CliResult<()> {
    let remote_tracking_branch = remote_tracking_branch.to_string();
    let commit_hash = commit_hash.to_string();
    let remote_name = remote_name.to_string();
    let remote_tracking_branch_for_error = remote_tracking_branch.clone();

    let db = get_db_conn_instance().await;
    let transaction_result = db
        .transaction(|txn| {
            Box::pin(async move {
                let old_oid = Branch::find_branch_result_with_conn(
                    txn,
                    &remote_tracking_branch,
                    Some(&remote_name),
                )
                .await
                .map_err(|error| {
                    map_update_remote_tracking_branch_error(&remote_tracking_branch, error)
                })?
                .map_or(ObjectHash::zero_str(get_hash_kind()).to_string(), |b| {
                    b.commit.to_string()
                });

                Branch::update_branch_with_conn(
                    txn,
                    &remote_tracking_branch,
                    &commit_hash,
                    Some(&remote_name),
                )
                .await
                .map_err(|source| {
                    CliError::fatal(format!(
                        "failed to update remote tracking branch '{remote_tracking_branch}': {source}"
                    ))
                    .with_stable_code(StableErrorCode::IoWriteFailed)
                })?;

                let context = ReflogContext {
                    old_oid,
                    new_oid: commit_hash.clone(),
                    action: ReflogAction::Push,
                };
                Reflog::insert_single_entry(txn, &context, &remote_tracking_branch)
                    .await
                    .map_err(|source| {
                        CliError::fatal(format!(
                            "failed to update remote tracking branch '{remote_tracking_branch}': {source}"
                        ))
                        .with_stable_code(StableErrorCode::IoWriteFailed)
                    })?;
                Ok::<_, CliError>(())
            })
        })
        .await;

    if let Err(error) = transaction_result {
        return Err(match error {
            TransactionError::Connection(source) => CliError::fatal(format!(
                "failed to update remote tracking branch '{remote_tracking_branch_for_error}': {source}"
            ))
            .with_stable_code(StableErrorCode::IoWriteFailed),
            TransactionError::Transaction(cli) => cli,
        });
    }
    Ok(())
}

fn map_update_remote_tracking_branch_error(
    remote_tracking_branch: &str,
    error: BranchStoreError,
) -> CliError {
    match error {
        BranchStoreError::Query(detail) => CliError::fatal(format!(
            "failed to inspect remote tracking branch '{remote_tracking_branch}': {detail}"
        ))
        .with_stable_code(StableErrorCode::IoReadFailed),
        BranchStoreError::Corrupt { .. } => CliError::fatal(format!(
            "failed to inspect remote tracking branch '{remote_tracking_branch}': {error}"
        ))
        .with_stable_code(StableErrorCode::RepoCorrupt),
        BranchStoreError::NotFound(_) => CliError::fatal(format!(
            "failed to inspect remote tracking branch '{remote_tracking_branch}': {error}"
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid),
        BranchStoreError::Delete { name, detail } => CliError::fatal(format!(
            "failed to inspect remote tracking branch '{name}': {detail}"
        ))
        .with_stable_code(StableErrorCode::IoWriteFailed),
    }
}

fn is_local_file_remote(spec: &str) -> bool {
    if let Ok(url) = Url::parse(spec) {
        if url.scheme() == "file" || url.scheme().len() == 1 {
            return true;
        }
        return false;
    }
    Path::new(spec).exists()
}

/// collect all commits from `commit_id` to root commit
fn collect_history_commits(commit_id: &ObjectHash) -> HashSet<ObjectHash> {
    let zero_oid = zero_object_hash();
    if commit_id == &zero_oid {
        return HashSet::new();
    }

    let mut commits = HashSet::new();
    let mut queue = VecDeque::new();
    commits.insert(*commit_id);
    queue.push_back(*commit_id);
    while let Some(commit) = queue.pop_front() {
        let commit = match Commit::try_load(&commit) {
            Some(c) => c,
            None => continue,
        };

        for parent in commit.parent_commit_ids.iter() {
            // Dedupe at enqueue time: without this, every merge commit
            // re-enqueues its shared ancestry, and a merge-heavy history
            // makes the walk exponential — pushing any merge tip then
            // spins for hours (observed on this repo, 2026-07-05).
            if commits.insert(*parent) {
                queue.push_back(*parent);
            }
        }
    }
    commits
}

/// Collected objects and any warnings emitted during object traversal.
/// Harvest thin-pack delta bases (lore.md 2.10): for each Update plan whose
/// advertised old tip is non-zero (the SERVER declared it during ref
/// discovery — known by construction), diff the old and new trees and map
/// same-path modified blobs `new → old`. Conservative by design: type
/// changes, adds, deletes, and anything not in the outgoing object set are
/// ignored; a failed load just skips the candidate (thin is an optimization,
/// never a failure source).
async fn harvest_thin_bases(
    plans: &[RefUpdatePlan],
    objs: &HashSet<Entry>,
) -> HashMap<ObjectHash, ObjectHash> {
    use crate::utils::object_ext::TreeExt;
    let outgoing: HashSet<ObjectHash> = objs.iter().map(|obj| obj.hash).collect();
    let mut bases = HashMap::new();
    for plan in plans {
        let Some(new_oid) = plan.new_oid else {
            continue;
        };
        let zero = plan.old_oid.to_string().chars().all(|c| c == '0');
        if zero {
            continue; // creating a new ref: the server has nothing yet
        }
        let Some(old_commit) = Commit::try_load(&plan.old_oid) else {
            continue;
        };
        let Some(new_commit) = Commit::try_load(&new_oid) else {
            continue;
        };
        let Some(old_tree) = Tree::try_load(&old_commit.tree_id) else {
            continue;
        };
        let Some(new_tree) = Tree::try_load(&new_commit.tree_id) else {
            continue;
        };
        let old_items: HashMap<std::path::PathBuf, ObjectHash> =
            old_tree.get_plain_items().into_iter().collect();
        for (path, new_blob) in new_tree.get_plain_items() {
            if let Some(old_blob) = old_items.get(&path)
                && *old_blob != new_blob
                && outgoing.contains(&new_blob)
            {
                // First-wins keeps the mapping deterministic.
                bases.entry(new_blob).or_insert(*old_blob);
            }
        }
    }
    bases
}

/// Load a local object's raw data (base blobs for thin deltas).
fn load_object_data(hash: &ObjectHash) -> Result<Vec<u8>, GitError> {
    let storage = crate::utils::client_storage::ClientStorage::init(crate::utils::path::objects());
    storage.get(hash)
}

struct IncrementalObjsResult {
    objs: HashSet<Entry>,
    warnings: Vec<String>,
}

fn incremental_objs(local_ref: ObjectHash, remote_ref: ObjectHash) -> IncrementalObjsResult {
    tracing::debug!("local_ref: {}, remote_ref: {}", local_ref, remote_ref);

    let empty = IncrementalObjsResult {
        objs: HashSet::new(),
        warnings: vec![],
    };
    let mut warnings = Vec::new();

    let zero_oid = zero_object_hash();
    if remote_ref != zero_oid {
        let mut commit = match Commit::try_load(&local_ref) {
            Some(c) => c,
            None => return empty,
        };
        let mut commits = Vec::new();
        let mut ok = true;
        loop {
            commits.push(commit.id);
            if commit.id == remote_ref {
                break;
            }
            if commit.parent_commit_ids.len() != 1 {
                ok = false;
                break;
            }
            commit = match Commit::try_load(&commit.parent_commit_ids[0]) {
                Some(c) => c,
                None => {
                    ok = false;
                    break;
                }
            };
        }
        if ok {
            let mut objs = HashSet::new();
            commits.reverse();
            for i in 0..commits.len() - 1 {
                let old_commit = match Commit::try_load(&commits[i]) {
                    Some(c) => c,
                    None => {
                        tracing::error!(
                            "Commit {} became inaccessible during push (fast-forward object collection)",
                            commits[i]
                        );
                        return empty;
                    }
                };
                let old_tree = old_commit.tree_id;
                let new_commit = match Commit::try_load(&commits[i + 1]) {
                    Some(c) => c,
                    None => {
                        tracing::error!(
                            "Commit {} became inaccessible during push (fast-forward object collection)",
                            commits[i + 1]
                        );
                        return empty;
                    }
                };
                objs.extend(diff_tree_objs(
                    Some(&old_tree),
                    &new_commit.tree_id,
                    &mut warnings,
                ));
                objs.insert(new_commit.into());
            }
            return IncrementalObjsResult { objs, warnings };
        }
    }

    let mut objs = HashSet::new();
    let mut visit = HashSet::new();
    let exist_commits = collect_history_commits(&remote_ref);
    let mut queue = VecDeque::new();
    if !exist_commits.contains(&local_ref) {
        queue.push_back(local_ref);
        visit.insert(local_ref);
    }
    let mut root_commit = None;

    while let Some(commit_id) = queue.pop_front() {
        let commit = match Commit::try_load(&commit_id) {
            Some(c) => c,
            None => continue,
        };
        let parents = &commit.parent_commit_ids;
        if parents.is_empty() {
            if root_commit.is_none() {
                root_commit = Some(commit.id);
            } else if root_commit != Some(commit.id) {
                tracing::warn!("multiple root commits detected during push object collection");
            }
        }
        for parent in parents.iter() {
            let parent_commit = match Commit::try_load(parent) {
                Some(c) => c,
                None => continue,
            };
            let parent_tree = parent_commit.tree_id;
            objs.extend(diff_tree_objs(
                Some(&parent_tree),
                &commit.tree_id,
                &mut warnings,
            ));
            if !exist_commits.contains(parent) && !visit.contains(parent) {
                queue.push_back(*parent);
                visit.insert(*parent);
            }
        }
        objs.insert(commit.into());

        tracing::debug!("counting objects: {}", objs.len());
    }

    // root commit has no parent
    if let Some(root_commit) = root_commit {
        let root_tree = Commit::load(&root_commit).tree_id;
        objs.extend(diff_tree_objs(None, &root_tree, &mut warnings));
    }

    tracing::debug!("counting objects: {} done", objs.len());
    IncrementalObjsResult { objs, warnings }
}

fn zero_object_hash() -> ObjectHash {
    ObjectHash::from_bytes(&vec![0u8; get_hash_kind().size()])
        .expect("zero hash should match hash kind size")
}

/// Check if `ancestor` is an ancestor of `descendant` using breadth-first search.
///
/// Returns `true` if `ancestor` is reachable by traversing parent commits from `descendant`,
/// or if `ancestor` and `descendant` are the same commit. Returns `false` otherwise.
fn is_ancestor(ancestor: &ObjectHash, descendant: &ObjectHash) -> bool {
    if ancestor == descendant {
        return true;
    }

    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();

    queue.push_back(*descendant);
    visited.insert(*descendant);

    while let Some(commit_id) = queue.pop_front() {
        if &commit_id == ancestor {
            return true;
        }

        let commit = match Commit::try_load(&commit_id) {
            Some(c) => c,
            None => continue,
        };

        for parent_id in &commit.parent_commit_ids {
            if parent_id == ancestor {
                return true;
            }
            if !visited.contains(parent_id) {
                visited.insert(*parent_id);
                queue.push_back(*parent_id);
            }
        }
    }

    false
}

/// calc objects that in `new_tree` but not in `old_tree`
///
/// Warnings (e.g. unsupported submodule entries) are collected into `warnings`
/// instead of being emitted directly, so callers can render them under
/// `OutputConfig` control without polluting stderr in JSON/machine mode.
fn diff_tree_objs(
    old_tree: Option<&ObjectHash>,
    new_tree: &ObjectHash,
    warnings: &mut Vec<String>,
) -> HashSet<Entry> {
    let mut objs = HashSet::new();
    if let Some(old_tree) = old_tree
        && old_tree == new_tree
    {
        return objs;
    }

    let new_tree = Tree::load(new_tree);
    objs.insert(new_tree.clone().into());

    let old_items = old_tree
        .map(|tree| {
            Tree::load(tree)
                .tree_items
                .into_iter()
                .map(|item| (item.name, (item.id, item.mode)))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();

    for item in new_tree.tree_items.iter() {
        let old_item = old_items.get(&item.name);
        if old_item.is_some_and(|(old_id, old_mode)| old_id == &item.id && old_mode == &item.mode) {
            continue;
        }

        match item.mode {
            TreeItemMode::Tree => {
                let old_subtree = old_item.and_then(|(old_id, old_mode)| {
                    (*old_mode == TreeItemMode::Tree).then_some(old_id)
                });
                objs.extend(diff_tree_objs(old_subtree, &item.id, warnings));
            }
            TreeItemMode::Commit => {
                warnings.push("submodule is not supported yet".to_string());
            }
            _ => {
                let blob = Blob::load(&item.id);
                objs.insert(blob.into());
            }
        }
    }

    objs
}

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use git_internal::{
        hash::ObjectHash,
        internal::object::{
            blob::Blob,
            commit::Commit,
            tree::{Tree, TreeItem, TreeItemMode},
        },
    };

    use super::*;

    fn save_test_blob(content: &str) -> Blob {
        let blob = Blob::from_content(content);
        crate::command::save_object(&blob, &blob.id).expect("test blob should save");
        blob
    }

    fn save_test_tree(items: Vec<TreeItem>) -> Tree {
        let tree = Tree::from_tree_items(items).expect("test tree should be valid");
        crate::command::save_object(&tree, &tree.id).expect("test tree should save");
        tree
    }

    fn save_test_commit(tree_id: ObjectHash, parents: Vec<ObjectHash>, message: &str) -> Commit {
        let commit = Commit::from_tree_id(tree_id, parents, message);
        crate::command::save_object(&commit, &commit.id).expect("test commit should save");
        commit
    }

    fn test_ref_update_plan(remote_ref: &str) -> RefUpdatePlan {
        let old_oid = ObjectHash::from_str("1111111111111111111111111111111111111111")
            .expect("test old oid should parse");
        let new_oid = ObjectHash::from_str("2222222222222222222222222222222222222222")
            .expect("test new oid should parse");
        RefUpdatePlan {
            update: PushRefUpdate {
                kind: PushRefUpdateKind::Update,
                local_ref: remote_ref.to_string(),
                remote_ref: remote_ref.to_string(),
                old_oid: Some(old_oid.to_string()),
                new_oid: new_oid.to_string(),
                forced: false,
            },
            old_oid,
            new_oid: Some(new_oid),
            local_kind: Some(LocalRefKind::Branch),
        }
    }

    /// A plan whose server-advertised OID (`update.old_oid`) is controllable, for
    /// force-with-lease unit tests.
    fn lease_plan(remote_ref: &str, server_oid: Option<&str>) -> RefUpdatePlan {
        let placeholder = ObjectHash::from_str("3333333333333333333333333333333333333333")
            .expect("placeholder oid should parse");
        RefUpdatePlan {
            update: PushRefUpdate {
                kind: PushRefUpdateKind::Update,
                local_ref: remote_ref.to_string(),
                remote_ref: remote_ref.to_string(),
                old_oid: server_oid.map(str::to_string),
                new_oid: "4444444444444444444444444444444444444444".to_string(),
                forced: true,
            },
            old_oid: placeholder,
            new_oid: None,
            local_kind: Some(LocalRefKind::Branch),
        }
    }

    #[test]
    fn force_with_lease_parses_bare_ref_and_exact() {
        assert_eq!(parse_force_with_lease(&None), None);
        assert_eq!(
            parse_force_with_lease(&Some(None)),
            Some(ForceWithLease::All)
        );
        assert_eq!(
            parse_force_with_lease(&Some(Some("main".to_string()))),
            Some(ForceWithLease::Ref("main".to_string()))
        );
        assert_eq!(
            parse_force_with_lease(&Some(Some("main:a1b2c3d".to_string()))),
            Some(ForceWithLease::Exact {
                refname: "main".to_string(),
                expect: "a1b2c3d".to_string(),
            })
        );
    }

    #[test]
    fn force_with_lease_conflicts_with_force() {
        use clap::Parser as _;
        assert!(
            PushArgs::try_parse_from(["push", "--force", "--force-with-lease", "origin", "main"])
                .is_err(),
            "--force and --force-with-lease must conflict at the clap layer"
        );
    }

    #[test]
    fn force_with_lease_does_not_swallow_positionals() {
        use clap::Parser as _;
        let args = PushArgs::try_parse_from(["push", "--force-with-lease", "origin", "main"])
            .expect("bare --force-with-lease origin main should parse");
        assert!(!args.force);
        assert_eq!(args.force_with_lease, Some(None));
        assert_eq!(args.repository.as_deref(), Some("origin"));
        assert_eq!(args.refspecs, vec!["main".to_string()]);

        let exact = PushArgs::try_parse_from(["push", "--force-with-lease=main:abc", "origin"])
            .expect("--force-with-lease=main:abc origin should parse");
        assert_eq!(exact.force_with_lease, Some(Some("main:abc".to_string())));
        assert_eq!(exact.repository.as_deref(), Some("origin"));
    }

    #[test]
    fn force_with_lease_all_matches_and_rejects() {
        let mut tracking = HashMap::new();
        tracking.insert("refs/heads/main".to_string(), Some("aaaa".to_string()));

        // server still at the OID we last tracked → ok
        let ok = vec![lease_plan("refs/heads/main", Some("aaaa"))];
        assert!(validate_force_with_lease(&ForceWithLease::All, &ok, &tracking).is_ok());

        // server moved on → reject
        let moved = vec![lease_plan("refs/heads/main", Some("bbbb"))];
        assert!(matches!(
            validate_force_with_lease(&ForceWithLease::All, &moved, &tracking),
            Err(PushError::NonFastForward { .. })
        ));
    }

    #[test]
    fn force_with_lease_exact_uses_expect_oid() {
        let full = "abcdef1234567890abcdef1234567890abcdef12";
        let plans = vec![lease_plan("refs/heads/main", Some(full))];
        let tracking = HashMap::new(); // ignored for Exact

        let good = ForceWithLease::Exact {
            refname: "main".to_string(),
            expect: "abcdef1".to_string(), // abbreviated, matches by prefix
        };
        assert!(validate_force_with_lease(&good, &plans, &tracking).is_ok());

        let bad = ForceWithLease::Exact {
            refname: "main".to_string(),
            expect: "9999999".to_string(),
        };
        assert!(validate_force_with_lease(&bad, &plans, &tracking).is_err());
    }

    #[test]
    fn force_with_lease_ref_only_checks_named_ref() {
        // `main` would mismatch but is not named; only `dev` is checked.
        let plans = vec![
            lease_plan("refs/heads/main", Some("bbbb")),
            lease_plan("refs/heads/dev", Some("dddd")),
        ];
        let mut tracking = HashMap::new();
        tracking.insert("refs/heads/main".to_string(), Some("aaaa".to_string()));
        tracking.insert("refs/heads/dev".to_string(), Some("dddd".to_string()));
        assert!(
            validate_force_with_lease(&ForceWithLease::Ref("dev".to_string()), &plans, &tracking)
                .is_ok()
        );
    }

    #[test]
    fn porcelain_ref_fields_flags() {
        let new_branch = PushRefUpdate {
            kind: PushRefUpdateKind::Update,
            local_ref: "refs/heads/main".to_string(),
            remote_ref: "refs/heads/main".to_string(),
            old_oid: None,
            new_oid: "2222222222222222222222222222222222222222".to_string(),
            forced: false,
        };
        assert_eq!(porcelain_ref_fields(&new_branch).0, '*');

        let forced = PushRefUpdate {
            old_oid: Some("1111111111111111111111111111111111111111".to_string()),
            forced: true,
            ..new_branch.clone()
        };
        let (flag, summary) = porcelain_ref_fields(&forced);
        assert_eq!(flag, '+');
        assert!(summary.contains("forced update"));

        let fast_forward = PushRefUpdate {
            old_oid: Some("1111111111111111111111111111111111111111".to_string()),
            forced: false,
            ..new_branch.clone()
        };
        assert_eq!(porcelain_ref_fields(&fast_forward).0, ' ');

        let deleted = PushRefUpdate {
            kind: PushRefUpdateKind::Delete,
            ..new_branch
        };
        assert_eq!(porcelain_ref_fields(&deleted).0, '-');
    }

    fn receive_pack_response(lines: &[&str]) -> Bytes {
        let mut bytes = BytesMut::new();
        for line in lines {
            add_pkt_line_string(&mut bytes, (*line).to_string());
        }
        bytes.extend_from_slice(b"0000");
        bytes.freeze()
    }

    #[tokio::test]
    async fn incremental_objs_fast_forward_skips_unchanged_subtree_blobs() {
        let repo = tempfile::tempdir().expect("repo tempdir should be created");
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;
        let _guard = crate::utils::test::ChangeDirGuard::new(repo.path());

        let existing_blob = save_test_blob("existing file content");
        let added_blob = save_test_blob("ignore rule\n");
        let old_subtree = save_test_tree(vec![TreeItem::new(
            TreeItemMode::Blob,
            existing_blob.id,
            "keep.txt".to_string(),
        )]);
        let new_subtree = save_test_tree(vec![
            TreeItem::new(
                TreeItemMode::Blob,
                added_blob.id,
                ".libraignore".to_string(),
            ),
            TreeItem::new(TreeItemMode::Blob, existing_blob.id, "keep.txt".to_string()),
        ]);
        let old_root = save_test_tree(vec![TreeItem::new(
            TreeItemMode::Tree,
            old_subtree.id,
            "moon".to_string(),
        )]);
        let new_root = save_test_tree(vec![TreeItem::new(
            TreeItemMode::Tree,
            new_subtree.id,
            "moon".to_string(),
        )]);
        let old_commit = save_test_commit(old_root.id, vec![], "initial");
        let new_commit = save_test_commit(new_root.id, vec![old_commit.id], "add ignore");

        let result = incremental_objs(new_commit.id, old_commit.id);
        let hashes = result
            .objs
            .iter()
            .map(|entry| entry.hash)
            .collect::<HashSet<_>>();

        assert!(result.warnings.is_empty());
        assert!(hashes.contains(&new_commit.id));
        assert!(hashes.contains(&new_root.id));
        assert!(hashes.contains(&new_subtree.id));
        assert!(hashes.contains(&added_blob.id));
        assert!(
            !hashes.contains(&existing_blob.id),
            "fast-forward push must not repack unchanged blobs inside changed subtrees"
        );
        assert_eq!(hashes.len(), 4);
    }

    /// Regression (2026-07-05): `collect_history_commits` enqueued parents
    /// without a visited check, so every merge commit re-enqueued its
    /// shared ancestry — a merge-heavy history made the walk exponential
    /// and pushing any merge tip effectively hung forever. 40 stacked
    /// merges are infeasible pre-fix (~2^40 queue operations) and
    /// instantaneous post-fix.
    #[tokio::test]
    #[serial_test::serial]
    async fn collect_history_commits_dedupes_merge_heavy_history() {
        let repo = tempfile::tempdir().expect("repo tempdir should be created");
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;
        let _guard = crate::utils::test::ChangeDirGuard::new(repo.path());

        let blob = save_test_blob("shared content");
        let tree = save_test_tree(vec![TreeItem::new(
            TreeItemMode::Blob,
            blob.id,
            "file.txt".to_string(),
        )]);

        let root = save_test_commit(tree.id, vec![], "root");
        let mut tip = root.id;
        let mut expected = 1usize;
        for i in 0..40 {
            let side = save_test_commit(tree.id, vec![tip], &format!("side {i}"));
            let merge = save_test_commit(tree.id, vec![tip, side.id], &format!("merge {i}"));
            tip = merge.id;
            expected += 2;
        }

        let commits = collect_history_commits(&tip);
        assert_eq!(commits.len(), expected, "every commit visited exactly once");
        assert!(commits.contains(&root.id));
        assert!(commits.contains(&tip));
    }

    #[tokio::test]
    async fn diff_tree_objs_recurses_by_path_for_changed_subtrees() {
        let repo = tempfile::tempdir().expect("repo tempdir should be created");
        crate::utils::test::setup_with_new_libra_in(repo.path()).await;
        let _guard = crate::utils::test::ChangeDirGuard::new(repo.path());

        let existing_blob = save_test_blob("existing file content");
        let added_blob = save_test_blob("ignore rule\n");
        let old_subtree = save_test_tree(vec![TreeItem::new(
            TreeItemMode::Blob,
            existing_blob.id,
            "keep.txt".to_string(),
        )]);
        let new_subtree = save_test_tree(vec![
            TreeItem::new(
                TreeItemMode::Blob,
                added_blob.id,
                ".libraignore".to_string(),
            ),
            TreeItem::new(TreeItemMode::Blob, existing_blob.id, "keep.txt".to_string()),
        ]);
        let old_root = save_test_tree(vec![TreeItem::new(
            TreeItemMode::Tree,
            old_subtree.id,
            "moon".to_string(),
        )]);
        let new_root = save_test_tree(vec![TreeItem::new(
            TreeItemMode::Tree,
            new_subtree.id,
            "moon".to_string(),
        )]);

        let mut warnings = Vec::new();
        let objs = diff_tree_objs(Some(&old_root.id), &new_root.id, &mut warnings);
        let hashes = objs.iter().map(|entry| entry.hash).collect::<HashSet<_>>();

        assert!(warnings.is_empty());
        assert!(hashes.contains(&new_root.id));
        assert!(hashes.contains(&new_subtree.id));
        assert!(hashes.contains(&added_blob.id));
        assert!(
            !hashes.contains(&existing_blob.id),
            "unchanged blobs inside a changed subtree must not be repacked"
        );
        assert_eq!(hashes.len(), 3);
    }

    /// Pin the `Display` format for the static-message and direct-message
    /// variants of [`PushError`]. These strings are used as the
    /// `CliError` message via `From<PushError> for CliError` and
    /// surface in both human and `--json` envelopes.
    ///
    /// Source-chained variants (ObjectCollection, PackEncoding, Network,
    /// TrackingRefUpdate, RepoState) wrap upstream error strings via `{0}`
    /// and are intentionally skipped — their content is owned by the
    /// wrapped error type.
    #[test]
    fn push_error_display_pins_static_message_variants() {
        assert_eq!(
            PushError::DetachedHead.to_string(),
            "HEAD is detached; cannot determine what to push",
        );
        assert_eq!(
            PushError::NoRemoteConfigured.to_string(),
            "no configured push destination",
        );
        assert_eq!(
            PushError::RemoteNotFound {
                name: "upstream".to_string(),
                suggestion: None,
            }
            .to_string(),
            "remote 'upstream' not found",
        );
        assert_eq!(
            PushError::InvalidRefspec("@invalid".to_string()).to_string(),
            "invalid refspec '@invalid'",
        );
        assert_eq!(
            PushError::InvalidArguments("bad push arguments".to_string()).to_string(),
            "bad push arguments",
        );
        assert_eq!(
            PushError::SourceRefNotFound("topic/x".to_string()).to_string(),
            "source ref 'topic/x' not found",
        );
        assert_eq!(
            PushError::UnsupportedLocalFileRemote.to_string(),
            "pushing to local file repositories is not supported",
        );
        assert_eq!(
            PushError::InvalidRemoteUrl {
                url: "ftp://example.com/repo".to_string(),
                detail: "unsupported scheme".to_string(),
            }
            .to_string(),
            "invalid remote URL 'ftp://example.com/repo': unsupported scheme",
        );
        assert_eq!(
            PushError::AuthenticationFailed {
                url: "https://example.com/repo".to_string(),
            }
            .to_string(),
            "authentication failed for 'https://example.com/repo'",
        );
        assert_eq!(
            PushError::DiscoveryFailed {
                url: "https://example.com/repo".to_string(),
                detail: "timed out".to_string(),
            }
            .to_string(),
            "failed to discover references from 'https://example.com/repo': timed out",
        );
        assert_eq!(
            PushError::Timeout {
                phase: "fetch-refs".to_string(),
                seconds: 30,
            }
            .to_string(),
            "network timeout during fetch-refs after 30s",
        );
        assert_eq!(
            PushError::NonFastForward {
                local_ref: "refs/heads/main".to_string(),
                remote_ref: "refs/heads/main".to_string(),
            }
            .to_string(),
            "cannot push to 'refs/heads/main': non-fast-forward update",
        );
        assert_eq!(
            PushError::HashKindMismatch {
                remote: "sha1".to_string(),
                local: "sha256".to_string(),
            }
            .to_string(),
            "remote object format 'sha1' does not match local 'sha256'",
        );
        assert_eq!(
            PushError::RemoteUnpackFailed.to_string(),
            "remote rejected push: unpack failed",
        );
        assert_eq!(
            PushError::RemoteRefUpdateFailed {
                refname: "refs/heads/main".to_string(),
                reason: "non-fast-forward".to_string(),
            }
            .to_string(),
            "remote rejected ref update for 'refs/heads/main': non-fast-forward",
        );
        assert_eq!(
            PushError::LfsUploadFailed {
                path: "src/big.bin".to_string(),
                oid: "abc123".to_string(),
                detail: "remote did not provide an upload action".to_string(),
            }
            .to_string(),
            "LFS upload failed for 'src/big.bin': remote did not provide an upload action",
        );
    }

    #[test]
    fn validate_receive_pack_response_accepts_all_expected_ref_statuses() {
        let plans = vec![
            test_ref_update_plan("refs/heads/main"),
            test_ref_update_plan("refs/heads/release"),
        ];
        let response = receive_pack_response(&[
            "unpack ok\n",
            "ok refs/heads/main\n",
            "ok refs/heads/release\n",
        ]);

        validate_receive_pack_response(response, &plans).expect("all ref statuses should pass");
    }

    #[test]
    fn validate_receive_pack_response_reports_remote_ng_status() {
        let plans = vec![test_ref_update_plan("refs/heads/main")];
        let response = receive_pack_response(&[
            "unpack ok\n",
            "ng refs/heads/main protected branch hook declined\n",
        ]);

        assert!(matches!(
            validate_receive_pack_response(response, &plans),
            Err(PushError::RemoteRefUpdateFailed { refname, reason })
                if refname == "refs/heads/main" && reason == "protected branch hook declined"
        ));
    }

    #[test]
    fn validate_receive_pack_response_rejects_missing_expected_ref_status() {
        let plans = vec![
            test_ref_update_plan("refs/heads/main"),
            test_ref_update_plan("refs/heads/release"),
        ];
        let response = receive_pack_response(&["unpack ok\n", "ok refs/heads/main\n"]);

        assert!(matches!(
            validate_receive_pack_response(response, &plans),
            Err(PushError::RemoteRefUpdateFailed { refname, reason })
                if refname == "refs/heads/release" && reason == "missing status from remote"
        ));
    }

    #[test]
    fn validate_receive_pack_response_rejects_unexpected_status_line() {
        let plans = vec![test_ref_update_plan("refs/heads/main")];
        let response = receive_pack_response(&["unpack ok\n", "ready refs/heads/main\n"]);

        assert!(matches!(
            validate_receive_pack_response(response, &plans),
            Err(PushError::Network(message))
                if message == "unexpected receive-pack status line: ready refs/heads/main"
        ));
    }

    #[test]
    /// Tests successful parsing of push command arguments with different parameter combinations.
    fn test_parse_args_success() {
        let args = vec!["push"];
        let args = PushArgs::parse_from(args);
        assert_eq!(args.repository, None);
        assert!(args.refspecs.is_empty());
        assert!(!args.set_upstream);
        assert!(!args.force);
        assert!(!args.dry_run);
        assert!(!args.tags);
        assert!(!args.mirror);

        let args = vec!["push", "origin", "master"];
        let args = PushArgs::parse_from(args);
        assert_eq!(args.repository, Some("origin".to_string()));
        assert_eq!(args.refspecs, vec!["master".to_string()]);
        assert!(!args.set_upstream);
        assert!(!args.force);
        assert!(!args.dry_run);

        let args = vec!["push", "-u", "origin", "master"];
        let args = PushArgs::parse_from(args);
        assert_eq!(args.repository, Some("origin".to_string()));
        assert_eq!(args.refspecs, vec!["master".to_string()]);
        assert!(args.set_upstream);
        assert!(!args.force);

        let args = vec!["push", "--force", "origin", "master"];
        let args = PushArgs::parse_from(args);
        assert_eq!(args.repository, Some("origin".to_string()));
        assert_eq!(args.refspecs, vec!["master".to_string()]);
        assert!(!args.set_upstream);
        assert!(args.force);
        assert!(!args.dry_run);

        let args = vec!["push", "-f", "origin", "master"];
        let args = PushArgs::parse_from(args);
        assert_eq!(args.repository, Some("origin".to_string()));
        assert_eq!(args.refspecs, vec!["master".to_string()]);
        assert!(!args.set_upstream);
        assert!(args.force);
        assert!(!args.dry_run);

        let args = vec!["push", "origin", "master", "feature:release"];
        let args = PushArgs::parse_from(args);
        assert_eq!(args.repository, Some("origin".to_string()));
        assert_eq!(
            args.refspecs,
            vec!["master".to_string(), "feature:release".to_string()]
        );

        let args = vec!["push", "--tags", "origin"];
        let args = PushArgs::parse_from(args);
        assert_eq!(args.repository, Some("origin".to_string()));
        assert!(args.refspecs.is_empty());
        assert!(args.tags);

        let args = vec!["push", "--mirror", "--dry-run", "origin"];
        let args = PushArgs::parse_from(args);
        assert_eq!(args.repository, Some("origin".to_string()));
        assert!(args.refspecs.is_empty());
        assert!(args.mirror);
        assert!(args.dry_run);
    }

    #[test]
    /// Tests parsing of --dry-run/-n argument for push command.
    fn test_parse_dry_run_args() {
        let args = vec!["push", "--dry-run", "origin", "master"];
        let args = PushArgs::parse_from(args);
        assert!(args.dry_run);
        assert_eq!(args.repository, Some("origin".to_string()));
        assert_eq!(args.refspecs, vec!["master".to_string()]);

        let args = vec!["push", "-n", "origin", "master"];
        let args = PushArgs::parse_from(args);
        assert!(args.dry_run);

        let args = vec!["push", "--dry-run"];
        let args = PushArgs::parse_from(args);
        assert!(args.dry_run);
        assert_eq!(args.repository, None);

        let args = vec!["push", "-n", "-f", "origin", "master"];
        let args = PushArgs::parse_from(args);
        assert!(args.dry_run);
        assert!(args.force);

        let args = vec!["push", "--dry-run", "--force", "-u", "origin", "master"];
        let args = PushArgs::parse_from(args);
        assert!(args.dry_run);
        assert!(args.force);
        assert!(args.set_upstream);
    }

    #[test]
    /// Tests failure cases for push command argument parsing.
    fn test_parse_args_fail() {
        let args = vec!["push", "-u"];
        let args = PushArgs::try_parse_from(args);
        assert!(args.is_err());
    }

    #[test]
    fn apply_delete_flag_rewrites_and_validates() {
        // Without --delete, the refspecs pass through unchanged.
        assert_eq!(
            apply_delete_flag(vec!["main".to_string()], false, false, false, false).unwrap(),
            vec!["main".to_string()]
        );
        // --delete rewrites each plain ref name to a `:<ref>` deletion request.
        assert_eq!(
            apply_delete_flag(
                vec!["main".to_string(), "feature".to_string()],
                true,
                false,
                false,
                false
            )
            .unwrap(),
            vec![":main".to_string(), ":feature".to_string()]
        );
        // --delete requires at least one ref.
        assert!(apply_delete_flag(vec![], true, false, false, false).is_err());
        // --delete rejects a refspec that already carries a ':'.
        assert!(apply_delete_flag(vec!["a:b".to_string()], true, false, false, false).is_err());
        // --delete cannot combine with --set-upstream / --tags / --mirror.
        assert!(apply_delete_flag(vec!["main".to_string()], true, true, false, false).is_err());
        assert!(apply_delete_flag(vec!["main".to_string()], true, false, true, false).is_err());
        assert!(apply_delete_flag(vec!["main".to_string()], true, false, false, true).is_err());
    }

    #[test]
    fn test_validate_push_args_rejects_invalid_combinations() {
        let args = PushArgs::parse_from(["push", "origin"]);
        assert!(matches!(
            validate_push_args(&args),
            Err(PushError::InvalidArguments(message))
                if message == "repository-only push requires a refspec, --tags, or --mirror"
        ));

        let args = PushArgs::parse_from(["push", "-u", "origin", "main", "topic"]);
        assert!(matches!(
            validate_push_args(&args),
            Err(PushError::InvalidArguments(message))
                if message == "--set-upstream requires exactly one branch refspec"
        ));

        let args = PushArgs::parse_from(["push", "-u", "--tags", "origin", "main"]);
        assert!(matches!(
            validate_push_args(&args),
            Err(PushError::InvalidArguments(message))
                if message == "--set-upstream requires exactly one branch refspec"
        ));

        let args = PushArgs::parse_from(["push", "--mirror", "--tags", "origin"]);
        assert!(matches!(
            validate_push_args(&args),
            Err(PushError::InvalidArguments(message))
                if message == "--mirror cannot be combined with refspecs, --tags, or --set-upstream"
        ));

        let args = PushArgs {
            repository: None,
            refspecs: vec!["main".to_string()],
            set_upstream: false,
            force: false,
            delete: false,
            force_with_lease: None,
            force_if_includes: false,
            thin: false,
            no_thin: false,
            atomic: false,
            push_option: Vec::new(),
            follow_tags: false,
            signed: false,
            porcelain: false,
            dry_run: false,
            tags: false,
            mirror: false,
            no_verify: false,
            no_progress: false,
        };
        assert!(matches!(
            validate_push_args(&args),
            Err(PushError::InvalidArguments(message))
                if message == "repository is required when specifying refspecs, --tags, or --mirror"
        ));
    }

    #[test]
    fn atomic_capability_resolution() {
        // Not requested: never advertised, regardless of server support.
        assert!(!resolve_atomic_capability(false, &[]).unwrap());
        assert!(!resolve_atomic_capability(false, &["atomic".to_string()]).unwrap());
        // Requested and advertised by the server: include the capability.
        assert!(
            resolve_atomic_capability(true, &["report-status".to_string(), "atomic".to_string()])
                .unwrap()
        );
        // Requested but unsupported: refuse up-front.
        assert!(matches!(
            resolve_atomic_capability(true, &["report-status".to_string()]),
            Err(PushError::AtomicUnsupported)
        ));
    }

    #[test]
    fn push_options_capability_resolution() {
        // No options: never advertised.
        assert!(!resolve_push_options_capability(&[], &["push-options".to_string()]).unwrap());
        // Options + server support: advertise.
        assert!(
            resolve_push_options_capability(
                &["ci.skip".to_string()],
                &["report-status".to_string(), "push-options".to_string()]
            )
            .unwrap()
        );
        // Options + no support: refuse up-front.
        assert!(matches!(
            resolve_push_options_capability(
                &["ci.skip".to_string()],
                &["report-status".to_string()]
            ),
            Err(PushError::PushOptionsUnsupported)
        ));
    }

    #[test]
    fn push_options_section_encoding() {
        let mut data = BytesMut::new();
        encode_push_options(
            &["ci.skip".to_string(), "notify=false".to_string()],
            &mut data,
        );
        let bytes = &data[..];
        assert!(
            bytes.ends_with(b"0000"),
            "push-options section ends with a flush-pkt"
        );
        assert!(
            bytes.windows(7).any(|w| w == b"ci.skip"),
            "first option is present in the encoded section"
        );
        assert!(
            bytes.windows(12).any(|w| w == b"notify=false"),
            "second option is present in the encoded section"
        );
    }

    #[test]
    fn follow_tags_selection_predicate() {
        let mut remote = HashMap::new();
        remote.insert("refs/tags/v1".to_string(), "abc".to_string());
        // Annotated, reachable, and absent on the remote: push it.
        assert!(follow_tag_should_push(true, true, "refs/tags/v2", &remote));
        // Lightweight tags are never followed.
        assert!(!follow_tag_should_push(
            false,
            true,
            "refs/tags/v2",
            &remote
        ));
        // Target not reachable from a pushed ref: skip.
        assert!(!follow_tag_should_push(
            true,
            false,
            "refs/tags/v2",
            &remote
        ));
        // Already present on the remote: skip.
        assert!(!follow_tag_should_push(true, true, "refs/tags/v1", &remote));
    }

    #[test]
    fn push_cert_nonce_resolution() {
        // Not signing: never resolves a nonce.
        assert_eq!(
            resolve_push_cert_nonce(false, &["push-cert=abc".to_string()]).unwrap(),
            None
        );
        // Signing + server nonce: extract it.
        assert_eq!(
            resolve_push_cert_nonce(true, &["push-cert=abc123".to_string()]).unwrap(),
            Some("abc123".to_string())
        );
        // Signing + bare push-cert: empty (stateless) nonce.
        assert_eq!(
            resolve_push_cert_nonce(true, &["push-cert".to_string()]).unwrap(),
            Some(String::new())
        );
        // Signing + no support: refuse up-front.
        assert!(matches!(
            resolve_push_cert_nonce(true, &["report-status".to_string()]),
            Err(PushError::PushSignUnsupported)
        ));
    }

    #[test]
    fn push_certificate_payload_and_framing() {
        let commands = vec![(
            "old1".to_string(),
            "new1".to_string(),
            "refs/heads/main".to_string(),
        )];
        let cert = build_push_certificate(
            "A U Thor <a@e> 1000000000 +0000",
            "https://example.com/r.git",
            "nonceXYZ",
            &commands,
        );
        assert_eq!(
            cert,
            "certificate version 0.1\n\
pusher A U Thor <a@e> 1000000000 +0000\n\
pushee https://example.com/r.git\n\
nonce nonceXYZ\n\
\n\
old1 new1 refs/heads/main\n"
        );

        let mut data = BytesMut::new();
        encode_push_cert_section(
            "report-status push-cert",
            &cert,
            "-----BEGIN PGP SIGNATURE-----\nSIGDATA\n-----END PGP SIGNATURE-----",
            &mut data,
        );
        let bytes = &data[..];
        assert!(
            bytes.windows(9).any(|w| w == b"push-cert"),
            "section announces push-cert"
        );
        assert!(
            bytes.windows(13).any(|w| w == b"push-cert-end"),
            "section ends with push-cert-end"
        );
        assert!(
            bytes.windows(7).any(|w| w == b"SIGDATA"),
            "armored signature is embedded"
        );
        assert!(bytes.ends_with(b"0000"), "section flushed before the pack");
    }

    #[test]
    fn test_is_ancestor() {
        let commit_id = ObjectHash::from_str("a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0").unwrap();
        assert!(is_ancestor(&commit_id, &commit_id));
    }

    #[test]
    fn test_parse_refspec_simple_name() {
        let parsed = parse_refspec("main").unwrap();
        assert_eq!(
            parsed,
            ParsedRefspec::Update {
                src: "main".to_string(),
                dst: "main".to_string()
            }
        );
    }

    #[test]
    fn test_parse_refspec_src_dst() {
        let parsed = parse_refspec("local_branch:release").unwrap();
        assert_eq!(
            parsed,
            ParsedRefspec::Update {
                src: "local_branch".to_string(),
                dst: "release".to_string()
            }
        );
    }

    #[test]
    fn test_parse_refspec_delete_dst() {
        let parsed = parse_refspec(":release").unwrap();
        assert_eq!(
            parsed,
            ParsedRefspec::Delete {
                dst: "release".to_string()
            }
        );
    }

    #[test]
    fn test_parse_refspec_empty_rejected() {
        assert!(parse_refspec("").is_err());
    }

    #[test]
    fn test_parse_refspec_empty_src_rejected() {
        assert!(matches!(
            parse_refspec(":dst"),
            Ok(ParsedRefspec::Delete { dst }) if dst == "dst"
        ));
    }

    #[test]
    fn test_parse_refspec_empty_dst_rejected() {
        assert!(parse_refspec("src:").is_err());
    }

    #[test]
    fn test_parse_refspec_multi_colon_rejected() {
        assert!(parse_refspec("a:b:c").is_err());
        assert!(parse_refspec("a::b").is_err());
        assert!(parse_refspec(":a:b").is_err());
    }

    #[test]
    fn test_normalize_destination_ref_accepts_private_refs_namespace() {
        let remote_ref =
            normalize_destination_ref("refs/libra/traces", LocalRefKind::Branch).unwrap();
        assert_eq!(remote_ref, "refs/libra/traces");
    }

    #[test]
    fn test_normalize_branch_ref_still_rejects_private_refs_source() {
        assert!(matches!(
            normalize_branch_ref("refs/libra/traces"),
            Err(PushError::InvalidRefspec(refspec)) if refspec == "refs/libra/traces"
        ));
    }

    #[test]
    fn test_push_error_to_cli_error_detached_head() {
        let err: CliError = PushError::DetachedHead.into();
        assert_eq!(err.stable_code(), StableErrorCode::RepoStateInvalid);
        assert_eq!(err.exit_code(), 128);
    }

    #[test]
    fn test_push_error_to_cli_error_no_remote() {
        let err: CliError = PushError::NoRemoteConfigured.into();
        assert_eq!(err.stable_code(), StableErrorCode::RepoStateInvalid);
        assert_eq!(err.exit_code(), 128);
        assert!(!err.hints().is_empty());
    }

    #[test]
    fn test_push_error_to_cli_error_invalid_refspec() {
        let err: CliError = PushError::InvalidRefspec(":bad".to_string()).into();
        assert_eq!(err.stable_code(), StableErrorCode::CliInvalidArguments);
        assert_eq!(err.exit_code(), 129);
    }

    #[test]
    fn test_push_error_to_cli_error_non_fast_forward() {
        let err: CliError = PushError::NonFastForward {
            local_ref: "main".to_string(),
            remote_ref: "refs/heads/main".to_string(),
        }
        .into();
        assert_eq!(err.stable_code(), StableErrorCode::ConflictOperationBlocked);
        assert_eq!(err.exit_code(), 128);
    }

    #[test]
    fn test_push_error_to_cli_error_auth_failed() {
        let err: CliError = PushError::AuthenticationFailed {
            url: "https://example.com".to_string(),
        }
        .into();
        assert_eq!(err.stable_code(), StableErrorCode::AuthMissingCredentials);
    }

    #[test]
    fn test_push_error_to_cli_error_timeout() {
        let err: CliError = PushError::Timeout {
            phase: "discovery".to_string(),
            seconds: PUSH_CONNECT_TIMEOUT.as_secs(),
        }
        .into();
        assert_eq!(err.stable_code(), StableErrorCode::NetworkUnavailable);
    }

    #[test]
    fn transport_timeout_uses_push_idle_timeout() {
        let err = classify_transport_error(
            "send-pack",
            std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out"),
        );
        assert!(matches!(
            err,
            PushError::Timeout { phase, seconds }
                if phase == "send-pack" && seconds == PUSH_IDLE_TIMEOUT.as_secs()
        ));
    }

    #[test]
    fn test_push_error_to_cli_error_source_ref_not_found() {
        let err: CliError = PushError::SourceRefNotFound("missing-branch".to_string()).into();
        assert_eq!(err.stable_code(), StableErrorCode::CliInvalidTarget);
        assert_eq!(err.exit_code(), 129);
    }

    #[test]
    fn test_push_error_to_cli_error_unsupported_local_remote() {
        let err: CliError = PushError::UnsupportedLocalFileRemote.into();
        assert_eq!(err.stable_code(), StableErrorCode::CliInvalidTarget);
    }

    #[test]
    fn test_push_error_to_cli_error_remote_not_found() {
        let err: CliError = PushError::RemoteNotFound {
            name: "upstream".to_string(),
            suggestion: None,
        }
        .into();
        assert_eq!(err.stable_code(), StableErrorCode::CliInvalidTarget);
        assert_eq!(err.exit_code(), 129);
    }

    #[test]
    fn test_push_error_to_cli_error_remote_not_found_with_suggestion() {
        let err: CliError = PushError::RemoteNotFound {
            name: "origni".to_string(),
            suggestion: Some("origin".to_string()),
        }
        .into();
        assert_eq!(err.stable_code(), StableErrorCode::CliInvalidTarget);
        assert!(
            err.hints()
                .iter()
                .any(|h| h.as_str().contains("did you mean"))
        );
    }

    #[test]
    fn test_push_error_to_cli_error_object_collection_has_issue_url() {
        let err: CliError = PushError::ObjectCollection("test failure".to_string()).into();
        assert_eq!(err.stable_code(), StableErrorCode::InternalInvariant);
        assert!(err.hints().iter().any(|h| h.as_str().contains("issues")));
    }

    #[test]
    fn test_push_error_to_cli_error_pack_encoding_has_issue_url() {
        let err: CliError = PushError::PackEncoding("test failure".to_string()).into();
        assert_eq!(err.stable_code(), StableErrorCode::InternalInvariant);
        assert!(err.hints().iter().any(|h| h.as_str().contains("issues")));
    }

    #[test]
    fn test_map_update_remote_tracking_branch_error_query() {
        let err = map_update_remote_tracking_branch_error(
            "refs/remotes/origin/main",
            BranchStoreError::Query("database is locked".to_string()),
        );
        assert_eq!(err.stable_code(), StableErrorCode::IoReadFailed);
    }

    #[test]
    fn test_map_update_remote_tracking_branch_error_corrupt() {
        let err = map_update_remote_tracking_branch_error(
            "refs/remotes/origin/main",
            BranchStoreError::Corrupt {
                name: "refs/remotes/origin/main".to_string(),
                detail: "invalid object id".to_string(),
            },
        );
        assert_eq!(err.stable_code(), StableErrorCode::RepoCorrupt);
    }

    #[test]
    fn test_levenshtein_basic() {
        assert_eq!(levenshtein("origin", "origin"), 0);
        assert_eq!(levenshtein("origni", "origin"), 2);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
    }
}
