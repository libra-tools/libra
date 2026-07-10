//! Commit command that collects staged changes, builds tree and commit objects, validates messages (including GPG), and updates HEAD/refs.

use std::{
    collections::HashSet,
    io::{IsTerminal, Write},
    path::PathBuf,
    process::{Command, Stdio},
    str::FromStr,
};

use chrono::DateTime;
use clap::Parser;
use git_internal::{
    hash::{ObjectHash, get_hash_kind},
    internal::{
        index::{Index, IndexEntry},
        object::{
            ObjectTrait,
            blob::Blob,
            commit::Commit,
            signature::{Signature, SignatureType},
            tree::{Tree, TreeItem, TreeItemMode},
            types::ObjectType,
        },
    },
};
use sea_orm::ConnectionTrait;
use serde::Serialize;

use crate::{
    command::{diff, editor, load_object, save_object_to_storage, status},
    common_utils::{check_conventional_commits_message, format_commit_msg, parse_commit_msg},
    internal::{
        ai::automation::{VCS_EVENT_POST_COMMIT, dispatch_current_repo_vcs_event_to_history},
        branch::Branch,
        config::{
            LocalIdentityTarget, env_first_non_empty, read_cascaded_config_value,
            resolve_user_identity_sources,
        },
        head::Head,
        log::date_parser::parse_date,
        reflog::{ReflogAction, ReflogContext, with_reflog},
        tree_plumbing,
    },
    utils::{
        client_storage::ClientStorage,
        error::{CliError, CliResult, StableErrorCode},
        lfs,
        object_ext::BlobExt,
        output::{OutputConfig, emit_json_data},
        path, util,
    },
};

/// Create a new commit from staged changes.
///
/// See `libra commit --help` for the same examples rendered through clap.
// GitHub Issues URL surfaced on internal-invariant bug paths
// (`CommitError::TreeCreation`) so users can report unexpected
// tree-build failures. Mirrors push.rs / tag.rs's hint pattern per
// Cross-Cutting G.
const ISSUE_URL: &str = "https://github.com/libra-tools/libra/issues";

/// `--help` examples shown in `libra commit --help` output.
///
/// Per `docs/development/commands/commit.md`, the commit command exposes nine
/// representative scenarios so users see the most common invocations
/// without having to read the doc. Keep this list and the rustdoc
/// snippet in `commit.md` in sync.
pub const COMMIT_EXAMPLES: &str = "\
EXAMPLES:
    libra commit -m 'Add new feature'                Create a commit with message
    libra commit -m 'feat: add login' --conventional Validate conventional commit format
    libra commit --amend                             Amend the last commit
    libra commit --amend --no-edit                   Amend without changing the message
    libra commit -a -m 'Fix typo'                    Auto-stage tracked changes and commit
    libra commit -F message.txt                      Read commit message from file
    libra commit -t template.txt                     Seed the message from a template file
    libra commit -s -m 'Add feature'                 Add Signed-off-by trailer
    libra commit -e -m 'Draft'                       Edit the message in $EDITOR before committing
    libra commit -v                                  Show the staged diff in the editor template
    libra commit --allow-empty -m 'Trigger CI'       Create an empty commit
    libra commit --json -m 'Add feature'             Structured JSON output for agents";

#[derive(Parser, Debug, Default)]
#[command(after_help = COMMIT_EXAMPLES)]
pub struct CommitArgs {
    /// Commit message body. When omitted (and no other source), the editor is
    /// opened on an interactive terminal; otherwise the commit aborts.
    #[arg(short, long)]
    pub message: Option<String>,

    /// read message from file
    #[arg(short = 'F', long)]
    pub file: Option<String>,

    /// Use the contents of FILE as the initial commit message (seeds the editor,
    /// or is used directly with --no-edit), matching `git commit -t`. Falls back
    /// to the `commit.template` config when unset. Ignored when a message source
    /// (-m/-F/-C/-c/--fixup/--squash) is given.
    #[arg(short = 't', long = "template", value_name = "FILE")]
    pub template: Option<String>,

    /// allow commit with empty index
    #[arg(long)]
    pub allow_empty: bool,

    /// check if the commit message follows conventional commits
    #[arg(long)]
    pub conventional: bool,

    /// amend the last commit
    #[arg(long)]
    pub amend: bool,

    /// Reuse the existing message (the amend parent's, or the one from -m/-F)
    /// without opening the editor.
    #[arg(long, conflicts_with = "edit")]
    pub no_edit: bool,
    /// add signed-off-by line at the end of the commit message
    #[arg(short = 's', long)]
    pub signoff: bool,

    /// Skip pre-commit hooks for this invocation (narrower than --no-verify, which also skips commit-msg hooks)
    #[arg(long)]
    pub disable_pre: bool,

    /// Automatically stage tracked files that have been modified or deleted
    #[arg(short = 'a', long)]
    pub all: bool,

    /// Skip all pre-commit and commit-msg hooks/validations (align with Git --no-verify)
    #[arg(long = "no-verify")]
    pub no_verify: bool,

    /// Override the commit author. Specify an explicit author using the standard A U Thor <author@example.com> format.
    #[arg(long)]
    pub author: Option<String>,

    /// Override the author date. Accepts Git raw dates (`<timestamp> <tz>`), RFC 3339, `YYYY-MM-DD`, or a Unix timestamp.
    #[arg(long, value_name = "DATE")]
    pub date: Option<String>,

    /// Create a fixup commit targeting the specified commit. The message becomes "fixup! <subject>".
    #[arg(long, value_name = "COMMIT", conflicts_with_all = ["message", "file", "squash", "reuse_message", "reedit_message"])]
    pub fixup: Option<String>,

    /// Create a squash commit targeting the specified commit. The message becomes "squash! <subject>".
    #[arg(long, value_name = "COMMIT", conflicts_with_all = ["message", "file", "fixup", "reuse_message", "reedit_message"])]
    pub squash: Option<String>,

    /// Clean up the commit message according to the given mode: strip (default), whitespace, verbatim, scissors, or default.
    #[arg(long, value_name = "MODE")]
    pub cleanup: Option<CleanupMode>,

    /// Do not actually create the commit; show what would be committed.
    #[arg(long)]
    pub dry_run: bool,

    /// Add a trailer line to the commit message. Can be given multiple times.
    #[arg(long = "trailer", value_name = "TRAILER")]
    pub trailers: Vec<String>,

    /// Reuse the message from the specified commit.
    #[arg(short = 'C', long = "reuse-message", value_name = "COMMIT", conflicts_with_all = ["message", "file", "fixup", "squash"])]
    pub reuse_message: Option<String>,

    /// Reuse and edit the message from the specified commit.
    #[arg(short = 'c', long = "reedit-message", value_name = "COMMIT", conflicts_with_all = ["message", "file", "fixup", "squash"])]
    pub reedit_message: Option<String>,

    /// Reset the author of the commit to the current user identity.
    #[arg(long)]
    pub reset_author: bool,

    /// Open the editor to edit the commit message even when -m/-F/-C is given.
    #[arg(short = 'e', long = "edit")]
    pub edit: bool,

    /// Show a diff of staged changes in the editor template or on stderr.
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,

    /// Print the working-tree status in machine-readable porcelain v1 format
    /// instead of the human commit summary (mirrors `git commit --porcelain`).
    #[arg(long)]
    pub porcelain: bool,

    /// Include the working-tree status as commented lines in the commit-message
    /// editor template (Git shows this by default; Libra defaults to omitting
    /// it, so `--status` opts in). The status lines are `#`-commented and so are
    /// stripped from the final message — informational only. Seeded only when an
    /// editor opens and the effective cleanup strips comments (`strip`/`default`);
    /// it is omitted under `--cleanup=verbatim`/`whitespace`/`scissors` (which keep
    /// `#` lines above the marker). `-v` only truncates the appended diff and does
    /// NOT force a strip, so the status stays omitted under those modes even with
    /// `-v`, and never leaks into the message. Toggle pair with `--no-status`; the
    /// last one wins.
    #[arg(long = "status", overrides_with = "no_status")]
    pub status: bool,

    /// Do not include the status in the commit-message editor template (Libra's
    /// default). Accepted for Git parity. Toggle pair with `--status`; the last
    /// one wins.
    #[arg(long = "no-status", overrides_with = "status")]
    pub no_status: bool,

    /// Force an unsigned commit: skip Libra's vault GPG signing
    /// (`vault_sign_commit`) for this commit, matching `git commit
    /// --no-gpg-sign`. Vault signing runs when `vault.signing=true` (the `libra
    /// init` default) and a vault unseal key is available; `--no-gpg-sign`
    /// suppresses it regardless, so it is a no-op only when signing would not
    /// have happened anyway. (Git's positive `-S`/`--gpg-sign` is not exposed.)
    #[arg(long = "no-gpg-sign")]
    pub no_gpg_sign: bool,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum CleanupMode {
    /// Strip leading/trailing empty lines and trailing whitespace from every line, then strip commentary lines.
    #[default]
    Strip,
    /// Same as strip but leave consecutive empty lines.
    Whitespace,
    /// Do not change the message at all.
    Verbatim,
    /// Same as strip but truncate the message at the scissors line.
    Scissors,
    /// Same as strip if the message is to be edited, otherwise whitespace.
    Default,
}

// ---------------------------------------------------------------------------
// Structured error types
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    /// The `lfs.lockEnforce` gate refused the commit (lore.md 2.8); the
    /// carried [`CliError`] already has its stable code and hints.
    #[error("{0}")]
    LockPolicy(crate::utils::error::CliError),
    #[error("failed to load index: {0}")]
    IndexLoad(String),

    #[error("failed to save index: {0}")]
    IndexSave(String),

    #[error("nothing to commit, working tree clean")]
    NothingToCommit,

    #[error("nothing to commit (create/copy files and use 'libra add' to track)")]
    NothingToCommitNoTracked,

    #[error("{0}")]
    IdentityMissing(String),

    #[error("there is no commit to amend")]
    NoCommitToAmend,

    #[error("amend is not supported for merge commits with multiple parents")]
    AmendUnsupported,

    #[error("invalid author format: {0}")]
    InvalidAuthor(String),

    #[error("invalid {date_source} date '{value}': {detail}")]
    InvalidDate {
        date_source: &'static str,
        value: String,
        detail: String,
    },

    #[error("failed to read message file '{path}': {detail}")]
    MessageFileRead { path: String, detail: String },

    #[error("could not read commit template '{path}': {detail}")]
    TemplateRead { path: String, detail: String },

    #[error("aborting commit; you did not edit the message")]
    TemplateUnedited,

    #[error("aborting commit due to empty commit message")]
    EmptyMessage,

    #[error("failed to create tree: {0}")]
    TreeCreation(String),

    #[error("index object validation failed: {0}")]
    IndexObjectInvalid(String),

    #[error("failed to store commit object: {0}")]
    ObjectStorage(String),

    #[error("failed to load parent commit '{commit_id}': {detail}")]
    ParentCommitLoad { commit_id: String, detail: String },

    #[error("failed to update HEAD: {0}")]
    HeadUpdate(String),

    #[error("pre-commit hook failed: {0}")]
    PreCommitHook(String),

    #[error("conventional commit validation failed: {0}")]
    ConventionalCommit(String),

    #[error("failed to sign commit: {0}")]
    VaultSign(String),

    #[error("failed to auto-stage tracked changes: {0}")]
    AutoStage(String),

    #[error("failed to calculate staged changes: {0}")]
    StagedChanges(String),

    #[error("{0}")]
    EditorFailed(String),

    #[error("{0}")]
    InvalidConfig(String),
}

