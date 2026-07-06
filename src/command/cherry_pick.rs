//! Applies commits onto the current branch by replaying their changes into the index/worktree and emitting new commits or conflict notices.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    io::IsTerminal,
    path::{Path, PathBuf},
    str::FromStr,
};

use clap::Parser;
use git_internal::{
    hash::ObjectHash,
    internal::{
        index::{Index, IndexEntry},
        object::{ObjectTrait, commit::Commit, tree::Tree, types::ObjectType},
    },
};
use sea_orm::ConnectionTrait;
use serde::Serialize;

use crate::{
    command::{
        commit::{CleanupMode, cleanup_commit_message, parse_cleanup_mode},
        load_object, merge, save_object,
    },
    common_utils::format_commit_msg,
    internal::{
        branch::Branch,
        config::ConfigKv,
        head::Head,
        reflog::{ReflogAction, ReflogContext, with_reflog},
        sequencer::{self, SequenceKind, SequenceState},
        tree_plumbing,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        object_ext::{BlobExt, TreeExt},
        output::{OutputConfig, emit_json_data},
        path,
        text::short_display_hash,
        util, worktree,
    },
};

/// A divergent path recorded during replay: `(path, ours blob, theirs blob,
/// base blob)`. Each blob side is `None` when that side has no content (an
/// add/delete on that side). The base feeds the line-level conflict merge.
type ConflictEntry = (
    PathBuf,
    Option<ObjectHash>,
    Option<ObjectHash>,
    Option<ObjectHash>,
);

const CHERRY_PICK_EXAMPLES: &str = "\
EXAMPLES:
    libra cherry-pick abc1234              Apply a single commit
    libra cherry-pick abc1234 def5678      Apply multiple commits in order
    libra cherry-pick -n abc1234           Apply without auto-committing
    libra cherry-pick -x abc1234           Append a '(cherry picked from ...)' line
    libra cherry-pick -s abc1234           Add a Signed-off-by trailer
    libra cherry-pick -m 1 <merge>         Cherry-pick a merge commit along parent 1
    libra cherry-pick --cleanup=strip abc1234  Clean up the replayed commit message
    libra cherry-pick --empty=drop abc1234  Skip the pick if it is already upstream
    libra cherry-pick --continue           Resume after resolving conflicts
    libra cherry-pick --abort              Cancel and restore the original HEAD
    libra cherry-pick --json abc1234       Structured JSON output for agents";

// ── Typed error ──────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
enum CherryPickError {
    #[error("not a libra repository")]
    NotInRepo,

    #[error("cannot cherry-pick on detached HEAD")]
    DetachedHead,

    #[error("failed to resolve commit reference '{0}'")]
    InvalidCommit(String),

    #[error("cherry-picking merge commits is not supported")]
    MergeCommitUnsupported,

    #[error("{0}")]
    InvalidMainline(String),

    #[error("invalid --cleanup mode '{0}'")]
    InvalidCleanup(String),

    #[error("invalid value for '--empty': '{0}'")]
    InvalidEmpty(String),

    #[error("unsupported cherry-pick option: {0}")]
    Unsupported(String),

    #[error("commit {0} is empty (its change set is empty)")]
    EmptyCommit(String),

    #[error("commit {0} became redundant after replay (no changes to apply)")]
    RedundantCommit(String),

    #[error("commit {0} has an empty commit message")]
    EmptyMessage(String),

    #[error("failed to cherry-pick {commit}: {reason}")]
    Conflict { commit: String, reason: String },

    #[error("a cherry-pick is already in progress")]
    InProgress,

    #[error("no cherry-pick in progress")]
    NoCherryPickInProgress,

    #[error(
        "the current branch '{current}' does not match the in-progress cherry-pick branch '{expected}'"
    )]
    WrongBranch { current: String, expected: String },

    #[error("failed to load cherry-pick state: {0}")]
    LoadObject(String),

    #[error("failed to update cherry-pick state: {0}")]
    SaveFailed(String),

    /// The repository configures an unsupported `merge.conflictStyle` value —
    /// a hard error before any conflicted index/worktree state is written,
    /// consistent with `libra merge`.
    #[error("unsupported merge.conflictStyle '{0}' (expected 'merge' or 'diff3')")]
    InvalidConflictStyle(String),

    /// The `merge.conflictStyle` config could not be read (config-store I/O
    /// failure) — never a silent default-style fall-back.
    #[error("failed to read merge.conflictStyle config: {0}")]
    ConflictStyleRead(String),
}

impl CherryPickError {
    fn stable_code(&self) -> StableErrorCode {
        match self {
            Self::NotInRepo => StableErrorCode::RepoNotFound,
            Self::DetachedHead => StableErrorCode::RepoStateInvalid,
            Self::InvalidCommit(_) => StableErrorCode::CliInvalidTarget,
            Self::MergeCommitUnsupported => StableErrorCode::CliInvalidArguments,
            Self::InvalidMainline(_) => StableErrorCode::CliInvalidArguments,
            Self::InvalidCleanup(_) => StableErrorCode::CliInvalidArguments,
            Self::InvalidEmpty(_) => StableErrorCode::CliInvalidArguments,
            Self::Unsupported(_) => StableErrorCode::Unsupported,
            Self::EmptyCommit(_) => StableErrorCode::CliInvalidArguments,
            Self::RedundantCommit(_) => StableErrorCode::CliInvalidArguments,
            Self::EmptyMessage(_) => StableErrorCode::CliInvalidArguments,
            Self::Conflict { .. } => StableErrorCode::ConflictUnresolved,
            Self::InProgress => StableErrorCode::ConflictOperationBlocked,
            Self::NoCherryPickInProgress => StableErrorCode::RepoStateInvalid,
            Self::WrongBranch { .. } => StableErrorCode::RepoStateInvalid,
            Self::LoadObject(_) => StableErrorCode::IoReadFailed,
            Self::SaveFailed(_) => StableErrorCode::IoWriteFailed,
            Self::InvalidConflictStyle(_) => StableErrorCode::RepoStateInvalid,
            Self::ConflictStyleRead(_) => StableErrorCode::IoReadFailed,
        }
    }
}