impl From<CommitError> for CliError {
    fn from(error: CommitError) -> Self {
        match &error {
            CommitError::LockPolicy(inner) => inner.clone(),
            CommitError::IndexLoad(..) => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint("the index file may be corrupted; try 'libra status' to verify"),
            CommitError::IndexSave(..) => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoWriteFailed)
            }
            CommitError::NothingToCommit => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("use 'libra add' to stage changes")
                .with_hint("use 'libra status' to see what changed"),
            CommitError::NothingToCommitNoTracked => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("create/copy files and use 'libra add' to track"),
            CommitError::IdentityMissing(..) => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::AuthMissingCredentials)
                .with_hint("run 'libra config --global user.name \"Your Name\"' and 'libra config --global user.email \"you@example.com\"'")
                .with_hint("omit '--global' to set the identity only in this repository."),
            CommitError::NoCommitToAmend => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("create a commit before using --amend"),
            CommitError::AmendUnsupported => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("create a new commit instead of amending a merge commit"),
            CommitError::InvalidAuthor(..) => CliError::command_usage(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("expected format: 'Name <email>'"),
            CommitError::InvalidDate { .. } => CliError::command_usage(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint(
                    "supported formats: '<unix> <+HHMM|-HHMM>', RFC 3339, 'YYYY-MM-DD HH:MM:SS +HHMM', 'YYYY-MM-DD', relative dates, or a Unix timestamp",
                ),
            CommitError::InvalidConfig(..) => CliError::command_usage(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("fix the offending value with 'libra config <key> <value>'"),
            CommitError::MessageFileRead { .. } => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
            }
            CommitError::TemplateRead { .. } => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
            }
            CommitError::TemplateUnedited => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("edit the message in the editor, or pass -m to set it directly"),
            CommitError::EmptyMessage => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("use -m to provide a commit message"),
            CommitError::EditorFailed(..) => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::IoReadFailed)
                .with_hint("set $EDITOR/core.editor, or pass -m to provide the message directly"),
            CommitError::TreeCreation(..) => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::InternalInvariant)
                .with_hint(format!("this is a bug; please report it at {ISSUE_URL}")),
            CommitError::IndexObjectInvalid(..) => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint("run 'libra fsck' to inspect missing or mistyped objects")
                .with_hint("restore the object or remove the bad index entry before committing"),
            CommitError::ObjectStorage(..) => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoWriteFailed)
            }
            CommitError::ParentCommitLoad { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint("the parent commit is missing or corrupted"),
            CommitError::HeadUpdate(..) => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoWriteFailed)
            }
            CommitError::PreCommitHook(..) => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("use --no-verify to bypass the hook"),
            CommitError::ConventionalCommit(..) => CliError::command_usage(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("see https://www.conventionalcommits.org for format rules"),
            CommitError::VaultSign(..) => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::AuthMissingCredentials)
                .with_hint("check vault configuration with 'libra config --list'"),
            CommitError::AutoStage(..) => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
            }
            CommitError::StagedChanges(..) => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint("failed to compute staged changes"),
        }
    }
}

// ---------------------------------------------------------------------------
// Structured output types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct FilesChanged {
    pub total: usize,
    pub new: usize,
    pub modified: usize,
    pub deleted: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommitOutput {
    /// Branch name or "detached" (backward-compatible with existing JSON consumers)
    pub head: String,
    /// Explicit branch indicator: Some(name) if on branch, None if detached HEAD
    pub branch: Option<String>,
    /// Full commit hash
    pub commit: String,
    /// Short commit hash (7 chars)
    pub short_id: String,
    /// First line of commit message
    pub subject: String,
    /// Whether this is a root commit (no parents)
    pub root_commit: bool,
    /// Whether this was an amend operation
    pub amend: bool,
    /// File change statistics
    pub files_changed: FilesChanged,
    /// Whether Signed-off-by trailer was appended
    pub signoff: bool,
    /// Conventional commit validation result: Some(true) if validated, None if not requested
    pub conventional: Option<bool>,
    /// Whether the commit was vault-GPG-signed
    pub signed: bool,
    /// Pre-rendered porcelain v1 status of the committed state for
    /// `--porcelain` (printed in place of the human summary). Not part of the
    /// JSON envelope.
    #[serde(skip)]
    pub porcelain: Option<String>,
}

/// Parse author string in format "Name <email>" and return (name, email)
/// If parsing fails, return an error message
fn parse_author(author: &str) -> Result<(String, String), CommitError> {
    let author = author.trim();

    // Try to parse "Name <email>" format
    if let Some(start_idx) = author.find('<')
        && let Some(end_idx) = author[start_idx..].find('>')
    {
        let end_idx = start_idx + end_idx;
        if start_idx < end_idx && end_idx == author.len() - 1 {
            let name = author[..start_idx].trim().to_string();
            let email = author[start_idx + 1..end_idx].trim().to_string();

            if !name.is_empty() && !email.is_empty() {
                return Ok((name, email));
            }
        }
    }

    Err(CommitError::InvalidAuthor(format!(
        "'{author}'. Expected format: 'Name <email>'"
    )))
}

/// A user's name + email pair used for commit authoring and committing.
#[derive(Clone, Debug)]
pub(crate) struct UserIdentity {
    pub(crate) name: String,
    pub(crate) email: String,
}

async fn get_user_config_value(key: &str) -> Option<String> {
    read_cascaded_config_value(LocalIdentityTarget::CurrentRepo, &format!("user.{key}"))
        .await
        .ok()
        .flatten()
}

fn missing_identity_error(name_missing: bool, email_missing: bool) -> CommitError {
    let detail = match (name_missing, email_missing) {
        (true, true) => "author identity unknown: name and email are not configured",
        (true, false) => "author identity unknown: name is not configured",
        (false, true) => "author identity unknown: email is not configured",
        (false, false) => "author identity unknown",
    };
    CommitError::IdentityMissing(detail.to_string())
}

async fn identity_config_only() -> bool {
    get_user_config_value("useConfigOnly")
        .await
        .map(|v| v.to_lowercase() == "true")
        .unwrap_or(false)
}

fn author_env_identity() -> (Option<String>, Option<String>) {
    (
        env_first_non_empty(&[
            "GIT_AUTHOR_NAME",
            "GIT_COMMITTER_NAME",
            "LIBRA_COMMITTER_NAME",
        ]),
        env_first_non_empty(&[
            "GIT_AUTHOR_EMAIL",
            "GIT_COMMITTER_EMAIL",
            "EMAIL",
            "LIBRA_COMMITTER_EMAIL",
        ]),
    )
}

fn committer_env_identity() -> (Option<String>, Option<String>) {
    (
        env_first_non_empty(&[
            "GIT_COMMITTER_NAME",
            "GIT_AUTHOR_NAME",
            "LIBRA_COMMITTER_NAME",
        ]),
        env_first_non_empty(&[
            "GIT_COMMITTER_EMAIL",
            "GIT_AUTHOR_EMAIL",
            "EMAIL",
            "LIBRA_COMMITTER_EMAIL",
        ]),
    )
}

pub(crate) async fn resolve_committer_identity() -> Result<UserIdentity, CommitError> {
    let identity_sources = resolve_user_identity_sources(LocalIdentityTarget::CurrentRepo)
        .await
        .map_err(|error| CommitError::IdentityMissing(error.to_string()))?;

    // Step 2: check user.useConfigOnly BEFORE reading env vars.
    // When useConfigOnly is true, only config values are acceptable — env vars are
    // skipped so the user is forced to configure identity
    // explicitly.
    if identity_config_only().await {
        if let (Some(name), Some(email)) = (
            identity_sources.config_name.clone(),
            identity_sources.config_email.clone(),
        ) {
            return Ok(UserIdentity { name, email });
        }
        // Report which field(s) are missing — using *config-only* perspective.
        // Reuse the already-fetched values instead of querying config again.
        let name_missing = identity_sources.config_name.is_none();
        let email_missing = identity_sources.config_email.is_none();
        return Err(missing_identity_error(name_missing, email_missing));
    }

    // Step 3: Git env vars override config; Libra-specific env vars remain a
    // lower-priority fallback for older automation.
    let (env_name, env_email) = committer_env_identity();
    let name = env_name.or(identity_sources.config_name);
    let email = env_email.or(identity_sources.config_email);

    if let (Some(name), Some(email)) = (name.clone(), email.clone()) {
        return Ok(UserIdentity { name, email });
    }

    Err(missing_identity_error(name.is_none(), email.is_none()))
}