impl From<CherryPickError> for CliError {
    fn from(error: CherryPickError) -> Self {
        let stable_code = error.stable_code();
        let message = error.to_string();
        match error {
            CherryPickError::NotInRepo => CliError::repo_not_found(),
            CherryPickError::DetachedHead => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("switch to a branch first with 'libra switch <branch>'"),
            CherryPickError::InvalidCommit(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("use 'libra log' to find valid commit references"),
            CherryPickError::MergeCommitUnsupported => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("specify -m <parent-number> to cherry-pick a merge commit"),
            CherryPickError::InvalidMainline(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("use -m <parent-number> only on a merge commit, within its parent count"),
            CherryPickError::InvalidCleanup(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("valid modes: strip, whitespace, verbatim, scissors, default"),
            CherryPickError::InvalidEmpty(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("valid modes: drop, keep, stop"),
            CherryPickError::Unsupported(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("this Git option is not supported by libra cherry-pick"),
            CherryPickError::EmptyCommit(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("use --allow-empty to cherry-pick an empty commit"),
            CherryPickError::RedundantCommit(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("use --keep-redundant-commits to keep the redundant commit"),
            CherryPickError::EmptyMessage(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("use --allow-empty-message to cherry-pick with an empty message"),
            CherryPickError::Conflict { .. } => CliError::failure(message)
                .with_stable_code(stable_code)
                .with_hint(
                    "resolve conflicts and 'libra add' them, then 'libra cherry-pick --continue' \
                     (or --skip / --abort / --quit)",
                ),
            CherryPickError::InProgress => CliError::failure(message)
                .with_stable_code(stable_code)
                .with_hint(
                    "finish it with 'libra cherry-pick --continue'/--skip, or cancel with --abort/--quit",
                ),
            CherryPickError::NoCherryPickInProgress => CliError::failure(message)
                .with_stable_code(stable_code)
                .with_hint("there is no cherry-pick to --continue/--skip/--abort/--quit"),
            CherryPickError::WrongBranch { expected, .. } => CliError::failure(message)
                .with_stable_code(stable_code)
                .with_hint(format!("switch back to '{expected}' before continuing")),
            CherryPickError::LoadObject(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("check repository integrity and retry"),
            CherryPickError::SaveFailed(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("check filesystem permissions and repository writability"),
            CherryPickError::InvalidConflictStyle(_) => CliError::failure(message)
                .with_stable_code(stable_code)
                .with_hint("set merge.conflictStyle to 'merge' (default) or 'diff3'"),
            CherryPickError::ConflictStyleRead(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("check repository integrity and retry"),
        }
    }
}

#[derive(Debug)]
enum CherryPickSingleError {
    MergeCommitUnsupported,
    InvalidMainline(String),
    EmptyCommit(String),
    RedundantCommit(String),
    EmptyMessage(String),
    /// A real three-way conflict: the listed paths were written to the index
    /// (stages 1/2/3) and worktree (conflict markers). The caller persists the
    /// sequencer state (commit-per-pick mode) before exiting.
    Conflicted(Vec<String>),
    Conflict(String),
    LoadObject(String),
    SaveFailed(String),
    /// Unsupported `merge.conflictStyle` value — raised BEFORE the conflicted
    /// index/worktree state is written, so nothing is mutated.
    InvalidConflictStyle(String),
    /// `merge.conflictStyle` config-store read failure.
    ConflictStyleRead(String),
}

/// Serializable snapshot of the commit-modifier options for a cherry-pick
/// sequence, persisted in the sequencer payload so `--continue`/`--skip`
/// rebuild the same commit shape after a conflict.
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
struct CherryPickOpts {
    #[serde(default)]
    append_source: bool,
    #[serde(default)]
    signoff: bool,
    #[serde(default)]
    edit: bool,
    #[serde(default)]
    allow_empty: bool,
    #[serde(default)]
    allow_empty_message: bool,
    #[serde(default)]
    keep_redundant_commits: bool,
    #[serde(default)]
    gpg_sign: bool,
    /// `--rerere-autoupdate`: stage a rerere-replayed resolution. Persisted so a
    /// later pick in a resumed sequence keeps auto-staging replayed conflicts.
    #[serde(default)]
    rerere_autoupdate: bool,
    /// Mainline parent for merge-commit picks; applies to every commit in the
    /// `-m <n>` invocation, so it must survive a conflict + resume.
    #[serde(default)]
    mainline: Option<usize>,
    /// Message cleanup mode; must survive a conflict + resume so resumed commits
    /// clean their message the same way. Stored as the raw mode string.
    #[serde(default)]
    cleanup: Option<String>,
    /// `--empty=<mode>` policy; must survive a conflict + resume so a later
    /// redundant commit in the sequence is handled the same way. Raw mode string.
    #[serde(default)]
    empty: Option<String>,
}

impl CherryPickOpts {
    fn from_args(args: &CherryPickArgs) -> Self {
        Self {
            append_source: args.append_source,
            signoff: args.signoff,
            edit: args.edit,
            allow_empty: args.allow_empty,
            allow_empty_message: args.allow_empty_message,
            keep_redundant_commits: args.keep_redundant_commits,
            gpg_sign: args.gpg_sign,
            rerere_autoupdate: args.rerere_autoupdate,
            mainline: args.mainline,
            cleanup: args.cleanup.clone(),
            empty: args.empty.clone(),
        }
    }

    /// Rebuild a minimal [`CherryPickArgs`] carrying just these options (used to
    /// re-run the commit-assembly path during `--continue`/`--skip`). EVERY
    /// commit-shaping modifier must round-trip so resumed commits keep the same
    /// shape — e.g. a `-S` sequence stays signed and a `-m <n>` sequence keeps
    /// applying later merge commits along the chosen parent.
    fn into_args(self) -> CherryPickArgs {
        CherryPickArgs {
            append_source: self.append_source,
            signoff: self.signoff,
            edit: self.edit,
            allow_empty: self.allow_empty,
            allow_empty_message: self.allow_empty_message,
            keep_redundant_commits: self.keep_redundant_commits,
            gpg_sign: self.gpg_sign,
            rerere_autoupdate: self.rerere_autoupdate,
            mainline: self.mainline,
            cleanup: self.cleanup,
            empty: self.empty,
            ..Default::default()
        }
    }
}

/// Policy for a pick whose change set becomes redundant against HEAD after
/// replay (Git's `--empty=<mode>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmptyMode {
    /// Halt and let the user decide (Git's default).
    Stop,
    /// Skip the redundant commit and continue (`--empty=drop`).
    Drop,
    /// Keep the now-empty commit (`--empty=keep`, == `--keep-redundant-commits`).
    Keep,
}

/// Parse a `--empty=<mode>` value; `None` for an unrecognized mode.
fn parse_empty_mode(value: &str) -> Option<EmptyMode> {
    match value {
        "stop" => Some(EmptyMode::Stop),
        "drop" => Some(EmptyMode::Drop),
        "keep" => Some(EmptyMode::Keep),
        _ => None,
    }
}

/// The effective become-redundant policy: `--empty=<mode>` wins; otherwise
/// `--keep-redundant-commits` means `keep` and the default is `stop` (matching
/// Git). Assumes `--empty` was already validated by [`run_cherry_pick`], so an
/// unexpected value defaults to `stop`.
fn effective_empty_mode(args: &CherryPickArgs) -> EmptyMode {
    if let Some(raw) = &args.empty {
        return parse_empty_mode(raw).unwrap_or(EmptyMode::Stop);
    }
    if args.keep_redundant_commits {
        EmptyMode::Keep
    } else {
        EmptyMode::Stop
    }
}

/// Outcome of picking a single commit.
enum PickOutcome {
    /// A new commit was created (or HEAD fast-forwarded) at this id.
    Committed(ObjectHash),
    /// `--no-commit`: changes staged, no commit created.
    Staged,
    /// `--empty=drop`: the pick was redundant against HEAD and was skipped. Carries
    /// the dropped commit's subject for the `dropping … -- patch contents already
    /// upstream` notice.
    Dropped(String),
}

// ── Structured output ────────────────────────────────────────────────

/// A commit skipped under `--empty=drop` (its patch is already upstream).
#[derive(Debug, Clone, Serialize)]
pub struct DroppedCommit {
    pub commit: String,
    pub subject: String,
}

/// Running tally of a pick sequence: committed/staged commits and the commits
/// dropped under `--empty=drop`. Bundled so the sequencer functions take one
/// accumulator rather than two parallel out-parameters.
#[derive(Default)]
struct PickAccumulator {
    picked: Vec<CherryPickEntry>,
    dropped: Vec<DroppedCommit>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CherryPickOutput {
    pub picked: Vec<CherryPickEntry>,
    pub no_commit: bool,
    /// Commits skipped under `--empty=drop` (additive; absent when none).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped: Vec<DroppedCommit>,
    /// Sequencer action: `"continue"`/`"skip"`/`"abort"`/`"quit"`. Absent for a
    /// plain pick (back-compatible: old consumers see the same `{picked,no_commit}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// OID `--abort` restored HEAD to (only set for the abort action).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restored_head: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CherryPickEntry {
    pub source_commit: String,
    pub short_source: String,
    pub new_commit: Option<String>,
    pub short_new: Option<String>,
}

// ── Entry points ─────────────────────────────────────────────────────

/// Arguments for the cherry-pick command
#[derive(Parser, Debug, Default)]
#[command(about = "Apply the changes introduced by some existing commits")]
#[command(after_help = CHERRY_PICK_EXAMPLES)]
pub struct CherryPickArgs {
    /// Commits to cherry-pick
    #[clap(required_unless_present_any = ["continue_pick", "skip", "abort", "quit"])]
    pub commits: Vec<String>,

    /// Don't automatically commit the cherry-pick
    #[clap(short = 'n', long)]
    pub no_commit: bool,

    /// Resume the in-progress cherry-pick after resolving conflicts
    #[clap(
        long = "continue",
        conflicts_with_all = ["commits", "skip", "abort", "quit", "no_commit"]
    )]
    pub continue_pick: bool,

    /// Skip the current conflicted commit and continue the sequence
    #[clap(
        long = "skip",
        conflicts_with_all = ["commits", "continue_pick", "abort", "quit", "no_commit"]
    )]
    pub skip: bool,

    /// Abort the in-progress cherry-pick and restore the original HEAD
    #[clap(
        long = "abort",
        conflicts_with_all = ["commits", "continue_pick", "skip", "quit", "no_commit"]
    )]
    pub abort: bool,

    /// Forget the in-progress cherry-pick without changing the working tree
    #[clap(
        long = "quit",
        conflicts_with_all = ["commits", "continue_pick", "skip", "abort", "no_commit"]
    )]
    pub quit: bool,

    /// Append a "(cherry picked from commit <oid>)" line to the commit message
    #[clap(short = 'x')]
    pub append_source: bool,

    /// Add a Signed-off-by trailer to the commit message
    #[clap(short = 's', long = "signoff", overrides_with = "no_signoff")]
    pub signoff: bool,

    /// Edit the commit message before committing
    #[clap(short = 'e', long = "edit", overrides_with = "no_edit")]
    pub edit: bool,

    /// Cherry-pick a commit even if its own change set is empty
    #[clap(long = "allow-empty", overrides_with = "no_allow_empty")]
    pub allow_empty: bool,

    /// Cherry-pick a commit even if its message is empty
    #[clap(
        long = "allow-empty-message",
        overrides_with = "no_allow_empty_message"
    )]
    pub allow_empty_message: bool,

    /// Keep commits that become redundant (empty) after being replayed
    #[clap(
        long = "keep-redundant-commits",
        overrides_with = "no_keep_redundant_commits"
    )]
    pub keep_redundant_commits: bool,

    /// Parent number (1-based) to follow when cherry-picking a merge commit
    #[clap(short = 'm', long = "mainline", value_name = "parent-number")]
    pub mainline: Option<usize>,

    /// Fast-forward when the picked commit is a direct child of HEAD
    #[clap(long = "ff", overrides_with = "no_ff")]
    pub ff: bool,

    /// GPG-sign the cherry-picked commit using the vault signing key
    #[clap(short = 'S', long = "gpg-sign", overrides_with = "no_gpg_sign")]
    pub gpg_sign: bool,

    // ── Negative (reset-to-default) forms; last flag wins, never an error ──
    #[clap(long = "no-signoff", overrides_with = "signoff", hide = true)]
    pub no_signoff: bool,
    #[clap(long = "no-edit", overrides_with = "edit", hide = true)]
    pub no_edit: bool,
    #[clap(long = "no-allow-empty", overrides_with = "allow_empty", hide = true)]
    pub no_allow_empty: bool,
    #[clap(
        long = "no-allow-empty-message",
        overrides_with = "allow_empty_message",
        hide = true
    )]
    pub no_allow_empty_message: bool,
    #[clap(
        long = "no-keep-redundant-commits",
        overrides_with = "keep_redundant_commits",
        hide = true
    )]
    pub no_keep_redundant_commits: bool,
    #[clap(long = "no-ff", overrides_with = "ff", hide = true)]
    pub no_ff: bool,
    #[clap(long = "no-gpg-sign", overrides_with = "gpg_sign", hide = true)]
    pub no_gpg_sign: bool,

    /// How to clean up the commit message
    /// (`strip`/`whitespace`/`verbatim`/`scissors`/`default`). Cleans the picked
    /// body (and any `-e` edited buffer) first; the generated `-x`/`Signed-off-by`
    /// trailers are appended afterward so their separator is preserved.
    #[clap(long = "cleanup", value_name = "mode")]
    pub cleanup: Option<String>,

    /// How to handle a pick that becomes empty (redundant against HEAD) after
    /// replay: `stop` (default — halt for you to decide), `drop` (skip it), or
    /// `keep` (record the empty commit; same as `--keep-redundant-commits`).
    #[clap(long = "empty", value_name = "mode")]
    pub empty: Option<String>,

    // ── Unsupported Git options captured for explicit rejection ──
    #[clap(long = "strategy", value_name = "name", hide = true)]
    pub strategy: Option<String>,
    #[clap(
        long = "rerere-autoupdate",
        overrides_with = "no_rerere_autoupdate",
        hide = true
    )]
    pub rerere_autoupdate: bool,
    #[clap(
        long = "no-rerere-autoupdate",
        overrides_with = "rerere_autoupdate",
        hide = true
    )]
    pub no_rerere_autoupdate: bool,
    #[clap(long = "commit", hide = true)]
    pub commit: bool,
    #[clap(
        short = 'X',
        long = "strategy-option",
        value_name = "option",
        hide = true
    )]
    pub strategy_option: Option<String>,
}

pub async fn execute(args: CherryPickArgs) {
    if let Err(e) = execute_safe(args, &OutputConfig::default()).await {
        e.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting. Replays one or more commit changes onto the current
/// branch, optionally creating new commits or leaving them staged.
pub async fn execute_safe(args: CherryPickArgs, output: &OutputConfig) -> CliResult<()> {
    crate::command::ensure_main_worktree("cherry-pick")?;
    // Symmetric sequencer mutex (lore.md 2.6): a NEW cherry-pick is refused
    // while ANY other sequence (merge/revert/rebase) is unresolved. Control
    // verbs are exempt (they conclude the in-progress cherry-pick). Same-op
    // in-progress falls through to run_cherry_pick's typed InProgress check.
    if !(args.continue_pick || args.skip || args.abort || args.quit) {
        sequencer::ensure_none_in_progress(SequenceKind::CherryPick).await?;
    }
    let result = run_cherry_pick(args, output)
        .await
        .map_err(CliError::from)?;
    render_cherry_pick_output(&result, output)
}

// ── Core execution ───────────────────────────────────────────────────

/// Reject Git options libra cherry-pick does not implement. Returns the first
/// offending flag (so the error names a concrete option) or `None`.
fn reject_unsupported_options(args: &CherryPickArgs) -> Option<&'static str> {
    // `--rerere-autoupdate` is honoured: when `rerere.enabled` is set it makes
    // the rerere hook stage a replayed resolution. With rerere disabled it is a
    // harmless no-op, matching Git's behaviour when rerere is off.
    if args.commit {
        return Some("--commit (auto-commit is the default; use -n to stage only)");
    }
    if args.strategy_option.is_some() {
        return Some("-X / --strategy-option");
    }
    if args.strategy.is_some() {
        return Some("--strategy (custom merge strategies are not supported)");
    }
    None
}

/// Map a per-commit error onto the public [`CherryPickError`]. `Conflicted` is
/// handled by the caller (it persists sequencer state), so it is unreachable here.
fn map_single_error(err: CherryPickSingleError, commit_label: &str) -> CherryPickError {
    match err {
        CherryPickSingleError::MergeCommitUnsupported => CherryPickError::MergeCommitUnsupported,
        CherryPickSingleError::InvalidMainline(m) => CherryPickError::InvalidMainline(m),
        CherryPickSingleError::EmptyCommit(c) => CherryPickError::EmptyCommit(c),
        CherryPickSingleError::RedundantCommit(c) => CherryPickError::RedundantCommit(c),
        CherryPickSingleError::EmptyMessage(c) => CherryPickError::EmptyMessage(c),
        CherryPickSingleError::Conflict(reason) => CherryPickError::Conflict {
            commit: commit_label.to_string(),
            reason,
        },
        CherryPickSingleError::Conflicted(paths) => CherryPickError::Conflict {
            commit: commit_label.to_string(),
            reason: format!("conflicts in {} path(s)", paths.len()),
        },
        CherryPickSingleError::LoadObject(r) => CherryPickError::LoadObject(r),
        CherryPickSingleError::SaveFailed(r) => CherryPickError::SaveFailed(r),
        CherryPickSingleError::InvalidConflictStyle(v) => CherryPickError::InvalidConflictStyle(v),
        CherryPickSingleError::ConflictStyleRead(r) => CherryPickError::ConflictStyleRead(r),
    }
}

fn make_entry(source: &ObjectHash, new_commit: Option<ObjectHash>) -> CherryPickEntry {
    let source_str = source.to_string();
    CherryPickEntry {
        source_commit: source_str.clone(),
        short_source: short_display_hash(&source_str).to_string(),
        new_commit: new_commit.as_ref().map(|id| id.to_string()),
        short_new: new_commit
            .as_ref()
            .map(|id| short_display_hash(&id.to_string()).to_string()),
    }
}

/// Record a single pick's [`PickOutcome`] into the accumulator: a
/// committed/staged pick becomes a `CherryPickEntry`, a `--empty=drop` pick
/// becomes a `DroppedCommit`.
fn record_outcome(outcome: PickOutcome, commit_id: &ObjectHash, acc: &mut PickAccumulator) {
    match outcome {
        PickOutcome::Committed(id) => acc.picked.push(make_entry(commit_id, Some(id))),
        PickOutcome::Staged => acc.picked.push(make_entry(commit_id, None)),
        PickOutcome::Dropped(subject) => acc.dropped.push(DroppedCommit {
            commit: commit_id.to_string(),
            subject,
        }),
    }
}

/// Current branch name, or [`CherryPickError::DetachedHead`] when HEAD is detached.
async fn current_branch_name() -> Result<String, CherryPickError> {
    match Head::current().await {
        Head::Branch(name) => Ok(name),
        Head::Detached(_) => Err(CherryPickError::DetachedHead),
    }
}

async fn load_state_or_err() -> Result<CherryPickState, CherryPickError> {
    CherryPickState::load()
        .await
        .map_err(CherryPickError::LoadObject)?
        .ok_or(CherryPickError::NoCherryPickInProgress)
}

/// Reject continuing on a different branch than the one the sequence began on.
async fn ensure_on_state_branch(state: &CherryPickState) -> Result<(), CherryPickError> {
    let current = current_branch_name().await?;
    if current != state.head_name {
        return Err(CherryPickError::WrongBranch {
            current,
            expected: state.head_name.clone(),
        });
    }
    Ok(())
}

fn silent_child_output(output: &OutputConfig) -> OutputConfig {
    let mut child = output.child_output_config();
    child.quiet = true;
    child
}

/// `reset --hard <target>` via the reset command, silenced so cherry-pick owns
/// the stdout/JSON envelope.
async fn reset_hard(target: &str, output: &OutputConfig) -> Result<(), CherryPickError> {
    let child = silent_child_output(output);
    crate::command::reset::execute_safe(
        crate::command::reset::ResetArgs {
            target: Some(target.to_string()),
            soft: false,
            mixed: false,
            hard: true,
            pathspecs: Vec::new(),
            pathspec_separator: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            no_refresh: false,
        },
        &child,
    )
    .await
    .map_err(|e| {
        CherryPickError::SaveFailed(format!("failed to reset to '{target}': {}", e.message()))
    })
}

async fn run_cherry_pick(
    args: CherryPickArgs,
    output: &OutputConfig,
) -> Result<CherryPickOutput, CherryPickError> {
    util::require_repo().map_err(|_| CherryPickError::NotInRepo)?;

    // Validate `--cleanup=<mode>` before ANY dispatch (including the sequencer
    // controls below), so an invalid mode fails fast (exit 129) and never slips
    // through `--continue`/`--skip`.
    if let Some(raw) = &args.cleanup
        && parse_cleanup_mode(raw).is_none()
    {
        return Err(CherryPickError::InvalidCleanup(raw.clone()));
    }

    // Validate `--empty=<mode>` before ANY dispatch too, for the same reason: an
    // invalid mode must fail fast (exit 129) and never slip through `--continue`.
    if let Some(raw) = &args.empty
        && parse_empty_mode(raw).is_none()
    {
        return Err(CherryPickError::InvalidEmpty(raw.clone()));
    }

    // Sequencer controls operate on the in-progress state and are dispatched
    // FIRST — they must never be rejected by the in-progress guard below.
    if args.continue_pick {
        return run_cherry_pick_continue(output).await;
    }
    if args.skip {
        return run_cherry_pick_skip(output).await;
    }
    if args.abort {
        return run_cherry_pick_abort(output).await;
    }
    if args.quit {
        return run_cherry_pick_quit().await;
    }

    if let Some(flag) = reject_unsupported_options(&args) {
        return Err(CherryPickError::Unsupported(flag.to_string()));
    }

    if let Head::Detached(_) = Head::current().await {
        return Err(CherryPickError::DetachedHead);
    }

    // A brand-new pick must not start on top of an in-progress sequence.
    if CherryPickState::is_in_progress()
        .await
        .map_err(CherryPickError::LoadObject)?
    {
        return Err(CherryPickError::InProgress);
    }

    let mut commit_ids = Vec::new();
    for commit_ref in &args.commits {
        let id = resolve_commit(commit_ref)
            .await
            .map_err(|_| CherryPickError::InvalidCommit(commit_ref.clone()))?;
        commit_ids.push(id);
    }

    // Anchors for sequencer persistence if a commit-per-pick conflict occurs.
    let head_name = current_branch_name().await?;
    let head_orig = Head::current_commit().await;
    let opts_json = serde_json::to_string(&CherryPickOpts::from_args(&args))
        .map_err(|e| CherryPickError::SaveFailed(format!("failed to serialize options: {e}")))?;

    let mut acc = PickAccumulator::default();
    for (i, commit_id) in commit_ids.iter().enumerate() {
        match cherry_pick_single_commit(commit_id, &args, output).await {
            Ok(outcome) => record_outcome(outcome, commit_id, &mut acc),
            Err(CherryPickSingleError::Conflicted(paths)) => {
                let label = args.commits[i].clone();
                if args.no_commit {
                    // `--no-commit` sequences have no per-step snapshot, so a
                    // conflict is terminal: no resumable state is written.
                    return Err(CherryPickError::Conflict {
                        commit: label,
                        reason: format!(
                            "conflicts in {} path(s); '--no-commit' multi-commit picks cannot be \
                             continued — clean up with 'libra reset --hard'/'libra restore'",
                            paths.len()
                        ),
                    });
                }
                let head_orig = head_orig.ok_or_else(|| {
                    CherryPickError::LoadObject("failed to resolve original HEAD".to_string())
                })?;
                let state = CherryPickState {
                    head_name: head_name.clone(),
                    head_orig,
                    current_oid: *commit_id,
                    todo: commit_ids[i + 1..].iter().copied().collect(),
                    opts_json: opts_json.clone(),
                };
                state.save().await.map_err(CherryPickError::SaveFailed)?;
                return Err(CherryPickError::Conflict {
                    commit: label,
                    reason: format!("conflicts in {} path(s)", paths.len()),
                });
            }
            Err(other) => return Err(map_single_error(other, &args.commits[i])),
        }
    }

    Ok(CherryPickOutput {
        picked: acc.picked,
        no_commit: args.no_commit,
        dropped: acc.dropped,
        ..Default::default()
    })
}

/// Pick the remaining `todo` of an in-progress sequence (used by `--continue`
/// after the resolved commit and by `--skip` after the dropped commit). On a
/// fresh conflict — or a non-conflict stop — it re-persists state advancing
/// `current_oid`/`todo` to the commit that stopped the sequence, so a follow-up
/// `--skip`/`--abort`/`--continue` operates on the correct position rather than
/// the stale pre-resume one. On completion it clears the state row.
async fn resume_picks(
    head_name: &str,
    head_orig: ObjectHash,
    mut todo: VecDeque<ObjectHash>,
    opts_args: &CherryPickArgs,
    opts_json: &str,
    output: &OutputConfig,
    acc: &mut PickAccumulator,
) -> Result<(), CherryPickError> {
    while let Some(commit_id) = todo.pop_front() {
        // Persist the position BEFORE attempting each commit so that whatever
        // happens — clean success, conflict, or a non-conflict hard error — the
        // `cherry_pick_state` row already reflects `current_oid = commit_id` and
        // the remaining `todo`. This keeps state accurate even when the pick
        // fails with a non-conflict error after earlier resumed commits landed.
        let pending = CherryPickState {
            head_name: head_name.to_string(),
            head_orig,
            current_oid: commit_id,
            todo: todo.clone(),
            opts_json: opts_json.to_string(),
        };
        pending.save().await.map_err(CherryPickError::SaveFailed)?;

        match cherry_pick_single_commit(&commit_id, opts_args, output).await {
            Ok(outcome) => record_outcome(outcome, &commit_id, acc),
            Err(CherryPickSingleError::Conflicted(paths)) => {
                // State already points at this commit + the remaining todo.
                return Err(CherryPickError::Conflict {
                    commit: commit_id.to_string(),
                    reason: format!("conflicts in {} path(s)", paths.len()),
                });
            }
            Err(other) => return Err(map_single_error(other, &commit_id.to_string())),
        }
    }
    CherryPickState::clear()
        .await
        .map_err(CherryPickError::SaveFailed)?;
    Ok(())
}

async fn run_cherry_pick_continue(
    output: &OutputConfig,
) -> Result<CherryPickOutput, CherryPickError> {
    let state = load_state_or_err().await?;
    ensure_on_state_branch(&state).await?;

    // The conflicted index must be fully resolved (no stage 1/2/3 left).
    let index = Index::load(path::index())
        .map_err(|e| CherryPickError::LoadObject(format!("failed to load index: {e}")))?;
    if !merge::unresolved_conflicted_paths(&index, &[]).is_empty() {
        return Err(CherryPickError::Conflict {
            commit: short_display_hash(&state.current_oid.to_string()).to_string(),
            reason: "unresolved conflicts remain in the index".to_string(),
        });
    }

    let opts: CherryPickOpts = serde_json::from_str(&state.opts_json)
        .map_err(|e| CherryPickError::LoadObject(format!("failed to read saved options: {e}")))?;
    let opts_args = opts.into_args();

    // rerere: the conflict is resolved — record its postimage so an identical
    // conflict is auto-resolved next time. A no-op unless `rerere.enabled`.
    if let Err(error) = crate::command::rerere::auto_update(opts_args.rerere_autoupdate).await {
        tracing::warn!("rerere auto-update on cherry-pick --continue failed: {error}");
    }

    // Finalize the resolved pick: build a commit from the resolved index tree.
    let original: Commit = load_object(&state.current_oid).map_err(|e| {
        CherryPickError::LoadObject(format!("failed to load conflicted commit: {e}"))
    })?;
    let parent = Head::current_commit()
        .await
        .ok_or_else(|| CherryPickError::LoadObject("failed to resolve current HEAD".to_string()))?;
    let tree_id = create_tree_from_index(&index).map_err(|e| map_single_error(e, ""))?;
    let new_commit = create_cherry_pick_commit(&original, &parent, tree_id, &opts_args, output)
        .await
        .map_err(|e| map_single_error(e, &state.current_oid.to_string()))?;

    let mut acc = PickAccumulator {
        picked: vec![make_entry(&state.current_oid, Some(new_commit))],
        dropped: Vec::new(),
    };
    resume_picks(
        &state.head_name,
        state.head_orig,
        state.todo,
        &opts_args,
        &state.opts_json,
        output,
        &mut acc,
    )
    .await?;

    Ok(CherryPickOutput {
        picked: acc.picked,
        dropped: acc.dropped,
        action: Some("continue".to_string()),
        ..Default::default()
    })
}

async fn run_cherry_pick_skip(output: &OutputConfig) -> Result<CherryPickOutput, CherryPickError> {
    let state = load_state_or_err().await?;
    ensure_on_state_branch(&state).await?;

    // Drop the current conflicted commit: restore index+worktree to the last
    // successful tip (current HEAD), discarding the conflict markers/stages.
    reset_hard("HEAD", output).await?;

    let opts: CherryPickOpts = serde_json::from_str(&state.opts_json)
        .map_err(|e| CherryPickError::LoadObject(format!("failed to read saved options: {e}")))?;
    let opts_args = opts.into_args();

    let mut acc = PickAccumulator::default();
    resume_picks(
        &state.head_name,
        state.head_orig,
        state.todo,
        &opts_args,
        &state.opts_json,
        output,
        &mut acc,
    )
    .await?;

    Ok(CherryPickOutput {
        picked: acc.picked,
        dropped: acc.dropped,
        action: Some("skip".to_string()),
        ..Default::default()
    })
}

async fn run_cherry_pick_abort(output: &OutputConfig) -> Result<CherryPickOutput, CherryPickError> {
    let state = load_state_or_err().await?;
    ensure_on_state_branch(&state).await?;

    let restored = state.head_orig.to_string();
    reset_hard(&restored, output).await?;
    CherryPickState::clear()
        .await
        .map_err(CherryPickError::SaveFailed)?;

    Ok(CherryPickOutput {
        action: Some("abort".to_string()),
        restored_head: Some(restored),
        ..Default::default()
    })
}

async fn run_cherry_pick_quit() -> Result<CherryPickOutput, CherryPickError> {
    // Confirm a sequence is actually in progress, then forget it without
    // touching the index/worktree.
    load_state_or_err().await?;
    CherryPickState::clear()
        .await
        .map_err(CherryPickError::SaveFailed)?;

    Ok(CherryPickOutput {
        action: Some("quit".to_string()),
        ..Default::default()
    })
}

// ── Rendering ────────────────────────────────────────────────────────

fn render_cherry_pick_output(result: &CherryPickOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("cherry-pick", result, output);
    }

    if output.quiet {
        return Ok(());
    }

    match result.action.as_deref() {
        Some("abort") => {
            match &result.restored_head {
                Some(head) => println!(
                    "cherry-pick aborted; HEAD reset to {}",
                    short_display_hash(head)
                ),
                None => println!("cherry-pick aborted"),
            }
            return Ok(());
        }
        Some("quit") => {
            println!("cherry-pick state cleared; working tree left unchanged");
            return Ok(());
        }
        _ => {}
    }

    // `--empty=drop`: note each redundant commit that was skipped (Git's
    // `dropping <sha> <subject> -- patch contents already upstream`).
    for d in &result.dropped {
        println!(
            "dropping {} {} -- patch contents already upstream",
            d.commit, d.subject
        );
    }
    for entry in &result.picked {
        if let Some(short_new) = &entry.short_new {
            println!("[{}] cherry-picked from {}", short_new, entry.short_source,);
        } else {
            println!(
                "Changes from {} staged. Use 'libra commit' to finalize.",
                entry.short_source,
            );
        }
    }
    Ok(())
}