async fn resolve_author_identity(
    author_override: Option<&str>,
) -> Result<UserIdentity, CommitError> {
    if let Some(author_str) = author_override {
        let (name, email) = parse_author(author_str)?;
        return Ok(UserIdentity { name, email });
    }

    let identity_sources = resolve_user_identity_sources(LocalIdentityTarget::CurrentRepo)
        .await
        .map_err(|error| CommitError::IdentityMissing(error.to_string()))?;

    if identity_config_only().await {
        if let (Some(name), Some(email)) = (
            identity_sources.config_name.clone(),
            identity_sources.config_email.clone(),
        ) {
            return Ok(UserIdentity { name, email });
        }
        return Err(missing_identity_error(
            identity_sources.config_name.is_none(),
            identity_sources.config_email.is_none(),
        ));
    }

    let (env_name, env_email) = author_env_identity();
    let name = env_name.or(identity_sources.config_name);
    let email = env_email.or(identity_sources.config_email);

    if let (Some(name), Some(email)) = (name.clone(), email.clone()) {
        return Ok(UserIdentity { name, email });
    }

    Err(missing_identity_error(name.is_none(), email.is_none()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SignatureDate {
    timestamp: usize,
    timezone: Option<String>,
}

fn env_date(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_timezone(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    if bytes.len() == 5
        && matches!(bytes[0], b'+' | b'-')
        && bytes[1..].iter().all(u8::is_ascii_digit)
    {
        return Some(value.to_string());
    }
    None
}

fn timezone_from_offset_seconds(offset_seconds: i32) -> String {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let abs = offset_seconds.unsigned_abs();
    let hours = abs / 3600;
    let minutes = (abs % 3600) / 60;
    format!("{sign}{hours:02}{minutes:02}")
}

fn timestamp_from_i64(
    source: &'static str,
    value: &str,
    timestamp: i64,
) -> Result<usize, CommitError> {
    usize::try_from(timestamp).map_err(|_| CommitError::InvalidDate {
        date_source: source,
        value: value.to_string(),
        detail: "date is before the Unix epoch or too large for this platform".to_string(),
    })
}

fn parse_signature_date(source: &'static str, value: &str) -> Result<SignatureDate, CommitError> {
    let trimmed = value.trim();

    if let Some((timestamp, timezone)) = trimmed.rsplit_once(' ')
        && let Some(timezone) = parse_timezone(timezone)
        && let Ok(timestamp) = timestamp.parse::<i64>()
    {
        return Ok(SignatureDate {
            timestamp: timestamp_from_i64(source, value, timestamp)?,
            timezone: Some(timezone),
        });
    }

    if let Ok(datetime) = DateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S %z") {
        return Ok(SignatureDate {
            timestamp: timestamp_from_i64(source, value, datetime.timestamp())?,
            timezone: Some(timezone_from_offset_seconds(
                datetime.offset().local_minus_utc(),
            )),
        });
    }

    if let Ok(datetime) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(SignatureDate {
            timestamp: timestamp_from_i64(source, value, datetime.timestamp())?,
            timezone: Some(timezone_from_offset_seconds(
                datetime.offset().local_minus_utc(),
            )),
        });
    }

    let timestamp = parse_date(trimmed).map_err(|error| CommitError::InvalidDate {
        date_source: source,
        value: value.to_string(),
        detail: error.to_string(),
    })?;
    Ok(SignatureDate {
        timestamp: timestamp_from_i64(source, value, timestamp)?,
        timezone: None,
    })
}

pub(crate) fn apply_signature_date(
    signature: &mut Signature,
    source: &'static str,
    value: &str,
) -> Result<(), CommitError> {
    let date = parse_signature_date(source, value)?;
    signature.timestamp = date.timestamp;
    if let Some(timezone) = date.timezone {
        signature.timezone = timezone;
    }
    Ok(())
}

/// Create author and committer signatures based on the provided arguments
pub(crate) async fn create_commit_signatures(
    author_override: Option<&str>,
    author_date_override: Option<&str>,
) -> Result<(Signature, Signature, UserIdentity), CommitError> {
    let author_identity = resolve_author_identity(author_override).await?;
    let committer_identity = resolve_committer_identity().await?;

    let mut author = Signature::new(
        SignatureType::Author,
        author_identity.name,
        author_identity.email,
    );
    if let Some(value) = author_date_override {
        apply_signature_date(&mut author, "--date", value)?;
    } else if let Some(value) = env_date("GIT_AUTHOR_DATE") {
        apply_signature_date(&mut author, "GIT_AUTHOR_DATE", &value)?;
    }

    let mut committer = Signature::new(
        SignatureType::Committer,
        committer_identity.name.clone(),
        committer_identity.email.clone(),
    );
    if let Some(value) = env_date("GIT_COMMITTER_DATE") {
        apply_signature_date(&mut committer, "GIT_COMMITTER_DATE", &value)?;
    }

    Ok((author, committer, committer_identity))
}

pub(crate) async fn create_committer_signature() -> Result<(Signature, UserIdentity), CommitError> {
    let committer_identity = resolve_committer_identity().await?;
    let mut committer = Signature::new(
        SignatureType::Committer,
        committer_identity.name.clone(),
        committer_identity.email.clone(),
    );
    if let Some(value) = env_date("GIT_COMMITTER_DATE") {
        apply_signature_date(&mut committer, "GIT_COMMITTER_DATE", &value)?;
    }
    Ok((committer, committer_identity))
}

fn first_message_line(message: &str) -> String {
    message.lines().next().unwrap_or("").trim().to_string()
}

/// Pure execution entry point. Receives `&OutputConfig` only for hook I/O
/// control (human mode: inherit, JSON/machine mode: piped). Does NOT render
/// output — returns [`CommitOutput`] on success for the caller to render.
pub async fn run_commit(
    args: CommitArgs,
    output: &OutputConfig,
) -> Result<CommitOutput, CommitError> {
    let is_amend = args.amend;
    let is_signoff = args.signoff;
    let is_conventional = args.conventional;
    let skip_hooks = args.disable_pre || args.no_verify;
    let skip_conventional_check = args.no_verify;
    // `--porcelain` is a machine-readable preview and, like Git, implies
    // `--dry-run`: it prints status and never creates the commit.
    let dry_run = args.dry_run || args.porcelain;

    // Auto-stage tracked modifications/deletions (git commit -a). For a dry run
    // this still computes the would-be-committed state, so snapshot the index
    // first and restore it afterwards, leaving the working state untouched.
    let index_snapshot = if dry_run && args.all {
        Some(std::fs::read(path::index()).map_err(|e| CommitError::IndexLoad(e.to_string()))?)
    } else {
        None
    };
    let auto_stage_applied = if args.all {
        auto_stage_tracked_changes()?
    } else {
        false
    };

    let index = Index::load(path::index()).map_err(|e| CommitError::IndexLoad(e.to_string()))?;
    let storage = ClientStorage::init(path::objects());
    let tracked_entries = index.tracked_entries(0);

    tree_plumbing::validate_index_objects(&index)
        .map_err(|error| CommitError::IndexObjectInvalid(error.to_string()))?;

    // Skip empty commit check for --amend operations
    if tracked_entries.is_empty() && !args.allow_empty && !is_amend && !auto_stage_applied {
        // No files have ever been staged — distinct from "staged but unchanged"
        return Err(CommitError::NothingToCommitNoTracked);
    }

    // Verify staged changes relative to HEAD (skip for --amend)
    let staged_changes = status::changes_to_be_committed_safe()
        .await
        .map_err(|e| CommitError::StagedChanges(e.to_string()))?;
    if staged_changes.is_empty() && !args.allow_empty && !is_amend {
        return Err(CommitError::NothingToCommit);
    }

    // `lfs.lockEnforce` gate (lore.md 2.8): staged new+modified+DELETED
    // paths (deletions never reach the push-time OID check — this is the
    // only guard for them). Skipped on dry-run/--porcelain (previews never
    // touch the network). Runs AFTER `-a` auto-staging, matching the
    // existing pre-commit-hook-failure semantics (the auto-staged index
    // mutation persists on abort).
    if !dry_run {
        let mut candidates: Vec<String> = Vec::new();
        for path in staged_changes
            .new
            .iter()
            .chain(staged_changes.modified.iter())
            .chain(staged_changes.deleted.iter())
        {
            candidates.push(path.display().to_string());
        }
        crate::command::lfs::enforce_lock_policy(&candidates)
            .await
            .map_err(CommitError::LockPolicy)?;
    }

    // `--porcelain` snapshot of the would-be-committed state: taken AFTER `-a`
    // auto-staging and the staged recompute above so it reflects what would be
    // committed. Inert under `--json`. `.take()` below hands it to whichever
    // build_commit_output branch (amend/normal dry-run) runs.
    let mut porcelain_text = if args.porcelain && !output.is_json() {
        Some(gather_commit_porcelain().await?)
    } else {
        None
    };

    // Restore the pre-`-a` index for dry runs so the preview never mutates the
    // working state (the snapshot is only taken for `dry_run && args.all`).
    if let Some(bytes) = index_snapshot {
        std::fs::write(path::index(), bytes).map_err(|e| CommitError::IndexSave(e.to_string()))?;
    }

    // INVARIANT: hooks and message validation must run before creating the
    // commit object or updating HEAD; once those writes happen, hook failure can
    // no longer block the commit without explicit rollback logic.
    if !skip_hooks {
        run_pre_commit_hook(output)?;
    }

    // Resolve parent commits (needed to seed the editor with the amend parent's
    // message).
    let parents_commit_ids = get_parents_ids().await;

    // Resolve the commit message (may open the editor for -e/-v or a bare commit).
    let message = resolve_final_message(&args, output, &parents_commit_ids).await?;

    // Create tree
    let tree = create_tree(&index, &storage, "".into()).await?;

    // Create author and committer signatures
    let reuse_author = load_reused_commit_author(&args).await?;
    let (mut author, committer, committer_identity) =
        create_commit_signatures(args.author.as_deref(), args.date.as_deref()).await?;
    let reused_author_applied = if let Some(mut reused_author) = reuse_author
        && args.author.is_none()
        && !args.reset_author
    {
        if let Some(date) = args.date.as_deref() {
            apply_signature_date(&mut reused_author, "--date", date)?;
        }
        author = reused_author;
        true
    } else {
        false
    };

    // Build the signoff trailer
    let signoff_line = if is_signoff {
        Some(format!(
            "Signed-off-by: {} <{}>",
            committer_identity.name, committer_identity.email
        ))
    } else {
        None
    };

    // Amend path
    if is_amend {
        if parents_commit_ids.is_empty() {
            return Err(CommitError::NoCommitToAmend);
        }
        if parents_commit_ids.len() > 1 {
            return Err(CommitError::AmendUnsupported);
        }
        let parent_commit = load_object::<Commit>(&parents_commit_ids[0]).map_err(|e| {
            CommitError::ParentCommitLoad {
                commit_id: parents_commit_ids[0].to_string(),
                detail: e.to_string(),
            }
        })?;
        let grandpa_commit_id = parent_commit.parent_commit_ids.clone();

        // Git-compatible amend authorship: preserve the original commit's author
        // (name, email, and authored date) unless the user explicitly resets it
        // with `--reset-author` or supplies a new one with `--author`. Without this
        // the amended commit would silently adopt the current committer identity,
        // which makes `--reset-author` a no-op and diverges from Git.
        let author = if args.reset_author || args.author.is_some() || reused_author_applied {
            author
        } else {
            let mut parent_author = parent_commit.author.clone();
            if let Some(date) = args.date.as_deref() {
                apply_signature_date(&mut parent_author, "--date", date)?;
            }
            parent_author
        };

        // `--amend --no-edit` reuses the parent message verbatim (no re-cleanup),
        // EXCEPT when an explicit `-t/--template` supplied a message — then the
        // resolved template wins (matching Git, where `-t` overrides the amend
        // parent message). Other message sources (`-m`/`-F`/`-C`) keep their
        // existing behavior.
        //
        // The parent's stored body embeds any `gpgsig` signature header (Libra
        // keeps the signature inside `Commit.message`; see `format_commit_msg`),
        // so we must extract the true log message with `parse_commit_msg` before
        // reusing it — otherwise a signed parent leaks its PGP/SSH signature
        // block into the amended message instead of the real subject/body.
        let final_message = if args.no_edit && args.template.is_none() {
            parse_commit_msg(&parent_commit.message).0.to_string()
        } else {
            message.clone()
        };

        let commit_message = match &signoff_line {
            // Route through append_trailers so `-s` joins an existing trailer
            // block (e.g. from `--trailer`) instead of opening a second
            // paragraph a Git-strict trailer parser would not see.
            Some(line) => append_trailers(&final_message, std::slice::from_ref(line)),
            None => final_message.clone(),
        };
        let mut committer = committer;
        refresh_noop_amend_committer_timestamp(
            &parent_commit,
            &author,
            &mut committer,
            &tree.id,
            &grandpa_commit_id,
            &commit_message,
        );

        // Conventional commit validation
        if is_conventional
            && !skip_conventional_check
            && !check_conventional_commits_message(&commit_message)
        {
            return Err(CommitError::ConventionalCommit(
                "commit message does not follow conventional commits".to_string(),
            ));
        }

        if dry_run {
            let commit = Commit::new(
                author,
                committer,
                tree.id,
                grandpa_commit_id,
                &format_commit_msg(&commit_message, None),
            );
            return Ok(build_commit_output(
                &commit,
                &commit_message,
                &staged_changes,
                true,
                is_signoff,
                if is_conventional && !skip_conventional_check {
                    Some(true)
                } else {
                    None
                },
                false,
                porcelain_text.take(),
            )
            .await);
        }

        let gpg_sig = if args.no_gpg_sign {
            None
        } else {
            vault_sign_commit(
                &tree.id,
                &grandpa_commit_id,
                &author,
                &committer,
                &commit_message,
                false,
            )
            .await?
        };

        let commit = Commit::new(
            author,
            committer,
            tree.id,
            grandpa_commit_id,
            &format_commit_msg(&commit_message, gpg_sig.as_deref()),
        );

        // INVARIANT: persist the commit object before moving HEAD so a crash
        // after ref update never points the branch at a missing object.
        save_commit_object(&storage, &commit)?;
        update_head_and_reflog(&commit.id.to_string(), &commit_message).await?;

        let conventional_result = if is_conventional && !skip_conventional_check {
            Some(true)
        } else {
            None
        };
        return Ok(build_commit_output(
            &commit,
            &commit_message,
            &staged_changes,
            is_amend,
            is_signoff,
            conventional_result,
            gpg_sig.is_some(),
            porcelain_text.take(),
        )
        .await);
    }

    // Normal (non-amend) path
    let commit_message = match &signoff_line {
        // See the amend path: `-s` must join an existing trailer block.
        Some(line) => append_trailers(&message, std::slice::from_ref(line)),
        None => message.clone(),
    };

    // Conventional commit validation
    if is_conventional
        && !skip_conventional_check
        && !check_conventional_commits_message(&commit_message)
    {
        return Err(CommitError::ConventionalCommit(
            "commit message does not follow conventional commits".to_string(),
        ));
    }

    if dry_run {
        let commit = Commit::new(
            author,
            committer,
            tree.id,
            parents_commit_ids,
            &format_commit_msg(&commit_message, None),
        );
        return Ok(build_commit_output(
            &commit,
            &commit_message,
            &staged_changes,
            false,
            is_signoff,
            if is_conventional && !skip_conventional_check {
                Some(true)
            } else {
                None
            },
            false,
            porcelain_text.take(),
        )
        .await);
    }

    let gpg_sig = if args.no_gpg_sign {
        None
    } else {
        vault_sign_commit(
            &tree.id,
            &parents_commit_ids,
            &author,
            &committer,
            &commit_message,
            false,
        )
        .await?
    };

    let commit = Commit::new(
        author,
        committer,
        tree.id,
        parents_commit_ids,
        &format_commit_msg(&commit_message, gpg_sig.as_deref()),
    );

    // INVARIANT: persist the commit object before moving HEAD so a crash after
    // ref update never points the branch at a missing object.
    save_commit_object(&storage, &commit)?;
    update_head_and_reflog(&commit.id.to_string(), &commit_message).await?;

    let conventional_result = if is_conventional && !skip_conventional_check {
        Some(true)
    } else {
        None
    };
    Ok(build_commit_output(
        &commit,
        &commit_message,
        &staged_changes,
        is_amend,
        is_signoff,
        conventional_result,
        gpg_sig.is_some(),
        porcelain_text.take(),
    )
    .await)
}

fn refresh_noop_amend_committer_timestamp(
    parent_commit: &Commit,
    author: &Signature,
    committer: &mut Signature,
    tree_id: &ObjectHash,
    parent_ids: &[ObjectHash],
    commit_message: &str,
) {
    let parent_message = parse_commit_msg(&parent_commit.message).0;
    let same_committer_identity = parent_commit.committer.signature_type
        == committer.signature_type
        && parent_commit.committer.name == committer.name
        && parent_commit.committer.email == committer.email
        && parent_commit.committer.timezone == committer.timezone;

    if parent_commit.tree_id == *tree_id
        && parent_commit.parent_commit_ids == parent_ids
        && parent_commit.author == *author
        && same_committer_identity
        && parent_message == commit_message
        && committer.timestamp <= parent_commit.committer.timestamp
    {
        committer.timestamp = parent_commit.committer.timestamp.saturating_add(1);
    }
}

/// Resolve the final commit message from CLI arguments.
/// Resolve the final commit message, opening the editor when needed and
/// possible.
///
/// Message sources are tried in order (fixup → squash → -C/-c → -m → -F). The
/// editor is opened when `-e`/`-c` is given, or when no source is supplied and
/// `--no-edit` is absent — provided an editor is available (an explicitly
/// configured `$GIT_EDITOR`/`core.editor`/`$VISUAL`/`$EDITOR` runs even without
/// a TTY; the implicit `vi` fallback requires an interactive terminal). With
/// `-v` the staged diff is appended to the template and stripped at the scissors
/// marker so it never enters the message. An empty final message aborts.
async fn resolve_final_message(
    args: &CommitArgs,
    output: &OutputConfig,
    parent_ids: &[ObjectHash],
) -> Result<String, CommitError> {
    let base: Option<String> = if let Some(spec) = &args.fixup {
        Some(format!(
            "fixup! {}",
            commit_subject(&load_commit_message(spec).await?)
        ))
    } else if let Some(spec) = &args.squash {
        Some(format!(
            "squash! {}",
            commit_subject(&load_commit_message(spec).await?)
        ))
    } else if let Some(spec) = args.reuse_message.as_ref().or(args.reedit_message.as_ref()) {
        Some(load_commit_message(spec).await?)
    } else if let Some(msg) = &args.message {
        Some(msg.clone())
    } else if let Some(file_path) = &args.file {
        Some(tokio::fs::read_to_string(file_path).await.map_err(|e| {
            CommitError::MessageFileRead {
                path: file_path.clone(),
                detail: e.to_string(),
            }
        })?)
    } else {
        None
    };

    // `-e`/`-c` always edit; otherwise an editor is needed only to author a
    // message when no source was supplied and `--no-edit` was not given.
    let needs_editor =
        args.edit || args.reedit_message.is_some() || (base.is_none() && !args.no_edit);

    // `-t`/`--template` (or the `commit.template` config) seeds the message only
    // when no explicit source was supplied (a message source wins, and the
    // template is then not even read — matching Git). The template takes
    // precedence over the amend parent's message as the editor seed.
    let template_content = if base.is_none() {
        resolve_commit_template(args).await?
    } else {
        None
    };

    // Initial editor buffer / non-editor fallback: the explicit source, else the
    // template, else the amend parent's message, else empty.
    let initial = match &base {
        Some(text) => text.clone(),
        None => match &template_content {
            Some(template) => template.clone(),
            None if args.amend && !parent_ids.is_empty() => load_object::<Commit>(&parent_ids[0])
                // Strip any embedded `gpgsig` header so a signed parent's
                // signature block does not seed the editor buffer.
                .map(|commit| parse_commit_msg(&commit.message).0.to_string())
                .unwrap_or_default(),
            None => String::new(),
        },
    };

    // The cleanup mode and verbose flag fall back to `commit.cleanup` /
    // `commit.verbose` config (cascade: local repo, then global) when the CLI flag
    // is unset — the CLI flag always WINS and short-circuits the config read, so a
    // bad repo/global config can still be overridden for a single commit. Only when
    // the flag is unset is the config consulted, and an invalid configured value is
    // then fatal (Git rejects a bad commit.cleanup mode / commit.verbose value).
    let mode = match args.cleanup {
        Some(mode) => mode,
        None => match read_cascaded_config_value(LocalIdentityTarget::CurrentRepo, "commit.cleanup")
            .await
            .ok()
            .flatten()
        {
            Some(value) => parse_cleanup_mode(&value).ok_or_else(|| {
                CommitError::InvalidConfig(format!(
                    "invalid commit.cleanup config value '{value}' (expected strip/whitespace/verbatim/scissors/default)"
                ))
            })?,
            None => CleanupMode::Strip,
        },
    };

    let verbose = if args.verbose {
        true
    } else {
        match read_cascaded_config_value(LocalIdentityTarget::CurrentRepo, "commit.verbose")
            .await
            .ok()
            .flatten()
        {
            Some(value) => parse_git_config_bool(&value).ok_or_else(|| {
                CommitError::InvalidConfig(format!(
                    "bad boolean config value '{value}' for 'commit.verbose'"
                ))
            })?,
            None => false,
        }
    };

    // Pick the editor: an explicitly configured one runs regardless of TTY; the
    // `vi` fallback only applies on an interactive terminal.
    let editor_cmd = if needs_editor && !output.is_json() {
        match editor::resolve_editor().await {
            Some(cmd) => Some(cmd),
            None if std::io::stdin().is_terminal() => Some("vi".to_string()),
            None => None,
        }
    } else {
        None
    };

    // `--status`: a `#`-commented status section to seed into the editor
    // template (informational only). Seed it ONLY when an editor will open AND the
    // cleanup that will be applied actually strips `#` comments — Strip/Default.
    // `-v` no longer forces a strip (it only truncates the appended diff, then the
    // selected mode cleans the message), so under `--cleanup=verbatim`/`whitespace`/
    // `scissors` (which keep `#` lines above the marker) the status is NOT seeded —
    // even with `-v` — so it can never leak into the final message (matching Git).
    let cleanup_strips_comments = matches!(mode, CleanupMode::Strip | CleanupMode::Default);
    let status_section = if args.status && editor_cmd.is_some() && cleanup_strips_comments {
        build_status_section().await
    } else {
        None
    };

    let editor_opened = editor_cmd.is_some();
    let resolved = if let Some(editor_cmd) = editor_cmd {
        let buffer = if verbose {
            build_verbose_template(&initial, status_section.as_deref(), cleanup_strips_comments)
                .await?
        } else {
            append_status_section(initial.clone(), status_section.as_deref())
        };
        let path = util::storage_path().join("COMMIT_EDITMSG");
        let raw = editor::edit_message(&path, &buffer, &editor_cmd, true)
            .await
            .map_err(|e| CommitError::EditorFailed(e.to_string()))?;
        // `-v` only appends the staged diff below a scissors marker — drop that
        // diff first, then apply the SELECTED cleanup to the edited message. `-v`
        // does not force a strip, so `--cleanup=verbatim`/`whitespace -v` still keep
        // `#` lines above the marker (matching Git, where the cleanup mode governs
        // the message regardless of `-v`).
        let edited = if verbose {
            truncate_at_scissors(&raw)
        } else {
            raw
        };
        cleanup_commit_message(&edited, mode)
    } else {
        // No editor was opened. `-v` here just prints the staged diff to stderr
        // (it never enters the message).
        if verbose
            && !output.is_json()
            && !output.quiet
            && let Ok(diff) = diff::staged_diff_text().await
            && !diff.trim().is_empty()
        {
            eprintln!("{diff}");
        }
        // Git's `default` and `scissors` cleanup both carry an "if the message is
        // edited" clause: `default` is strip-when-edited / whitespace-otherwise, and
        // `scissors` truncates at the scissors marker only when edited. No editor
        // opened here, so both resolve to whitespace (comment/scissors lines kept);
        // every other mode is applied as-is.
        let effective_mode = match mode {
            CleanupMode::Default | CleanupMode::Scissors => CleanupMode::Whitespace,
            other => other,
        };
        cleanup_commit_message(&initial, effective_mode)
    };

    // When a template seeded the message and the editor was meant to open
    // (i.e. NOT `--no-edit`), Git aborts unless the user actually edited it:
    //   - editor ran but the result equals the cleaned template → unedited;
    //   - the editor was required but none was available → never edited.
    // `--no-edit` (needs_editor == false) bypasses this and uses the template
    // directly.
    if let Some(template) = &template_content {
        let unedited = if editor_opened {
            resolved == cleanup_commit_message(template, mode)
        } else {
            needs_editor
        };
        if unedited {
            return Err(CommitError::TemplateUnedited);
        }
    }

    if resolved.trim().is_empty() {
        return Err(CommitError::EmptyMessage);
    }

    if args.trailers.is_empty() {
        Ok(resolved)
    } else {
        Ok(append_trailers(&resolved, &args.trailers))
    }
}

/// Resolve the commit message template: the `-t`/`--template` file when given,
/// otherwise the `commit.template` config (a file path), otherwise `None`. A
/// leading `~/` is expanded to `$HOME`. The caller only invokes this when no
/// explicit message source was supplied, so a `-t` path is never read when a
/// message source (e.g. `-m`) wins — matching Git, which then ignores `-t`.
async fn resolve_commit_template(args: &CommitArgs) -> Result<Option<String>, CommitError> {
    let path = match &args.template {
        // An explicit `-t` always applies (it overrides the amend parent message).
        Some(path) => Some(path.clone()),
        // The `commit.template` config seeds new commits only — `--amend` reuses
        // the parent message, so the config template is not consulted there (and
        // is not read, so a bad config path cannot break an `--amend` reuse).
        None if !args.amend => {
            read_cascaded_config_value(LocalIdentityTarget::CurrentRepo, "commit.template")
                .await
                .ok()
                .flatten()
        }
        None => None,
    };
    let Some(path) = path else {
        return Ok(None);
    };
    let expanded = match path.strip_prefix("~/") {
        Some(rest) => match std::env::var("HOME") {
            Ok(home) => format!("{home}/{rest}"),
            Err(_) => path.clone(),
        },
        None => path.clone(),
    };
    let content =
        tokio::fs::read_to_string(&expanded)
            .await
            .map_err(|e| CommitError::TemplateRead {
                path,
                detail: e.to_string(),
            })?;
    Ok(Some(content))
}

/// Build the `commit -v` editor template: the initial message, a commented
/// help header, an optional commented status section (`--status`), the
/// Git-standard scissors marker, and the staged diff. Everything from the
/// scissors line down is stripped by `Scissors` cleanup; the commented header
/// and status section are stripped as comment lines.
async fn build_verbose_template(
    initial: &str,
    status_section: Option<&str>,
    strips_comments: bool,
) -> Result<String, CommitError> {
    let diff = diff::staged_diff_text()
        .await
        .map_err(|e| CommitError::StagedChanges(e.to_string()))?;
    let mut buffer = String::new();
    buffer.push_str(initial);
    if !initial.is_empty() && !initial.ends_with('\n') {
        buffer.push('\n');
    }
    buffer.push('\n');
    // The `#`-prefixed helper/status lines ABOVE the scissors marker would survive
    // into the message under a cleanup mode that keeps comments (verbatim/whitespace/
    // explicit scissors). Emit them ONLY when the effective cleanup strips comments,
    // so a non-stripping `-v` commit cannot commit Libra's own template cruft. The
    // scissors marker and the staged diff below it are always truncated away.
    if strips_comments {
        buffer.push_str("# Please enter the commit message for your changes. Lines starting\n");
        buffer.push_str("# with '#' will be ignored, and an empty message aborts the commit.\n");
        // `--status`: commented status, above the scissors so it stays visible
        // while editing (Git places the status section here too). It is only ever
        // `Some` when the cleanup strips comments, so it is gated here too.
        if let Some(section) = status_section {
            buffer.push_str(section);
        }
        buffer.push_str("#\n");
    }
    buffer.push_str("# ------------------------ >8 ------------------------\n");
    buffer.push_str("# Do not modify or remove the line above.\n");
    buffer.push_str("# Everything below it will be ignored.\n");
    buffer.push_str(&diff);
    if !diff.is_empty() && !diff.ends_with('\n') {
        buffer.push('\n');
    }
    Ok(buffer)
}

/// Append a `#`-commented status section (`--status`) to a plain (non-verbose)
/// editor buffer. Returns `buffer` unchanged when there is no section.
fn append_status_section(mut buffer: String, status_section: Option<&str>) -> String {
    if let Some(section) = status_section {
        if !buffer.is_empty() && !buffer.ends_with('\n') {
            buffer.push('\n');
        }
        buffer.push('\n');
        buffer.push_str(section);
    }
    buffer
}

/// Render the working-tree status as a `#`-commented block for the commit
/// editor template (`--status`). Each line of the long-format `status` output
/// is prefixed with `# ` so `cleanup_commit_message` strips it from the final
/// message (informational only). Returns `None` when the status cannot be
/// rendered or is empty — non-fatal, the template simply omits it.
async fn build_status_section() -> Option<String> {
    let mut raw: Vec<u8> = Vec::new();
    status::execute_to(status::StatusArgs::default(), &mut raw)
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&raw);
    if text.trim().is_empty() {
        return None;
    }
    let mut section = String::new();
    for line in text.lines() {
        if line.is_empty() {
            section.push_str("#\n");
        } else {
            section.push_str("# ");
            section.push_str(line);
            section.push('\n');
        }
    }
    Some(section)
}

/// Load the commit message of the given commit-ish.
async fn load_commit_message(spec: &str) -> Result<String, CommitError> {
    let commit = load_commit_for_message_source(spec).await?;
    // Strip any embedded `gpgsig` header so that reusing a signed commit's
    // message (via `-C`/`-c`/`--reuse-message`/`--fixup`/`--squash`) yields the
    // real log message rather than the leading PGP/SSH signature block.
    Ok(parse_commit_msg(&commit.message).0.to_string())
}

async fn load_reused_commit_author(args: &CommitArgs) -> Result<Option<Signature>, CommitError> {
    let Some(spec) = args.reuse_message.as_ref().or(args.reedit_message.as_ref()) else {
        return Ok(None);
    };
    Ok(Some(load_commit_for_message_source(spec).await?.author))
}

async fn load_commit_for_message_source(spec: &str) -> Result<Commit, CommitError> {
    let hash =
        util::get_commit_base_typed(spec)
            .await
            .map_err(|e| CommitError::ParentCommitLoad {
                commit_id: spec.to_string(),
                detail: e.to_string(),
            })?;
    let commit = load_object::<Commit>(&hash).map_err(|e| CommitError::ParentCommitLoad {
        commit_id: spec.to_string(),
        detail: e.to_string(),
    })?;
    Ok(commit)
}

/// Extract the subject (first line) of a commit message.
fn commit_subject(message: &str) -> &str {
    message.lines().next().unwrap_or(message).trim()
}

/// Parse a `commit.cleanup` config value into a [`CleanupMode`], case-insensitively
/// (`strip`/`whitespace`/`verbatim`/`scissors`/`default`). Returns `None` for an
/// unrecognized value so the caller falls back to the built-in default.
pub(crate) fn parse_cleanup_mode(value: &str) -> Option<CleanupMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "strip" => Some(CleanupMode::Strip),
        "whitespace" => Some(CleanupMode::Whitespace),
        "verbatim" => Some(CleanupMode::Verbatim),
        "scissors" => Some(CleanupMode::Scissors),
        "default" => Some(CleanupMode::Default),
        _ => None,
    }
}