// ── Internal logic ───────────────────────────────────────────────────

async fn cherry_pick_single_commit(
    commit_id: &ObjectHash,
    args: &CherryPickArgs,
    output: &OutputConfig,
) -> Result<PickOutcome, CherryPickSingleError> {
    let commit_to_pick: Commit =
        load_object(commit_id).map_err(|e| CherryPickSingleError::LoadObject(e.to_string()))?;

    let parent_count = commit_to_pick.parent_commit_ids.len();
    let short = short_display_hash(&commit_id.to_string()).to_string();

    // `--ff`: when the picked commit is a single-parent direct child of HEAD and
    // no commit-rewriting modifier is set, advance HEAD without replaying or
    // rewriting the commit (no hash drift).
    if args.ff
        && !args.no_commit
        && !args.append_source
        && !args.signoff
        && !args.edit
        && args.mainline.is_none()
        && parent_count == 1
        && let Some(head) = Head::current_commit().await
        && commit_to_pick.parent_commit_ids[0] == head
    {
        reset_hard(&commit_id.to_string(), output)
            .await
            .map_err(|e| CherryPickSingleError::SaveFailed(e.to_string()))?;
        return Ok(PickOutcome::Committed(*commit_id));
    }

    // Resolve the diff base parent, honoring `-m <n>` for merge commits.
    let base_parent: Option<ObjectHash> = match (parent_count, args.mainline) {
        (0, None) => None,
        (0, Some(_)) => {
            return Err(CherryPickSingleError::InvalidMainline(format!(
                "commit {short} is a root commit; -m/--mainline is invalid"
            )));
        }
        (1, None) => Some(commit_to_pick.parent_commit_ids[0]),
        (1, Some(_)) => {
            return Err(CherryPickSingleError::InvalidMainline(format!(
                "commit {short} is not a merge commit; -m/--mainline only applies to merge commits"
            )));
        }
        (_, None) => return Err(CherryPickSingleError::MergeCommitUnsupported),
        (n, Some(m)) => {
            if m < 1 || m > n {
                return Err(CherryPickSingleError::InvalidMainline(format!(
                    "mainline {m} is out of range for merge commit {short} with {n} parents"
                )));
            }
            Some(commit_to_pick.parent_commit_ids[m - 1])
        }
    };

    let parent_tree = match base_parent {
        None => {
            let empty_id = ObjectHash::from_type_and_data(ObjectType::Tree, &[]);
            Tree::from_bytes(&[], empty_id).map_err(|e| {
                CherryPickSingleError::SaveFailed(format!(
                    "failed to create empty tree for root commit: {e}",
                ))
            })?
        }
        Some(parent_id) => {
            let parent_commit: Commit = load_object(&parent_id).map_err(|e| {
                CherryPickSingleError::LoadObject(format!("failed to load parent commit: {e}"))
            })?;
            load_object(&parent_commit.tree_id).map_err(|e| {
                CherryPickSingleError::LoadObject(format!("failed to load parent tree: {e}"))
            })?
        }
    };

    let their_tree: Tree = load_object(&commit_to_pick.tree_id).map_err(|e| {
        CherryPickSingleError::LoadObject(format!("failed to load commit tree: {e}"))
    })?;

    // (A) "Empty" class 1: the picked commit's own change set is empty (its tree
    // equals its parent tree). Git blocks this unless `--allow-empty`. Checked
    // before any index/worktree mutation so a blocked pick leaves state intact.
    let originally_empty = commit_to_pick.tree_id == parent_tree.id;
    if originally_empty && !args.allow_empty {
        return Err(CherryPickSingleError::EmptyCommit(commit_id.to_string()));
    }

    let index_file = path::index();
    let current_index = Index::load(&index_file).map_err(|e| {
        CherryPickSingleError::LoadObject(format!("failed to load current index: {e}"))
    })?;
    let mut index = Index::load(&index_file).map_err(|e| {
        CherryPickSingleError::LoadObject(format!("failed to load current index: {e}"))
    })?;

    // ── Three-way apply: base = parent tree, ours = current index stage 0,
    // theirs = picked commit tree. A path whose ours-side still matches base
    // fast-forwards to theirs; a path where both sides agree is a no-op; a path
    // that diverged on both sides becomes a stage 1/2/3 conflict. ──
    let ours_items: HashMap<PathBuf, ObjectHash> = current_index
        .tracked_files()
        .into_iter()
        .filter_map(|p| {
            let key = p.to_str()?;
            current_index.get_hash(key, 0).map(|h| (p.clone(), h))
        })
        .collect();

    let mut conflicts: Vec<ConflictEntry> = Vec::new();
    for (path, their_hash, base_hash) in diff_trees(&their_tree, &parent_tree) {
        let ours_hash = ours_items.get(&path).cloned();
        if ours_hash == base_hash {
            match their_hash {
                Some(th) => update_index_entry(&mut index, &path, th)?,
                None => {
                    index.remove(path_to_utf8(&path)?, 0);
                }
            }
        } else if ours_hash == their_hash {
            // Both sides already converged on the same content — nothing to do.
        } else {
            index.remove(path_to_utf8(&path)?, 0);
            if let Some(b) = base_hash {
                add_stage_entry(&mut index, &path, b, 1)?;
            }
            if let Some(o) = ours_hash {
                add_stage_entry(&mut index, &path, o, 2)?;
            }
            if let Some(t) = their_hash {
                add_stage_entry(&mut index, &path, t, 3)?;
            }
            conflicts.push((path, ours_hash, their_hash, base_hash));
        }
    }

    if !conflicts.is_empty() {
        // Honor the Git-compatible `merge.conflictStyle` config (merge/diff3)
        // for the line-level markers, same as `libra merge` (lore.md §1.3).
        // Resolved FIRST — before the conflicted index is saved or the worktree
        // is touched — so an invalid config errors with nothing mutated.
        let conflict_style =
            super::merge::conflict_style_from_config()
                .await
                .map_err(|e| match e {
                    super::merge::ConflictStyleError::Invalid(value) => {
                        CherryPickSingleError::InvalidConflictStyle(value)
                    }
                    super::merge::ConflictStyleError::Read(detail) => {
                        CherryPickSingleError::ConflictStyleRead(detail)
                    }
                })?;
        index
            .save(&index_file)
            .map_err(|e| CherryPickSingleError::SaveFailed(format!("failed to save index: {e}")))?;
        // Sync the cleanly-applied stage-0 paths, then overlay conflict markers
        // onto each divergent path so the user can resolve them in the worktree.
        reset_workdir_tracked_only(&current_index, &index)?;
        let short_src = short_display_hash(&commit_id.to_string()).to_string();
        for (path, ours_hash, their_hash, base_hash) in &conflicts {
            write_conflict_markers_file(
                path,
                ours_hash,
                their_hash,
                base_hash,
                &short_src,
                conflict_style,
            )?;
        }
        // rerere: record the preimage of each just-written conflict and replay a
        // previously recorded resolution if one matches. A no-op unless
        // `rerere.enabled` is set, so default cherry-pick behaviour is unchanged.
        if let Err(error) = crate::command::rerere::auto_update(args.rerere_autoupdate).await {
            tracing::warn!("rerere auto-update after cherry-pick conflict failed: {error}");
        }
        let mut paths: Vec<String> = conflicts
            .iter()
            .map(|(path, _, _, _)| path.display().to_string())
            .collect();
        paths.sort();
        return Err(CherryPickSingleError::Conflicted(paths));
    }

    // Build the candidate tree first (saves tree objects, but does NOT touch the
    // on-disk index or worktree yet) so the "redundant after replay" check can
    // bail out before mutating any state.
    let tree_id = create_tree_from_index(&index)?;

    if args.no_commit {
        index
            .save(&index_file)
            .map_err(|e| CherryPickSingleError::SaveFailed(format!("failed to save index: {e}")))?;
        reset_workdir_tracked_only(&current_index, &index)?;
        return Ok(PickOutcome::Staged);
    }

    let current_head = Head::current_commit().await.ok_or_else(|| {
        CherryPickSingleError::LoadObject("failed to resolve current HEAD".to_string())
    })?;

    // (B) "Empty" class 2: the replayed change is redundant against the current
    // HEAD (resulting tree is identical). Git's `--empty=<mode>` decides: `stop`
    // (default) halts, `drop` skips the commit, `keep` records the empty commit.
    // An originally-empty commit that reached here has already passed `--allow-empty`,
    // so it is committed regardless (its emptiness is intentional, not redundant).
    let head_commit: Commit = load_object(&current_head).map_err(|e| {
        CherryPickSingleError::LoadObject(format!("failed to load current HEAD commit: {e}"))
    })?;
    if tree_id == head_commit.tree_id && !originally_empty {
        match effective_empty_mode(args) {
            EmptyMode::Stop => {
                return Err(CherryPickSingleError::RedundantCommit(
                    commit_id.to_string(),
                ));
            }
            EmptyMode::Drop => {
                // Skip without touching the index/worktree or advancing HEAD. Take
                // the subject from the de-signed message so a signed commit reports
                // its real first line, not the `gpgsig` header.
                let (clean_msg, _) = crate::common_utils::parse_commit_msg(&commit_to_pick.message);
                let subject = clean_msg.lines().next().unwrap_or("").to_string();
                return Ok(PickOutcome::Dropped(subject));
            }
            // `keep`: fall through and commit the (empty) commit.
            EmptyMode::Keep => {}
        }
    }

    index
        .save(&index_file)
        .map_err(|e| CherryPickSingleError::SaveFailed(format!("failed to save index: {e}")))?;
    reset_workdir_tracked_only(&current_index, &index)?;

    let cherry_pick_commit_id =
        create_cherry_pick_commit(&commit_to_pick, &current_head, tree_id, args, output).await?;
    Ok(PickOutcome::Committed(cherry_pick_commit_id))
}

/// Resolve the Signed-off-by identity from the configured `user.name`/`user.email`
/// (falling back to the same defaults `libra commit` uses).
async fn resolve_signoff_identity() -> (String, String) {
    let (_, committer) = util::create_signatures().await;
    (committer.name, committer.email)
}

/// Resolve the editor for `-e`, mirroring Git's precedence:
/// `core.editor` config → `$VISUAL` → `$EDITOR`. Returns `None` when none is set.
async fn resolve_editor() -> Option<String> {
    if let Ok(Some(entry)) = ConfigKv::get("core.editor").await
        && !entry.value.trim().is_empty()
    {
        return Some(entry.value);
    }
    for var in ["VISUAL", "EDITOR"] {
        if let Ok(value) = std::env::var(var)
            && !value.trim().is_empty()
        {
            return Some(value);
        }
    }
    None
}

/// Assemble the cherry-pick commit message, honoring `-x` (append source line),
/// `-s` (Signed-off-by trailer, in that order), and `-e` (interactive edit).
async fn build_cherry_pick_message(
    original_commit: &Commit,
    args: &CherryPickArgs,
    output: &OutputConfig,
) -> Result<String, CherryPickSingleError> {
    // Resolve the editor up front: `-e` only opens one on an interactive TTY,
    // never in machine/JSON mode, and only if one is configured. Whether it
    // actually opens governs the `default`/`scissors` cleanup fallback.
    let editor = if args.edit && !output.is_json() && std::io::stdin().is_terminal() {
        resolve_editor().await
    } else {
        None
    };

    // Resolve `--cleanup=<mode>` to its effective mode (validated up front, so an
    // unparseable value cannot reach here). `default`/`scissors` fall back to
    // `whitespace` when no editor opens — matching Git's "if the message is to be
    // edited" clause and `libra commit`.
    let effective_cleanup = args
        .cleanup
        .as_deref()
        .and_then(parse_cleanup_mode)
        .map(|mode| {
            if editor.is_some() {
                mode
            } else {
                match mode {
                    CleanupMode::Default | CleanupMode::Scissors => CleanupMode::Whitespace,
                    other => other,
                }
            }
        });

    let body = original_commit.message.trim();

    if let Some(mode) = effective_cleanup {
        // `--cleanup` path: clean the BODY (and, after `-e`, the edited buffer),
        // THEN append the generated trailers — so cleanup applies to everything
        // the user can change while never collapsing the trailer separator.
        let mut message = cleanup_commit_message(body, mode);
        if let Some(editor) = editor {
            let edited = edit_cherry_pick_message(&message, &editor).await?;
            message = cleanup_commit_message(&edited, mode);
        }
        append_cherry_pick_trailers(&mut message, original_commit, args).await;
        Ok(message)
    } else {
        // Default path (unchanged): trim → trailers → optional `-e` edit.
        let mut message = body.to_string();
        append_cherry_pick_trailers(&mut message, original_commit, args).await;
        if let Some(editor) = editor {
            message = edit_cherry_pick_message(&message, &editor).await?;
        }
        Ok(message)
    }
}