/// Interpret a Git "bool-or-int" config value for `commit.verbose`: `true`/`yes`/
/// `on` are `Some(true)` and `false`/`no`/`off` are `Some(false)`; an integer is
/// truthy when non-zero (Git documents `commit.verbose` as boolean-or-int, where
/// `0` is off and any positive level enables the verbose diff); any other value is
/// `None` (invalid — Git treats a bad value as fatal). A present-but-empty value is
/// not reachable here: the shared config reader maps it to "unset".
fn parse_git_config_bool(value: &str) -> Option<bool> {
    let v = value.trim().to_ascii_lowercase();
    match v.as_str() {
        "true" | "yes" | "on" => Some(true),
        "false" | "no" | "off" => Some(false),
        _ => {
            // Git bool-or-int: an integer with an optional case-insensitive
            // `k`/`m`/`g` 1024-based multiplier suffix, truthy when non-zero.
            let (digits, mult) = match v.as_bytes().last() {
                Some(b'k') => (&v[..v.len() - 1], 1024_i64),
                Some(b'm') => (&v[..v.len() - 1], 1024 * 1024),
                Some(b'g') => (&v[..v.len() - 1], 1024 * 1024 * 1024),
                _ => (v.as_str(), 1),
            };
            digits
                .parse::<i64>()
                .ok()
                .and_then(|n| n.checked_mul(mult))
                .map(|n| n != 0)
        }
    }
}