/// Append the cherry-pick trailer block to `message`: the `-x`
/// `(cherry picked from commit …)` line first, then the `-s` `Signed-off-by`
/// line, each only when requested and not already present (matches Git's order).
async fn append_cherry_pick_trailers(
    message: &mut String,
    original_commit: &Commit,
    args: &CherryPickArgs,
) {
    let mut trailers: Vec<String> = Vec::new();
    if args.append_source {
        let line = format!("(cherry picked from commit {})", original_commit.id);
        if !message.contains(&line) {
            trailers.push(line);
        }
    }
    if args.signoff {
        let (name, email) = resolve_signoff_identity().await;
        let line = format!("Signed-off-by: {name} <{email}>");
        if !message.contains(&line) {
            trailers.push(line);
        }
    }
    if !trailers.is_empty() {
        message.push_str("\n\n");
        message.push_str(&trailers.join("\n"));
    }
}

/// Launch the resolved editor on a scratch message file via the shared editor
/// helper. A missing or failing editor leaves the message intact
/// (`abort_on_failure = false`). Cherry-pick keeps its own editor precedence
/// (`core.editor` → `$VISUAL` → `$EDITOR`, no `$GIT_EDITOR`) via `resolve_editor`
/// above and passes the resolved command to the shared launcher.
async fn edit_cherry_pick_message(
    message: &str,
    editor: &str,
) -> Result<String, CherryPickSingleError> {
    let path = util::storage_path().join("CHERRY_PICK_MSG");
    crate::command::editor::edit_message(&path, message, editor, false)
        .await
        .map_err(|e| CherryPickSingleError::SaveFailed(e.to_string()))
}

async fn create_cherry_pick_commit(
    original_commit: &Commit,
    parent_id: &ObjectHash,
    tree_id: ObjectHash,
    args: &CherryPickArgs,
    output: &OutputConfig,
) -> Result<ObjectHash, CherryPickSingleError> {
    let message = build_cherry_pick_message(original_commit, args, output).await?;

    if message.trim().is_empty() && !args.allow_empty_message {
        return Err(CherryPickSingleError::EmptyMessage(
            original_commit.id.to_string(),
        ));
    }

    let parents = vec![*parent_id];
    let commit = if args.gpg_sign {
        // Sign via the libra vault (force=true so it signs regardless of the
        // `vault.signing` default).
        let (author, committer) = util::create_signatures().await;
        let gpgsig = crate::command::commit::vault_sign_commit(
            &tree_id, &parents, &author, &committer, &message, true,
        )
        .await
        .map_err(|e| CherryPickSingleError::SaveFailed(format!("failed to sign commit: {e}")))?;
        match gpgsig {
            Some(sig) => Commit::new(
                author,
                committer,
                tree_id,
                parents,
                &format_commit_msg(&message, Some(&sig)),
            ),
            None => {
                return Err(CherryPickSingleError::SaveFailed(
                    "vault signing key unavailable; configure libra vault to use --gpg-sign"
                        .to_string(),
                ));
            }
        }
    } else {
        Commit::from_tree_id(tree_id, parents, &format_commit_msg(&message, None))
    };

    save_object(&commit, &commit.id)
        .map_err(|e| CherryPickSingleError::SaveFailed(format!("failed to save commit: {e}")))?;

    let action = ReflogAction::CherryPick {
        source_message: original_commit.message.clone(),
    };
    let context = ReflogContext {
        old_oid: parent_id.to_string(),
        new_oid: commit.id.to_string(),
        action,
    };

    with_reflog(
        context,
        move |txn| {
            Box::pin(async move {
                update_head(txn, &commit.id.to_string()).await?;
                Ok(())
            })
        },
        true,
    )
    .await
    .map_err(|e| {
        CherryPickSingleError::SaveFailed(format!("failed to update branch and reflog: {e}"))
    })?;
    Ok(commit.id)
}

fn diff_trees(
    theirs: &Tree,
    base: &Tree,
) -> Vec<(PathBuf, Option<ObjectHash>, Option<ObjectHash>)> {
    let mut diffs = Vec::new();
    let their_items: HashMap<_, _> = theirs.get_plain_items().into_iter().collect();
    let base_items: HashMap<_, _> = base.get_plain_items().into_iter().collect();

    let all_paths: HashSet<_> = their_items.keys().chain(base_items.keys()).collect();

    for path in all_paths {
        let their_hash = their_items.get(path).cloned();
        let base_hash = base_items.get(path).cloned();
        if their_hash != base_hash {
            diffs.push((path.clone(), their_hash, base_hash));
        }
    }
    diffs
}

fn update_index_entry(
    index: &mut Index,
    path: &Path,
    hash: ObjectHash,
) -> Result<(), CherryPickSingleError> {
    let blob = git_internal::internal::object::blob::Blob::load(&hash);
    let entry = IndexEntry::new_from_blob(
        path_to_utf8(path)?.to_string(),
        hash,
        blob.data.len() as u32,
    );
    index.add(entry);
    Ok(())
}

/// Add a conflict-stage (1=base / 2=ours / 3=theirs) index entry for `path`.
fn add_stage_entry(
    index: &mut Index,
    path: &Path,
    hash: ObjectHash,
    stage: u8,
) -> Result<(), CherryPickSingleError> {
    let blob = git_internal::internal::object::blob::Blob::load(&hash);
    let mut entry = IndexEntry::new_from_blob(
        path_to_utf8(path)?.to_string(),
        hash,
        blob.data.len() as u32,
    );
    entry.flags.stage = stage;
    index.add(entry);
    Ok(())
}

/// Write Git-style conflict markers for a divergent path into the working tree.
///
/// When both sides and the base are UTF-8 text, this delegates to the shared
/// line-level renderer ([`merge::render_line_level_conflict`]) so the conflict
/// markers enclose only the diverging hunks, matching Git. A delete/modify
/// conflict (one side absent) or binary content falls back to a whole-file
/// presentation — ours between `<<<<<<< HEAD` and `=======`, theirs up to
/// `>>>>>>> <short-source>`.
fn write_conflict_markers_file(
    path: &Path,
    ours_hash: &Option<ObjectHash>,
    their_hash: &Option<ObjectHash>,
    base_hash: &Option<ObjectHash>,
    short_src: &str,
    conflict_style: diffy::ConflictStyle,
) -> Result<(), CherryPickSingleError> {
    fn side_bytes(hash: &Option<ObjectHash>) -> Option<Vec<u8>> {
        hash.as_ref()
            .map(|h| git_internal::internal::object::blob::Blob::load(h).data)
    }
    let ours_bytes = side_bytes(ours_hash);
    let theirs_bytes = side_bytes(their_hash);
    let base_bytes = side_bytes(base_hash);

    // Line-level merge applies only when both sides are present and text; the
    // shared helper returns None otherwise so we fall back to whole-file markers.
    let content: Vec<u8> = match (&ours_bytes, &theirs_bytes) {
        (Some(ours), Some(theirs)) => super::merge::render_line_level_conflict(
            base_bytes.as_deref(),
            ours,
            theirs,
            short_src,
            conflict_style,
        )
        .unwrap_or_else(|| whole_file_conflict(ours, theirs, short_src)),
        _ => whole_file_conflict(
            ours_bytes.as_deref().unwrap_or(&[]),
            theirs_bytes.as_deref().unwrap_or(&[]),
            short_src,
        ),
    };

    let target = util::working_dir().join(path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            CherryPickSingleError::SaveFailed(format!(
                "failed to create parent directory '{}': {e}",
                parent.display()
            ))
        })?;
    }
    fs::write(&target, &content).map_err(|e| {
        CherryPickSingleError::SaveFailed(format!(
            "failed to write conflict markers to '{}': {e}",
            target.display()
        ))
    })?;
    Ok(())
}

/// Whole-file conflict presentation, used when a line-level merge does not apply
/// (a delete/modify conflict, or binary content): ours between `<<<<<<< HEAD`
/// and `=======`, theirs up to `>>>>>>> <short-source>`.
fn whole_file_conflict(ours: &[u8], theirs: &[u8], short_src: &str) -> Vec<u8> {
    fn side_text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }
    let ours = side_text(ours);
    let theirs = side_text(theirs);

    let mut content = String::from("<<<<<<< HEAD\n");
    content.push_str(&ours);
    if !ours.is_empty() && !ours.ends_with('\n') {
        content.push('\n');
    }
    content.push_str("=======\n");
    content.push_str(&theirs);
    if !theirs.is_empty() && !theirs.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&format!(">>>>>>> {short_src}\n"));
    content.into_bytes()
}

/// Build (and persist) the nested tree for the current index, delegating to the
/// shared [`tree_plumbing::write_tree_from_index`] so cherry-pick, merge, and
/// `write-tree` share one tree-construction rule (and one bug-fix surface — the
/// shared builder handles intermediate directories the old per-command builder
/// dropped).
fn create_tree_from_index(index: &Index) -> Result<ObjectHash, CherryPickSingleError> {
    tree_plumbing::write_tree_from_index(index)
        .map_err(|e| CherryPickSingleError::SaveFailed(e.to_string()))
}

fn reset_workdir_tracked_only(
    current_index: &Index,
    new_index: &Index,
) -> Result<(), CherryPickSingleError> {
    let workdir = util::working_dir();
    let untracked_paths = worktree::untracked_workdir_paths(current_index).map_err(|e| {
        CherryPickSingleError::LoadObject(format!("failed to inspect untracked files: {e}"))
    })?;
    if let Some(conflict) = worktree::untracked_overwrite_path(&untracked_paths, new_index) {
        return Err(CherryPickSingleError::Conflict(format!(
            "untracked working tree file would be overwritten: {}",
            conflict.display()
        )));
    }
    let new_tracked_paths: HashSet<_> = new_index.tracked_files().into_iter().collect();

    for path_buf in current_index.tracked_files() {
        if !new_tracked_paths.contains(&path_buf) {
            let full_path = workdir.join(path_buf);
            if full_path.exists() {
                fs::remove_file(&full_path).map_err(|e| {
                    CherryPickSingleError::SaveFailed(format!(
                        "failed to remove file '{}': {e}",
                        full_path.display()
                    ))
                })?;
            }
        }
    }

    for path_buf in new_index.tracked_files() {
        let path_str = path_to_utf8(&path_buf)?;
        if let Some(entry) = new_index.get(path_str, 0) {
            let blob = git_internal::internal::object::blob::Blob::load(&entry.hash);
            let target_path = workdir.join(path_str);
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    CherryPickSingleError::SaveFailed(format!(
                        "failed to create parent directory '{}': {e}",
                        parent.display()
                    ))
                })?;
            }
            fs::write(&target_path, &blob.data).map_err(|e| {
                CherryPickSingleError::SaveFailed(format!(
                    "failed to write file '{}': {e}",
                    target_path.display()
                ))
            })?;
        }
    }
    Ok(())
}

fn path_to_utf8(path: &Path) -> Result<&str, CherryPickSingleError> {
    path.to_str().ok_or_else(|| {
        CherryPickSingleError::LoadObject(format!("invalid path encoding: {}", path.display()))
    })
}

async fn resolve_commit(reference: &str) -> Result<ObjectHash, String> {
    util::get_commit_base(reference).await
}

async fn update_head<C: ConnectionTrait>(db: &C, commit_id: &str) -> Result<(), sea_orm::DbErr> {
    if let Head::Branch(name) = Head::current_with_conn(db).await {
        Branch::update_branch_with_conn(db, &name, commit_id, None).await?;
    }
    Ok(())
}

// ── Cherry-pick sequencer state (unified `sequence_state`, lore.md 2.6) ──

/// Upper bound on `todo` OIDs read back from a persisted state row. Guards
/// against an externally-corrupted `todo` column ballooning memory on load.
const CHERRY_PICK_TODO_CAP: usize = 10_000;

/// In-progress cherry-pick sequence persisted in the repo database.
///
/// Mirrors [`crate::command::rebase::RebaseState`]: the sequence lives ONLY in
/// the unified `sequence_state` table (there is no `.libra/CHERRY_PICK_HEAD`
/// file), matching the repository's metadata-in-SQLite convention. The
/// `_with_conn` variants accept any [`ConnectionTrait`] so a caller can wrap the
/// `DELETE`+`INSERT` save in one transaction; [`CherryPickState::save`] does
/// exactly that so a single sequencer write is never left half-applied.
#[derive(Debug, Clone)]
pub struct CherryPickState {
    /// Branch name HEAD pointed at when the sequence began.
    pub head_name: String,
    /// That branch's commit at sequence start — the `--abort` rollback target.
    pub head_orig: ObjectHash,
    /// The commit whose application is currently conflicted.
    pub current_oid: ObjectHash,
    /// Remaining commits to pick, in order.
    pub todo: VecDeque<ObjectHash>,
    /// Serialized commit-modifier options (`-x`/`-s`/…) for the sequence.
    pub opts_json: String,
}

impl CherryPickState {
    /// Convert to the unified sequencer row (lore.md 2.6).
    fn to_sequence(&self) -> SequenceState {
        SequenceState {
            kind: SequenceKind::CherryPick,
            head_name: self.head_name.clone(),
            head_orig: self.head_orig.to_string(),
            current_oid: self.current_oid.to_string(),
            todo: self.todo.iter().map(|oid| oid.to_string()).collect(),
            payload: self.opts_json.clone(),
        }
    }