/// Truncate a message at the scissors marker (everything from the marker line
/// onward is dropped). Accepts both a bare marker and Git's comment-prefixed form
/// (`# ------------------------ >8 ...`), so the `commit -v` template (Git-standard
/// `#` form) and the staged diff below it are removed.
fn truncate_at_scissors(message: &str) -> String {
    message
        .lines()
        .take_while(|line| {
            !line
                .trim()
                .trim_start_matches('#')
                .trim_start()
                .starts_with("------------------------ >8 ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Apply Git-style cleanup to a commit message.
pub(crate) fn cleanup_commit_message(message: &str, mode: CleanupMode) -> String {
    match mode {
        CleanupMode::Verbatim => message.to_string(),
        CleanupMode::Scissors => {
            // Git's `scissors` is whitespace cleanup PLUS truncation at the marker:
            // the message above the marker keeps its `#` comment lines (unlike
            // strip). The verbose path, which needs a post-truncation strip, calls
            // `truncate_at_scissors` + `Strip` directly instead.
            cleanup_commit_message(&truncate_at_scissors(message), CleanupMode::Whitespace)
        }
        CleanupMode::Whitespace => {
            let lines: Vec<String> = message
                .lines()
                .map(|line| line.trim_end().to_string())
                .collect();
            let trimmed = trim_empty_lines(&lines);
            trimmed.join("\n")
        }
        CleanupMode::Strip | CleanupMode::Default => {
            let lines: Vec<String> = message
                .lines()
                .map(|line| {
                    let trimmed = line.trim_end();
                    if trimmed.starts_with('#') {
                        String::new()
                    } else {
                        trimmed.to_string()
                    }
                })
                .collect();
            let mut result = trim_empty_lines(&lines);
            // Git's strip collapses CONSECUTIVE blank lines into one but keeps
            // single blank separators — deleting every interior blank (the old
            // behavior) flattened multi-paragraph messages and destroyed
            // user-typed trailer blocks at write time.
            result.dedup_by(|current, previous| current.is_empty() && previous.is_empty());
            result.join("\n")
        }
    }
}

fn trim_empty_lines(lines: &[String]) -> Vec<String> {
    let start = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map(|i| i + 1)
        .unwrap_or(lines.len());
    lines[start..end].to_vec()
}

/// Append trailer lines to a commit message the way `git interpret-trailers`
/// does: when the message already ends in a qualifying trailer block, append
/// INTO that block (single newline — so `-s` + `--trailer` produce ONE
/// Git-parseable block); otherwise open a new final paragraph (blank line).
/// The old single-newline branch for newline-terminated messages glued
/// trailers onto the last body line, invisible to a Git-strict parser.
fn append_trailers(message: &str, trailers: &[String]) -> String {
    let trailers_block = trailers.join("\n");
    let trimmed = message.trim_end();
    if trimmed.is_empty() {
        trailers_block
    } else if crate::internal::log::trailer::ends_with_trailer_block(trimmed) {
        format!("{trimmed}\n{trailers_block}")
    } else {
        format!("{trimmed}\n\n{trailers_block}")
    }
}

/// Run the pre-commit hook, respecting OutputConfig for I/O isolation.
fn run_pre_commit_hook(output: &OutputConfig) -> Result<(), CommitError> {
    let hooks_dir = path::hooks();

    #[cfg(not(target_os = "windows"))]
    let hook_path = hooks_dir.join("pre-commit.sh");

    #[cfg(target_os = "windows")]
    let hook_path = hooks_dir.join("pre-commit.ps1");

    if !hook_path.exists() {
        return Ok(());
    }

    let hook_display = hook_path.display().to_string();

    // In JSON/machine mode, capture hook output to prevent stdout/stderr pollution.
    // In human mode, inherit so the user sees hook output directly.
    let (stdout_cfg, stderr_cfg) = if output.is_json() {
        (Stdio::piped(), Stdio::piped())
    } else {
        (Stdio::inherit(), Stdio::inherit())
    };

    #[cfg(not(target_os = "windows"))]
    let hook_output = Command::new("sh")
        .arg(&hook_path)
        .current_dir(util::working_dir())
        .stdout(stdout_cfg)
        .stderr(stderr_cfg)
        .output()
        .map_err(|e| {
            CommitError::PreCommitHook(format!("failed to execute hook {hook_display}: {e}"))
        })?;

    #[cfg(target_os = "windows")]
    let hook_output = Command::new("powershell")
        .arg("-File")
        .arg(&hook_path)
        .current_dir(util::working_dir())
        .stdout(stdout_cfg)
        .stderr(stderr_cfg)
        .output()
        .map_err(|e| {
            CommitError::PreCommitHook(format!("failed to execute hook {hook_display}: {e}"))
        })?;

    if !hook_output.status.success() {
        return Err(CommitError::PreCommitHook(format!(
            "hook {hook_display} failed with exit code {}",
            hook_output.status.code().unwrap_or(-1)
        )));
    }
    Ok(())
}

/// Save a commit object to storage.
fn save_commit_object(storage: &ClientStorage, commit: &Commit) -> Result<(), CommitError> {
    let data = commit
        .to_data()
        .map_err(|e| CommitError::ObjectStorage(format!("failed to serialize commit: {e}")))?;
    storage
        .put(&commit.id, &data, commit.get_type())
        .map_err(|e| CommitError::ObjectStorage(format!("failed to save commit: {e}")))?;
    Ok(())
}

/// Build a [`CommitOutput`] from the created commit and flags.
///
/// `user_message` is the commit message as provided by the user (before GPG
/// signature embedding), used to derive the `subject` field.
// Assembles the structured commit result from already-computed parts; the
// argument count is inherent to what a commit result records.
#[allow(clippy::too_many_arguments)]
async fn build_commit_output(
    commit: &Commit,
    user_message: &str,
    staged_changes: &status::Changes,
    amend: bool,
    signoff: bool,
    conventional: Option<bool>,
    signed: bool,
    porcelain: Option<String>,
) -> CommitOutput {
    let (head_label, branch) = match Head::current().await {
        Head::Branch(name) => (name.clone(), Some(name)),
        Head::Detached(_) => ("detached".to_string(), None),
    };

    let commit_str = commit.id.to_string();
    let short_id: String = commit_str.chars().take(7).collect();
    let subject = first_message_line(user_message);

    CommitOutput {
        head: head_label,
        branch,
        commit: commit_str,
        short_id,
        subject,
        root_commit: commit.parent_commit_ids.is_empty(),
        amend,
        files_changed: FilesChanged {
            total: staged_changes.new.len()
                + staged_changes.modified.len()
                + staged_changes.deleted.len(),
            new: staged_changes.new.len(),
            modified: staged_changes.modified.len(),
            deleted: staged_changes.deleted.len(),
        },
        signoff,
        conventional,
        signed,
        porcelain,
    }
}

/// Render commit output according to OutputConfig (human / JSON / machine).
fn render_commit_output(result: &CommitOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("commit", result, output);
    }

    if output.quiet {
        return Ok(());
    }

    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    if result.root_commit {
        writeln!(
            writer,
            "[{} (root-commit) {}] {}",
            result.head, result.short_id, result.subject
        )
        .map_err(|e| CliError::io(format!("failed to write commit summary: {e}")))?;
    } else {
        writeln!(
            writer,
            "[{} {}] {}",
            result.head, result.short_id, result.subject
        )
        .map_err(|e| CliError::io(format!("failed to write commit summary: {e}")))?;
    }

    let file_count = result.files_changed.total;
    if file_count > 0 {
        let files_word = if file_count == 1 { "file" } else { "files" };
        writeln!(
            writer,
            " {} {} changed (new: {}, modified: {}, deleted: {})",
            file_count,
            files_word,
            result.files_changed.new,
            result.files_changed.modified,
            result.files_changed.deleted
        )
        .map_err(|e| CliError::io(format!("failed to write commit summary: {e}")))?;
    }
    Ok(())
}

pub async fn execute(args: CommitArgs) {
    if let Err(error) = execute_safe(args, &OutputConfig::default()).await {
        error.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting.
///
/// # Side Effects
/// - Reads the index and staged objects to build a new tree and commit object.
/// - Resolves author/committer identity and optionally signs the commit through
///   the vault when signing is enabled.
/// - Writes new objects, updates HEAD/current branch, records reflog state, and
///   renders the requested success output.
///
/// # Errors
/// Returns [`CliError`] when the repository is missing or corrupt, there is
/// nothing to commit, identity/signing setup fails, object writes fail, or HEAD
/// cannot be updated.
pub async fn execute_safe(args: CommitArgs, output: &OutputConfig) -> CliResult<()> {
    let result = run_commit(args, output).await.map_err(CliError::from)?;
    // rerere: a commit may have finalized a resolved merge — record the
    // postimage of any tracked conflict now resolved so an identical conflict is
    // auto-resolved next time. A no-op unless `rerere.enabled` and there is a
    // tracked conflict to record (so ordinary commits are unaffected).
    if let Err(error) = crate::command::rerere::auto_update(false).await {
        tracing::warn!("rerere auto-update after commit failed: {error}");
    }
    // `--porcelain` replaces the human commit summary with `status --porcelain`
    // output of the committed state (gathered inside run_commit AFTER any `-a`
    // auto-staging, before the commit write); inert under `--json`.
    if let Some(text) = &result.porcelain {
        print!("{text}");
    } else {
        render_commit_output(&result, output)?;
    }
    dispatch_current_repo_vcs_event_to_history(VCS_EVENT_POST_COMMIT).await;
    Ok(())
}

/// Render the to-be-committed working-tree state in porcelain v1 format,
/// identical to `libra status --porcelain` (staged changes in column 1,
/// unstaged in column 2, untracked as `??`, with untracked directories
/// collapsed). Returned as a string so the caller can emit it in place of the
/// normal commit summary.
async fn gather_commit_porcelain() -> Result<String, CommitError> {
    let to_err = |e: String| CommitError::StagedChanges(e);
    let staged = status::changes_to_be_committed_safe()
        .await
        .map(|c| c.to_relative())
        .map_err(|e| to_err(e.to_string()))?;
    let mut unstaged = status::changes_to_be_staged()
        .map(|c| c.to_relative())
        .map_err(|e| to_err(e.to_string()))?;
    // Match `status --porcelain` default (`-unormal`): collapse untracked dirs.
    let index = status::load_status_index().map_err(|e| to_err(e.to_string()))?;
    unstaged.new = status::collapse_untracked_directories(unstaged.new, &index);

    let mut buf: Vec<u8> = Vec::new();
    status::output_porcelain(&staged, &unstaged, false, &mut buf)
        .map_err(|e| to_err(e.to_string()))?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// If vault signing is enabled, sign the commit content and return the
/// formatted `gpgsig` header string. Returns `None` if vault is not configured.
/// Sign a commit using the libra vault PGP key.
///
/// When `force` is `false` the signature is only produced if `vault.signing`
/// is enabled in config (the default `libra commit` behavior). When `force`
/// is `true` the commit is signed regardless of `vault.signing` — used by
/// `cherry-pick -S`/`--gpg-sign`, which signs on explicit request. Returns
/// `Ok(None)` only when signing is not requested (or disabled and not forced).
pub(crate) async fn vault_sign_commit(
    tree_id: &ObjectHash,
    parent_ids: &[ObjectHash],
    author: &Signature,
    committer: &Signature,
    message: &str,
    force: bool,
) -> Result<Option<String>, CommitError> {
    use crate::internal::{config::ConfigKv, vault};

    // Check if vault signing is enabled (unless an explicit `--gpg-sign`
    // request forces it on).
    if !force {
        let signing_enabled = ConfigKv::get("vault.signing")
            .await
            .ok()
            .flatten()
            .map(|e| e.value);
        if signing_enabled.as_deref() != Some("true") {
            return Ok(None);
        }
    }

    // Load unseal key
    let unseal_key = vault::load_unseal_key().await.ok_or_else(|| {
        CommitError::VaultSign("vault signing enabled but no unseal key found".to_string())
    })?;

    // Build the commit content to sign (same format Git uses)
    let mut content: Vec<u8> = Vec::new();
    content.extend(b"tree ");
    content.extend(tree_id.to_string().as_bytes());
    content.extend(b"\n");
    for parent in parent_ids {
        content.extend(b"parent ");
        content.extend(parent.to_string().as_bytes());
        content.extend(b"\n");
    }
    let author_data = author.to_data().map_err(|e| {
        CommitError::VaultSign(format!(
            "failed to serialize author signature for vault signing: {e}"
        ))
    })?;
    content.extend(author_data);
    content.extend(b"\n");
    let committer_data = committer.to_data().map_err(|e| {
        CommitError::VaultSign(format!(
            "failed to serialize committer signature for vault signing: {e}"
        ))
    })?;
    content.extend(committer_data);
    content.extend(b"\n\n");
    content.extend(message.as_bytes());

    let root_dir = util::storage_path();

    let sig_hex = vault::pgp_sign(&root_dir, &unseal_key, &content)
        .await
        .map_err(|e| CommitError::VaultSign(format!("vault PGP signing failed: {e}")))?;
    let gpgsig = vault::signature_to_gpgsig(&sig_hex)
        .map_err(|e| CommitError::VaultSign(format!("failed to format PGP signature: {e}")))?;

    Ok(Some(gpgsig))
}

/// Result of checking a commit's embedded PGP signature (for
/// `merge --verify-signatures`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommitSignatureStatus {
    /// The commit carries no `gpgsig` header.
    Unsigned,
    /// The signature validates against the local vault PGP key.
    Good,
    /// A signature is present but does not validate.
    Bad,
}

/// Verify the PGP signature embedded in `commit`'s `gpgsig` header against the
/// local vault key. Reconstructs the exact bytes that were signed (the commit
/// content minus the signature — the same serialization [`vault_sign_commit`]
/// produces) and checks them via the vault.
///
/// Like `tag -v`, this can only validate signatures made by THIS repository's
/// vault key — Libra has no external GPG keyring, so a commit signed elsewhere
/// (or with an SSH signature) cannot be verified and reports [`CommitSignatureStatus::Bad`].
pub(crate) async fn verify_commit_signature(
    commit: &Commit,
) -> Result<CommitSignatureStatus, CommitError> {
    use crate::{common_utils::parse_commit_msg, internal::vault};

    let (_, signature) = parse_commit_msg(&commit.message);
    let Some(sig_block) = signature else {
        return Ok(CommitSignatureStatus::Unsigned);
    };

    // Recover the EXACT signed message body. `vault_sign_commit` signs the raw
    // message, and `format_commit_msg` stores it as `"{gpgsig}\n\n{message}"`, so
    // the message is the bytes immediately after the signature block and the single
    // blank-line separator — taken VERBATIM (NOT via `parse_commit_msg`, whose
    // `trim_start()` would drop leading whitespace and break verification of a
    // commit whose message starts with blanks/spaces).
    //
    // `sig_block` is a subslice of `commit.message`, so locate the body by the
    // block's END OFFSET rather than searching for marker text — the message body
    // itself may legitimately contain `-----END … SIGNATURE-----`, which a text
    // search could mis-select.
    let base = commit.message.as_ptr() as usize;
    let sig_end = (sig_block.as_ptr() as usize - base) + sig_block.len();
    debug_assert!(sig_end <= commit.message.len());
    let after_signature = &commit.message[sig_end..];
    let message = after_signature
        .strip_prefix("\n\n")
        .unwrap_or(after_signature);

    // Git prefixes each `gpgsig` continuation line with a single space; strip it
    // to recover the clean armored block `armored_to_signature_hex` expects.
    let armored: String = sig_block
        .lines()
        .map(|line| line.strip_prefix(' ').unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    let sig_hex = match vault::armored_to_signature_hex(&armored) {
        Ok(hex) => hex,
        // A malformed / non-PGP (e.g. SSH) signature block cannot be validated
        // against the vault PGP key — treat it as a bad signature, not an error.
        Err(_) => return Ok(CommitSignatureStatus::Bad),
    };

    // Reconstruct the signed content byte-for-byte, matching `vault_sign_commit`.
    let mut content: Vec<u8> = Vec::new();
    content.extend(b"tree ");
    content.extend(commit.tree_id.to_string().as_bytes());
    content.extend(b"\n");
    for parent in &commit.parent_commit_ids {
        content.extend(b"parent ");
        content.extend(parent.to_string().as_bytes());
        content.extend(b"\n");
    }
    let author_data = commit
        .author
        .to_data()
        .map_err(|e| CommitError::VaultSign(format!("failed to serialize author: {e}")))?;
    content.extend(author_data);
    content.extend(b"\n");
    let committer_data = commit
        .committer
        .to_data()
        .map_err(|e| CommitError::VaultSign(format!("failed to serialize committer: {e}")))?;
    content.extend(committer_data);
    content.extend(b"\n\n");
    content.extend(message.as_bytes());

    let unseal_key = vault::load_unseal_key().await.ok_or_else(|| {
        CommitError::VaultSign("signature verification requires a vault unseal key".to_string())
    })?;
    let root_dir = util::storage_path();
    let valid = vault::pgp_verify(&root_dir, &unseal_key, &content, &sig_hex)
        .await
        .map_err(|e| CommitError::VaultSign(format!("vault PGP verification failed: {e}")))?;

    Ok(if valid {
        CommitSignatureStatus::Good
    } else {
        CommitSignatureStatus::Bad
    })
}

/// recursively create tree from index's tracked entries
pub async fn create_tree(
    index: &Index,
    storage: &ClientStorage,
    current_root: PathBuf,
) -> Result<Tree, CommitError> {
    // blob created when add file to index
    let get_blob_entry = |path: &PathBuf| -> Result<TreeItem, CommitError> {
        let name = util::path_to_string(path);
        let mete = index.get(&name, 0).ok_or_else(|| {
            CommitError::TreeCreation(format!("failed to get index entry for {}", name))
        })?;
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                CommitError::TreeCreation(format!("invalid filename in path: {:?}", path))
            })?
            .to_string();

        Ok(TreeItem {
            name: filename,
            mode: TreeItemMode::tree_item_type_from_bytes(format!("{:o}", mete.mode).as_bytes())
                .map_err(|e| {
                    CommitError::TreeCreation(format!("invalid mode for {}: {}", name, e))
                })?,
            id: mete.hash,
        })
    };

    let mut tree_items: Vec<TreeItem> = Vec::new();
    let mut processed_path: HashSet<String> = HashSet::new();
    let path_entries: Vec<PathBuf> = index
        .tracked_entries(0)
        .iter()
        .map(|file| PathBuf::from(file.name.clone()))
        .filter(|path| path.starts_with(&current_root))
        .collect();
    for path in path_entries.iter() {
        let in_current_path = path
            .parent()
            .ok_or_else(|| CommitError::TreeCreation(format!("invalid path: {:?}", path)))?
            == current_root;
        if in_current_path {
            let item = get_blob_entry(path)?;
            tree_items.push(item);
        } else {
            if path.components().count() == 1 {
                continue;
            }
            // next level tree
            let process_path = path
                .components()
                .nth(current_root.components().count())
                .ok_or_else(|| {
                    CommitError::TreeCreation("failed to get next path component".to_string())
                })?
                .as_os_str()
                .to_str()
                .ok_or_else(|| CommitError::TreeCreation("invalid path component".to_string()))?;

            if processed_path.contains(process_path) {
                continue;
            }
            processed_path.insert(process_path.to_string());

            let sub_tree = Box::pin(create_tree(
                index,
                storage,
                current_root.clone().join(process_path),
            ))
            .await?;
            tree_items.push(TreeItem {
                name: process_path.to_string(),
                mode: TreeItemMode::Tree,
                id: sub_tree.id,
            });
        }
    }
    crate::utils::tree::sort_tree_items_for_git(&mut tree_items);
    let tree = {
        // `from_tree_items` can't create empty tree, so use `from_bytes` instead
        if tree_items.is_empty() {
            let empty_id = ObjectHash::from_type_and_data(ObjectType::Tree, &[]);
            Tree::from_bytes(&[], empty_id).map_err(|e| {
                CommitError::TreeCreation(format!("failed to create empty tree: {}", e))
            })?
        } else {
            Tree::from_tree_items(tree_items).map_err(|e| {
                CommitError::TreeCreation(format!("failed to create tree from items: {}", e))
            })?
        }
    };
    // save
    save_object_to_storage(storage, &tree, &tree.id)
        .map_err(|e| CommitError::TreeCreation(format!("failed to save tree object: {}", e)))?;
    Ok(tree)
}

fn auto_stage_tracked_changes() -> Result<bool, CommitError> {
    let pending = status::changes_to_be_staged().map_err(|e| {
        CommitError::AutoStage(format!("failed to determine working tree status: {e}"))
    })?;
    if pending.modified.is_empty() && pending.deleted.is_empty() {
        return Ok(false);
    }

    let index_path = path::index();
    let mut index = Index::load(&index_path)
        .map_err(|e| CommitError::IndexLoad(format!("failed to load index: {}", e)))?;
    let workdir = util::working_dir();
    let mut touched = false;

    for file in pending.modified {
        let abs = util::workdir_to_absolute(&file);
        if !abs.exists() {
            continue;
        }
        // Refresh blob IDs for modified tracked files before updating the index
        let blob = blob_from_file(&abs);
        blob.save();
        index.update(
            IndexEntry::new_from_file(&file, blob.id, &workdir).map_err(|e| {
                CommitError::AutoStage(format!("failed to create index entry: {}", e))
            })?,
        );
        touched = true;
    }

    for file in pending.deleted {
        if let Some(path) = file.to_str() {
            // Drop entries that disappeared from the working tree
            index.remove(path, 0);
            touched = true;
        }
    }

    if touched {
        index
            .save(&index_path)
            .map_err(|e| CommitError::IndexSave(format!("failed to save index: {}", e)))?;
    }
    Ok(touched)
}

fn blob_from_file(path: impl AsRef<std::path::Path>) -> Blob {
    if lfs::is_lfs_tracked(&path) {
        Blob::from_lfs_file(path)
    } else {
        Blob::from_file(path)
    }
}

/// Get the current HEAD commit ID as parent.
///
/// If on a branch, returns the branch's commit ID; if detached HEAD, returns the HEAD commit ID.
async fn get_parents_ids() -> Vec<ObjectHash> {
    let current_commit_id = Head::current_commit().await;
    match current_commit_id {
        Some(id) => vec![id],
        None => vec![], // first commit
    }
}

/// Update HEAD to point to a new commit.
///
/// If on a branch, updates the branch's commit ID; if detached HEAD, updates the HEAD reference.
async fn update_head<C: ConnectionTrait>(db: &C, commit_id: &str) -> Result<(), CommitError> {
    match Head::current_with_conn(db).await {
        Head::Branch(name) => {
            Branch::update_branch_with_conn(db, &name, commit_id, None)
                .await
                .map_err(|e| {
                    CommitError::HeadUpdate(format!("failed to update branch '{name}': {e}"))
                })?;
        }
        Head::Detached(_) => {
            let head = Head::Detached(
                ObjectHash::from_str(commit_id)
                    .map_err(|e| CommitError::HeadUpdate(format!("invalid commit id: {e}")))?,
            );
            Head::update_with_conn(db, head, None).await;
        }
    }
    Ok(())
}

async fn update_head_and_reflog(commit_id: &str, commit_message: &str) -> Result<(), CommitError> {
    let reflog_context = new_reflog_context(commit_id, commit_message).await;
    let commit_id = commit_id.to_string();
    with_reflog(
        reflog_context,
        |txn| {
            Box::pin(async move {
                update_head(txn, &commit_id)
                    .await
                    .map_err(|e| sea_orm::DbErr::Custom(e.to_string()))
            })
        },
        true,
    )
    .await
    .map_err(|e| CommitError::HeadUpdate(format!("failed to update reflog: {}", e)))
}

async fn new_reflog_context(commit_id: &str, message: &str) -> ReflogContext {
    // INVARIANT: zero-filled bytes of the correct hash size always produce a valid ObjectHash
    let zero_hash =
        ObjectHash::from_bytes(&vec![0u8; get_hash_kind().size()]).expect("zero hash is valid");
    let old_oid = Head::current_commit()
        .await
        .unwrap_or(zero_hash)
        .to_string();
    let new_oid = commit_id.to_string();
    let action = ReflogAction::Commit {
        message: message.to_string(),
    };
    ReflogContext {
        old_oid,
        new_oid,
        action,
    }
}

#[cfg(test)]
mod test {
    use std::env;

    use git_internal::internal::object::{ObjectTrait, signature::Signature};
    use serial_test::serial;
    use tempfile::tempdir;
    use tokio::{fs::File, io::AsyncWriteExt};

    use super::*;

    #[test]
    fn parse_git_config_bool_handles_bool_int_and_suffixes() {
        // Boolean spellings.
        for t in ["true", "yes", "on", "TRUE", "On"] {
            assert_eq!(parse_git_config_bool(t), Some(true), "{t}");
        }
        for f in ["false", "no", "off", "FALSE"] {
            assert_eq!(parse_git_config_bool(f), Some(false), "{f}");
        }
        // Bare integers: non-zero is truthy (Git bool-or-int).
        assert_eq!(parse_git_config_bool("0"), Some(false));
        assert_eq!(parse_git_config_bool("1"), Some(true));
        assert_eq!(parse_git_config_bool("2"), Some(true));
        // k/m/g 1024-multiplier suffixes (case-insensitive), non-zero -> true.
        assert_eq!(parse_git_config_bool("1k"), Some(true));
        assert_eq!(parse_git_config_bool("1K"), Some(true));
        assert_eq!(parse_git_config_bool("0k"), Some(false));
        // Invalid values are rejected (None -> the caller makes it fatal).
        assert_eq!(parse_git_config_bool("garbage"), None);
        assert_eq!(parse_git_config_bool("1x"), None);
    }

    #[test]
    fn parse_cleanup_mode_is_case_insensitive_and_rejects_unknown() {
        assert_eq!(parse_cleanup_mode("strip"), Some(CleanupMode::Strip));
        assert_eq!(
            parse_cleanup_mode("WHITESPACE"),
            Some(CleanupMode::Whitespace)
        );
        assert_eq!(parse_cleanup_mode("verbatim"), Some(CleanupMode::Verbatim));
        assert_eq!(parse_cleanup_mode("scissors"), Some(CleanupMode::Scissors));
        assert_eq!(parse_cleanup_mode("default"), Some(CleanupMode::Default));
        assert_eq!(parse_cleanup_mode("bogus"), None);
    }
    use crate::utils::test::*;

    #[test]
    fn test_commit_error_nothing_to_commit_maps_to_repo_state() {
        let err: CliError = CommitError::NothingToCommit.into();
        assert_eq!(err.exit_code(), 128);
        assert_eq!(err.stable_code().as_str(), "LBR-REPO-003");
        assert!(err.message().contains("nothing to commit"));
    }

    /// Pin the `Display` format for the static-message and direct-message
    /// variants of [`CommitError`]. These strings are used as the
    /// `CliError` message via `From<CommitError> for CliError` and
    /// surface in both human and `--json` envelopes (visible to scripts
    /// reading exit codes and JSON error blobs).
    ///
    /// Source-chained / wrapper variants (IndexLoad, IndexSave,
    /// TreeCreation, ObjectStorage, ParentCommitLoad, HeadUpdate,
    /// PreCommitHook, VaultSign, AutoStage, StagedChanges,
    /// MessageFileRead) wrap upstream error strings via `{0}` /
    /// `{detail}` and are intentionally skipped — their content is
    /// owned by the wrapped error type.
    #[test]
    fn commit_error_display_pins_static_message_variants() {
        assert_eq!(
            CommitError::NothingToCommit.to_string(),
            "nothing to commit, working tree clean",
        );
        assert_eq!(
            CommitError::NothingToCommitNoTracked.to_string(),
            "nothing to commit (create/copy files and use 'libra add' to track)",
        );
        assert_eq!(
            CommitError::IdentityMissing("set user.name and user.email".to_string()).to_string(),
            "set user.name and user.email",
        );
        assert_eq!(
            CommitError::NoCommitToAmend.to_string(),
            "there is no commit to amend",
        );
        assert_eq!(
            CommitError::AmendUnsupported.to_string(),
            "amend is not supported for merge commits with multiple parents",
        );
        assert_eq!(
            CommitError::InvalidAuthor("missing '<email>'".to_string()).to_string(),
            "invalid author format: missing '<email>'",
        );
        assert_eq!(
            CommitError::EmptyMessage.to_string(),
            "aborting commit due to empty commit message",
        );
        assert_eq!(
            CommitError::ConventionalCommit("subject too long".to_string()).to_string(),
            "conventional commit validation failed: subject too long",
        );
    }

    #[test]
    fn test_commit_error_identity_missing_maps_to_auth() {
        let err: CliError =
            CommitError::IdentityMissing("author identity unknown".to_string()).into();
        assert_eq!(err.exit_code(), 128);
        assert_eq!(err.stable_code().as_str(), "LBR-AUTH-001");
    }

    #[test]
    fn test_commit_error_no_commit_to_amend_maps_to_repo_state() {
        let err: CliError = CommitError::NoCommitToAmend.into();
        assert_eq!(err.exit_code(), 128);
        assert_eq!(err.stable_code().as_str(), "LBR-REPO-003");
    }

    #[test]
    fn test_commit_error_amend_unsupported_maps_to_repo_state() {
        let err: CliError = CommitError::AmendUnsupported.into();
        assert_eq!(err.exit_code(), 128);
        assert_eq!(err.stable_code().as_str(), "LBR-REPO-003");
    }

    #[test]
    fn test_commit_error_invalid_author_maps_to_cli_args() {
        let err: CliError = CommitError::InvalidAuthor("bad format".to_string()).into();
        assert_eq!(err.exit_code(), 129);
        assert_eq!(err.stable_code().as_str(), "LBR-CLI-002");
    }

    #[test]
    fn test_commit_error_tree_creation_maps_to_internal() {
        let err: CliError = CommitError::TreeCreation("unexpected".to_string()).into();
        assert_eq!(err.exit_code(), 128);
        assert_eq!(err.stable_code().as_str(), "LBR-INTERNAL-001");
    }

    #[test]
    fn test_commit_error_conventional_maps_to_cli_args() {
        let err: CliError = CommitError::ConventionalCommit("bad format".to_string()).into();
        assert_eq!(err.exit_code(), 129);
        assert_eq!(err.stable_code().as_str(), "LBR-CLI-002");
    }

    #[test]
    fn test_commit_error_pre_commit_hook_maps_to_repo_state() {
        let err: CliError = CommitError::PreCommitHook("hook failed".to_string()).into();
        assert_eq!(err.exit_code(), 128);
        assert_eq!(err.stable_code().as_str(), "LBR-REPO-003");
    }

    #[test]
    fn test_commit_error_vault_sign_maps_to_auth() {
        let err: CliError = CommitError::VaultSign("no key".to_string()).into();
        assert_eq!(err.exit_code(), 128);
        assert_eq!(err.stable_code().as_str(), "LBR-AUTH-001");
    }

    #[test]
    fn test_commit_error_index_load_maps_to_repo_corrupt() {
        let err: CliError = CommitError::IndexLoad("corrupted".to_string()).into();
        assert_eq!(err.exit_code(), 128);
        assert_eq!(err.stable_code().as_str(), "LBR-REPO-002");
    }

    #[test]
    fn test_commit_error_object_storage_maps_to_io_write() {
        let err: CliError = CommitError::ObjectStorage("disk full".to_string()).into();
        assert_eq!(err.exit_code(), 128);
        assert_eq!(err.stable_code().as_str(), "LBR-IO-002");
    }

    #[test]
    fn test_commit_error_parent_commit_load_maps_to_repo_corrupt() {
        let err: CliError = CommitError::ParentCommitLoad {
            commit_id: "abc1234".to_string(),
            detail: "missing object".to_string(),
        }
        .into();
        assert_eq!(err.exit_code(), 128);
        assert_eq!(err.stable_code().as_str(), "LBR-REPO-002");
    }

    #[test]
    fn test_commit_error_empty_message_maps_to_repo_state() {
        let err: CliError = CommitError::EmptyMessage.into();
        assert_eq!(err.exit_code(), 128);
        assert_eq!(err.stable_code().as_str(), "LBR-REPO-003");
    }

    #[test]
    fn test_commit_error_nothing_to_commit_no_tracked_maps_to_repo_state() {
        let err: CliError = CommitError::NothingToCommitNoTracked.into();
        assert_eq!(err.exit_code(), 128);
        assert_eq!(err.stable_code().as_str(), "LBR-REPO-003");
    }

    #[test]
    fn test_commit_error_index_save_maps_to_io_write() {
        let err: CliError = CommitError::IndexSave("disk full".to_string()).into();
        assert_eq!(err.exit_code(), 128);
        assert_eq!(err.stable_code().as_str(), "LBR-IO-002");
    }

    #[test]
    fn test_commit_error_message_file_read_maps_to_io_read() {
        let err: CliError = CommitError::MessageFileRead {
            path: "msg.txt".to_string(),
            detail: "not found".to_string(),
        }
        .into();
        assert_eq!(err.exit_code(), 128);
        assert_eq!(err.stable_code().as_str(), "LBR-IO-001");
    }

    #[test]
    fn test_commit_error_auto_stage_maps_to_io_read() {
        let err: CliError = CommitError::AutoStage("failed".to_string()).into();
        assert_eq!(err.exit_code(), 128);
        assert_eq!(err.stable_code().as_str(), "LBR-IO-001");
    }

    #[test]
    fn test_commit_error_staged_changes_maps_to_repo_corrupt() {
        let err: CliError = CommitError::StagedChanges("failed".to_string()).into();
        assert_eq!(err.exit_code(), 128);
        assert_eq!(err.stable_code().as_str(), "LBR-REPO-002");
    }

    #[test]
    fn test_commit_error_head_update_maps_to_io_write() {
        let err: CliError = CommitError::HeadUpdate("failed".to_string()).into();
        assert_eq!(err.exit_code(), 128);
        assert_eq!(err.stable_code().as_str(), "LBR-IO-002");
    }

    #[test]
    ///Testing basic parameter parsing functionality.
    fn test_parse_args() {
        let args = CommitArgs::try_parse_from(["commit", "-m", "init"]);
        assert!(args.is_ok());

        let args = CommitArgs::try_parse_from(["commit", "-m", "init", "--allow-empty"]);
        assert!(args.is_ok());

        let args = CommitArgs::try_parse_from(["commit", "--conventional", "-m", "init"]);
        assert!(args.is_ok());

        // Since PR-15, no message source is required at parse time: the editor is
        // opened at runtime (or the commit aborts when no editor is available).
        let args = CommitArgs::try_parse_from(["commit", "--conventional"]);
        assert!(args.is_ok(), "message is now optional (editor authoring)");

        let args = CommitArgs::try_parse_from(["commit"]);
        assert!(args.is_ok(), "bare commit parses (opens the editor)");

        let args = CommitArgs::try_parse_from(["commit", "-m", "init", "--amend"]);
        assert!(args.is_ok());
        let args = CommitArgs::try_parse_from(["commit", "--amend", "--no-edit"]);
        assert!(args.is_ok());
        // --no-edit no longer requires --amend and may carry -m.
        let args = CommitArgs::try_parse_from(["commit", "--no-edit"]);
        assert!(args.is_ok(), "--no-edit no longer requires --amend");
        let args = CommitArgs::try_parse_from(["commit", "--no-edit", "-m", "init"]);
        assert!(args.is_ok(), "--no-edit may coexist with -m");
        // -e and --no-edit are mutually exclusive.
        let args = CommitArgs::try_parse_from(["commit", "-e", "--no-edit", "-m", "x"]);
        assert!(args.is_err(), "--edit conflicts with --no-edit");
        let args = CommitArgs::try_parse_from(["commit", "-m", "init", "--allow-empty", "--amend"]);
        assert!(args.is_ok());
        let args = CommitArgs::try_parse_from(["commit", "-m", "init", "-s"]);
        assert!(args.is_ok());
        assert!(args.unwrap().signoff);

        let args = CommitArgs::try_parse_from(["commit", "-m", "init", "--signoff"]);
        assert!(args.is_ok());
        assert!(args.unwrap().signoff);

        let args = CommitArgs::try_parse_from(["commit", "-m", "init", "-a"]);
        assert!(args.is_ok());
        assert!(args.unwrap().all);

        let args = CommitArgs::try_parse_from(["commit", "-m", "init", "--all"]);
        assert!(args.is_ok());
        assert!(args.unwrap().all);

        // Since PR-15, --no-edit may coexist with --message/--file (it just
        // suppresses the editor; the supplied message is used directly).
        let args = CommitArgs::try_parse_from(["commit", "-m", "init", "--amend", "--no-edit"]);
        assert!(args.is_ok(), "--no-edit may coexist with --message");
        let args = CommitArgs::try_parse_from(["commit", "-F", "init", "--amend", "--no-edit"]);
        assert!(args.is_ok(), "--no-edit may coexist with --file");
        let args = CommitArgs::try_parse_from(["commit", "-m", "init", "--amend", "--signoff"]);
        assert!(args.is_ok());
        let args = args.unwrap();
        assert!(args.amend);
        assert!(args.signoff);

        let args = CommitArgs::try_parse_from(["commit", "-F", "unreachable_file"]);
        assert!(args.is_ok());
        assert!(args.unwrap().file.is_some());

        let args = CommitArgs::try_parse_from(["commit", "-m", "init", "--no-verify"]);
        assert!(args.is_ok(), "--no-verify should be a valid parameter");

        let args =
            CommitArgs::try_parse_from(["commit", "-m", "init", "--conventional", "--no-verify"]);
        assert!(args.is_ok(), "--no-verify should work with --conventional");

        let args = CommitArgs::try_parse_from(["commit", "-m", "init", "--amend", "--no-verify"]);
        assert!(args.is_ok(), "--no-verify should work with --amend");

        let args = CommitArgs::try_parse_from([
            "commit",
            "-m",
            "init",
            "--author",
            "Test User <test@example.com>",
        ]);
        assert!(args.is_ok(), "--author should be a valid parameter");
        let args = args.unwrap();
        assert_eq!(
            args.author,
            Some("Test User <test@example.com>".to_string())
        );

        let args = CommitArgs::try_parse_from([
            "commit",
            "-m",
            "init",
            "--author",
            "Test User <test@example.com>",
            "--amend",
        ]);
        assert!(args.is_ok(), "--author should work with --amend");
    }

    #[test]
    fn test_parse_author() {
        // Valid author formats
        let (name, email) = parse_author("John Doe <john@example.com>").unwrap();
        assert_eq!(name, "John Doe");
        assert_eq!(email, "john@example.com");

        let (name, email) = parse_author("  Jane Smith  <jane@test.org>  ").unwrap();
        assert_eq!(name, "Jane Smith");
        assert_eq!(email, "jane@test.org");

        let (name, email) = parse_author("Multi Word Name <multi@word.com>").unwrap();
        assert_eq!(name, "Multi Word Name");
        assert_eq!(email, "multi@word.com");

        // Invalid formats should return CommitError::InvalidAuthor
        assert!(matches!(
            parse_author("invalid"),
            Err(CommitError::InvalidAuthor(_))
        ));
        assert!(matches!(
            parse_author("No Email"),
            Err(CommitError::InvalidAuthor(_))
        ));
        assert!(matches!(
            parse_author("<noemail@test.com>"),
            Err(CommitError::InvalidAuthor(_))
        ));
        assert!(matches!(
            parse_author("Name <"),
            Err(CommitError::InvalidAuthor(_))
        ));
    }

    #[test]
    fn test_commit_message() {
        let args = CommitArgs {
            message: None,
            file: None,
            allow_empty: false,
            conventional: false,
            amend: true,
            no_edit: true,
            signoff: false,
            disable_pre: false,
            all: false,
            no_verify: false,
            author: None,
            ..Default::default()
        };
        fn message_and_file_are_none(args: &CommitArgs) -> Option<String> {
            match (&args.message, &args.file) {
                (Some(msg), _) => Some(msg.clone()),
                (None, Some(file)) => Some(file.clone()),
                (None, None) => {
                    if args.no_edit {
                        Some("".to_string())
                    } else {
                        None
                    }
                }
            }
        }
        let message = message_and_file_are_none(&args);
        assert_eq!(message, Some("".to_string()));
    }

    #[tokio::test]
    #[serial]
    async fn test_commit_message_from_file() {
        let temp_dir = tempdir().unwrap();
        let test_path = temp_dir.path().join("test_data.txt");

        let test_cases = vec![
            "Hello, World! 你好，世界！",
            "Special chars: \n\t\r\\\"'",
            "Emoji: 😀🎉🚀, Unicode:  Café café",
            "",
            "Mix: 中文\n\tEmoji😀\rSpecial\\\"'",
        ];

        for test_data in test_cases {
            let bytes = test_data.as_bytes();
            let mut file = File::create(&test_path).await.expect("create file failed");
            file.write_all(bytes)
                .await
                .expect("write test data to file failed");
            file.sync_all()
                .await
                .expect("write test data to file failed");

            let content = tokio::fs::read_to_string(&test_path).await.unwrap();

            let author = Signature {
                signature_type: git_internal::internal::object::signature::SignatureType::Author,
                name: "test".to_string(),
                email: "test".to_string(),
                timestamp: 1,
                timezone: "test".to_string(),
            };

            let commiter = Signature {
                signature_type: git_internal::internal::object::signature::SignatureType::Committer,
                name: "test".to_string(),
                email: "test".to_string(),
                timestamp: 1,
                timezone: "test".to_string(),
            };

            let zero = ObjectHash::from_bytes(&vec![0u8; get_hash_kind().size()]).unwrap();
            let commit = Commit::new(author, commiter, zero, Vec::new(), &content);

            let commit_data = commit.to_data().unwrap();

            let message = Commit::from_bytes(&commit_data, commit.id).unwrap().message;

            assert_eq!(message, test_data);
        }
    }

    #[tokio::test]
    #[serial]
    // Tests the recursive tree creation from index entries (uses original test data via absolute path)
    async fn test_create_tree() {
        // 1. Initialize a temporary Libra repository
        let temp_path = tempdir().unwrap();
        setup_with_new_libra_in(temp_path.path()).await;
        let _guard = ChangeDirGuard::new(temp_path.path());

        // 2. Build absolute path to the test index file using the project root (CARGO_MANIFEST_DIR)
        let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let index_file_path = project_root.join("tests/data/index/index-760");

        // 3. Verify the test fixture exists
        assert!(
            index_file_path.exists(),
            "test fixture not found: {}; please place the index-760 file at that path",
            index_file_path.display()
        );

        // 4. Load the index file
        let index = Index::from_file(index_file_path).unwrap_or_else(|e| {
            panic!(
                "failed to load index file: {}; verify the file format is correct",
                e
            );
        });
        println!(
            "loaded index contains {} tracked entries",
            index.tracked_entries(0).len()
        );

        // 5. Initialize storage pointing at the temp repo's objects directory
        let temp_objects_dir = temp_path.path().join(".libra/objects");
        let storage = ClientStorage::init(temp_objects_dir);

        // 6. Call create_tree with an empty root (index paths are repo-root-relative)
        let tree = create_tree(&index, &storage, PathBuf::new()).await.unwrap();

        // 7. Verify tree structure
        assert!(
            storage.get(&tree.id).is_ok(),
            "root tree not saved to storage"
        );
        for item in tree.tree_items.iter() {
            if item.mode == TreeItemMode::Tree {
                assert!(
                    storage.get(&item.id).is_ok(),
                    "sub-tree not saved: {}",
                    item.name
                );
                if item.name == "DeveloperExperience" {
                    let sub_tree_data = storage.get(&item.id).unwrap();
                    let sub_tree = Tree::from_bytes(&sub_tree_data, item.id).unwrap();
                    assert_eq!(
                        sub_tree.tree_items.len(),
                        4,
                        "DeveloperExperience sub-tree entry count mismatch"
                    );
                }
            }
        }
    }

    #[test]
    fn test_no_verify_skips_conventional_check() {
        let invalid_conventional_msg = "invalid commit: no type or scope";
        assert!(
            !check_conventional_commits_message(invalid_conventional_msg),
            "Test setup error: message should be invalid for conventional commits"
        );

        let args_with_verify = CommitArgs {
            message: Some(invalid_conventional_msg.to_string()),
            file: None,
            allow_empty: true,
            conventional: true,
            no_verify: false,
            amend: false,
            no_edit: false,
            signoff: false,
            disable_pre: false,
            all: false,
            author: None,
            ..Default::default()
        };

        let commit_message_with_verify = if args_with_verify.signoff {
            format!(
                "{}\n\nSigned-off-by: test <test@example.com>",
                invalid_conventional_msg
            )
        } else {
            invalid_conventional_msg.to_string()
        };

        let verify_result = std::panic::catch_unwind(|| {
            if args_with_verify.conventional
                && !args_with_verify.no_verify
                && !check_conventional_commits_message(&commit_message_with_verify)
            {
                panic!("fatal: commit message does not follow conventional commits");
            }
        });
        assert!(
            verify_result.is_err(),
            "Conventional check should fail without --no-verify"
        );

        let args_no_verify = CommitArgs {
            no_verify: true,
            ..args_with_verify
        };

        let commit_message_no_verify = if args_no_verify.signoff {
            format!(
                "{}\n\nSigned-off-by: test <test@example.com>",
                invalid_conventional_msg
            )
        } else {
            invalid_conventional_msg.to_string()
        };

        let no_verify_result = std::panic::catch_unwind(|| {
            if args_no_verify.conventional
                && !args_no_verify.no_verify
                && !check_conventional_commits_message(&commit_message_no_verify)
            {
                panic!("fatal: commit message does not follow conventional commits");
            }
        });
        assert!(
            no_verify_result.is_ok(),
            "--no-verify should skip conventional check"
        );
    }

    /// Cross-Cutting G: `TreeCreation` is the lone CommitError variant
    /// that maps to `InternalInvariant`. It must include the GitHub
    /// Issues URL hint so users can report the bug.
    #[test]
    fn test_commit_error_tree_creation_has_issue_url_hint() {
        let err: CliError =
            CommitError::TreeCreation("synthetic tree-build failure".to_string()).into();
        assert_eq!(err.stable_code(), StableErrorCode::InternalInvariant);
        assert!(
            err.hints().iter().any(|h| h.as_str().contains("issues")),
            "TreeCreation must include the GitHub Issues URL hint, got hints: {:?}",
            err.hints()
        );
    }
}