    /// Rebuild from a unified sequencer row, re-validating the OIDs and the
    /// todo cap through the existing parser.
    fn from_sequence(state: SequenceState) -> Result<Self, String> {
        let head_orig = ObjectHash::from_str(state.head_orig.trim())
            .map_err(|e| format!("invalid head_orig hash: {e}"))?;
        let current_oid = ObjectHash::from_str(state.current_oid.trim())
            .map_err(|e| format!("invalid current_oid hash: {e}"))?;
        let todo = VecDeque::from(Self::parse_todo(&state.todo.join("\n"))?);
        Ok(CherryPickState {
            head_name: state.head_name,
            head_orig,
            current_oid,
            todo,
            opts_json: state.payload,
        })
    }

    /// Persist via the unified sequencer (atomic DELETE+INSERT txn).
    pub async fn save(&self) -> Result<(), String> {
        sequencer::save(&self.to_sequence()).await
    }

    /// Load the active cherry-pick sequence, if the active sequence is a
    /// cherry-pick (the unified table holds at most one active op).
    pub async fn load() -> Result<Option<Self>, String> {
        match sequencer::load().await? {
            Some(state) if state.kind == SequenceKind::CherryPick => {
                Ok(Some(Self::from_sequence(state)?))
            }
            _ => Ok(None),
        }
    }

    /// Clear the active sequence (idempotent; scoped to the cherry-pick kind).
    pub async fn clear() -> Result<(), String> {
        sequencer::clear(SequenceKind::CherryPick).await
    }

    /// Whether a cherry-pick specifically is in progress.
    pub async fn is_in_progress() -> Result<bool, String> {
        Ok(matches!(
            sequencer::load().await?,
            Some(state) if state.kind == SequenceKind::CherryPick
        ))
    }

    fn parse_todo(raw: &str) -> Result<Vec<ObjectHash>, String> {
        let mut out = Vec::new();
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if out.len() >= CHERRY_PICK_TODO_CAP {
                return Err(format!(
                    "cherry_pick_state todo exceeds {CHERRY_PICK_TODO_CAP} entries"
                ));
            }
            let oid = ObjectHash::from_str(trimmed)
                .map_err(|e| format!("invalid todo OID '{trimmed}': {e}"))?;
            out.push(oid);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the `Display` format for every variant of [`CherryPickError`].
    /// These strings are used as the `CliError` message via
    /// `From<CherryPickError> for CliError` and surface in both human
    /// and `--json` envelopes for the `cherry-pick` subcommand.
    ///
    /// All variants are pinned because every variant carries either a
    /// static message or an explicit `{0}` field interpolation; none
    /// wrap an upstream source error directly.
    #[test]
    fn cherry_pick_error_display_pins_each_variant() {
        assert_eq!(
            CherryPickError::NotInRepo.to_string(),
            "not a libra repository",
        );
        assert_eq!(
            CherryPickError::DetachedHead.to_string(),
            "cannot cherry-pick on detached HEAD",
        );
        assert_eq!(
            CherryPickError::InvalidCommit("deadbeef".to_string()).to_string(),
            "failed to resolve commit reference 'deadbeef'",
        );
        assert_eq!(
            CherryPickError::MergeCommitUnsupported.to_string(),
            "cherry-picking merge commits is not supported",
        );
        assert_eq!(
            CherryPickError::InvalidMainline("mainline 3 is out of range".to_string()).to_string(),
            "mainline 3 is out of range",
        );
        assert_eq!(
            CherryPickError::Unsupported("--cleanup".to_string()).to_string(),
            "unsupported cherry-pick option: --cleanup",
        );
        assert_eq!(
            CherryPickError::EmptyCommit("abc123".to_string()).to_string(),
            "commit abc123 is empty (its change set is empty)",
        );
        assert_eq!(
            CherryPickError::RedundantCommit("abc123".to_string()).to_string(),
            "commit abc123 became redundant after replay (no changes to apply)",
        );
        assert_eq!(
            CherryPickError::EmptyMessage("abc123".to_string()).to_string(),
            "commit abc123 has an empty commit message",
        );
        assert_eq!(
            CherryPickError::Conflict {
                commit: "abc123".to_string(),
                reason: "untracked file would be overwritten".to_string(),
            }
            .to_string(),
            "failed to cherry-pick abc123: untracked file would be overwritten",
        );
        assert_eq!(
            CherryPickError::InProgress.to_string(),
            "a cherry-pick is already in progress",
        );
        assert_eq!(
            CherryPickError::NoCherryPickInProgress.to_string(),
            "no cherry-pick in progress",
        );
        assert_eq!(
            CherryPickError::WrongBranch {
                current: "feature".to_string(),
                expected: "main".to_string(),
            }
            .to_string(),
            "the current branch 'feature' does not match the in-progress cherry-pick branch 'main'",
        );
        assert_eq!(
            CherryPickError::LoadObject("object not found".to_string()).to_string(),
            "failed to load cherry-pick state: object not found",
        );
        assert_eq!(
            CherryPickError::SaveFailed("disk full".to_string()).to_string(),
            "failed to update cherry-pick state: disk full",
        );
    }

    /// Pin the `stable_code()` mapping for every variant of
    /// [`CherryPickError`]. This is the second public surface contract:
    /// the [`StableErrorCode`] value is what `--json` consumers read
    /// from the `code` field of the error envelope and branch on
    /// (e.g. retry on `IoReadFailed`, surface a typed hint on
    /// `ConflictUnresolved`). A future refactor that re-routes a
    /// variant silently changes the wire surface unless every variant
    /// has its own guard.
    ///
    /// Enumerate every variant explicitly so adding a new variant
    /// trips the exhaustive match below (the compiler enforces it
    /// alongside the `stable_code()` match in the impl), and silently
    /// changing an existing variant's code trips the assertion.
    #[test]
    fn cherry_pick_error_stable_code_pins_each_variant() {
        assert_eq!(
            CherryPickError::NotInRepo.stable_code(),
            StableErrorCode::RepoNotFound,
        );
        assert_eq!(
            CherryPickError::DetachedHead.stable_code(),
            StableErrorCode::RepoStateInvalid,
        );
        assert_eq!(
            CherryPickError::InvalidCommit("deadbeef".to_string()).stable_code(),
            StableErrorCode::CliInvalidTarget,
        );
        assert_eq!(
            CherryPickError::MergeCommitUnsupported.stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            CherryPickError::InvalidMainline("out of range".to_string()).stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            CherryPickError::Unsupported("--cleanup".to_string()).stable_code(),
            StableErrorCode::Unsupported,
        );
        assert_eq!(
            CherryPickError::EmptyCommit("abc123".to_string()).stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            CherryPickError::RedundantCommit("abc123".to_string()).stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            CherryPickError::EmptyMessage("abc123".to_string()).stable_code(),
            StableErrorCode::CliInvalidArguments,
        );
        assert_eq!(
            CherryPickError::Conflict {
                commit: "abc123".to_string(),
                reason: "ignored".to_string(),
            }
            .stable_code(),
            StableErrorCode::ConflictUnresolved,
        );
        assert_eq!(
            CherryPickError::InProgress.stable_code(),
            StableErrorCode::ConflictOperationBlocked,
        );
        assert_eq!(
            CherryPickError::NoCherryPickInProgress.stable_code(),
            StableErrorCode::RepoStateInvalid,
        );
        assert_eq!(
            CherryPickError::WrongBranch {
                current: "feature".to_string(),
                expected: "main".to_string(),
            }
            .stable_code(),
            StableErrorCode::RepoStateInvalid,
        );
        assert_eq!(
            CherryPickError::LoadObject("ignored".to_string()).stable_code(),
            StableErrorCode::IoReadFailed,
        );
        assert_eq!(
            CherryPickError::SaveFailed("ignored".to_string()).stable_code(),
            StableErrorCode::IoWriteFailed,
        );
    }

    /// Every commit-shaping modifier must round-trip through `CherryPickOpts`
    /// serde (the `cherry_pick_state.opts_json` blob), so a conflict + resume
    /// replays the rest of the sequence with the same options. Guards against
    /// silently dropping a flag (e.g. `-S` producing unsigned commits, or `-m`
    /// failing a later merge commit) on `--continue`/`--skip`.
    #[test]
    fn cherry_pick_opts_round_trip_preserves_all_modifiers() {
        let args = CherryPickArgs {
            append_source: true,
            signoff: true,
            edit: true,
            allow_empty: true,
            allow_empty_message: true,
            keep_redundant_commits: true,
            gpg_sign: true,
            mainline: Some(2),
            cleanup: Some("strip".to_string()),
            empty: Some("drop".to_string()),
            ..Default::default()
        };
        let json = serde_json::to_string(&CherryPickOpts::from_args(&args)).unwrap();
        let rebuilt = serde_json::from_str::<CherryPickOpts>(&json)
            .unwrap()
            .into_args();
        assert!(rebuilt.append_source);
        assert!(rebuilt.signoff);
        assert!(rebuilt.edit);
        assert!(rebuilt.allow_empty);
        assert!(rebuilt.allow_empty_message);
        assert!(rebuilt.keep_redundant_commits);
        assert!(rebuilt.gpg_sign);
        assert_eq!(rebuilt.mainline, Some(2));
        assert_eq!(rebuilt.cleanup.as_deref(), Some("strip"));
        assert_eq!(rebuilt.empty.as_deref(), Some("drop"));
    }
}
