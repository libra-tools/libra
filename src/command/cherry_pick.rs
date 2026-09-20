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
        object::{
            ObjectTrait,
            blob::Blob,
            commit::Commit,
            tree::{Tree, TreeItemMode},
            types::ObjectType,
        },
    },
};
use sea_orm::ConnectionTrait;
use serde::Serialize;

use crate::{
    command::{
        commit::{
            CleanupMode, cleanup_commit_message, create_committer_signature, parse_cleanup_mode,
        },
        load_object,
        merge::{self, MergeFavor},
        save_object,
    },
    common_utils::{format_commit_msg, parse_commit_msg},
    internal::{
        branch::Branch,
        change::{RelationKind, record_current_repo_commit_revision_for_active_operation},
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
    merge::BuiltinMergeDriver,
);

const CHERRY_PICK_EXAMPLES: &str = "\
EXAMPLES:
    libra cherry-pick abc1234              Apply a single commit
    libra cherry-pick abc1234 def5678      Apply multiple commits in order
    libra cherry-pick -n abc1234           Apply without auto-committing
    libra cherry-pick -x abc1234           Append a '(cherry picked from ...)' line
    libra cherry-pick -s abc1234           Add a Signed-off-by trailer
    libra cherry-pick -m 1 <merge>         Cherry-pick a merge commit along parent 1
    libra cherry-pick -X ours abc1234      Favor current-side conflicting hunks
    libra cherry-pick --cleanup=strip abc1234  Clean up the replayed commit message
    libra cherry-pick --empty=drop abc1234  Skip the pick if it is already upstream
    libra cherry-pick --continue           Resume after resolving conflicts
    libra cherry-pick --abort              Cancel and restore the original HEAD
    libra cherry-pick --json abc1234       Structured JSON output for agents";

// ── Typed error ──────────────────────────────────────────────────────

/// Where an untracked-overwrite refusal stopped a pick (ADR-HF-04 U8-U14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UntrackedStop {
    /// No commit of this invocation was applied yet: nothing was written.
    BeforeAnyWrite,
    /// A commit-per-pick sequence stopped before this commit and saved its state.
    SequenceStopped,
    /// A `--no-commit` pick stopped after earlier commits were already staged.
    NoCommitPartial,
}

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

    /// The pick's three-way inputs carry a gitlink the cherry-pick would have
    /// to arbitrate. Refused before any index/worktree write (ADR-MG-01) rather
    /// than silently dropped from the picked change set.
    #[error("{0}")]
    GitlinkUnsupported(String),

    #[error("commit {0} is empty (its change set is empty)")]
    EmptyCommit(String),

    #[error("commit {0} became redundant after replay (no changes to apply)")]
    RedundantCommit(String),

    #[error("commit {0} has an empty commit message")]
    EmptyMessage(String),

    #[error("failed to cherry-pick {commit}: {reason}")]
    Conflict { commit: String, reason: String },

    /// ADR-HF-04: a new pick refuses to start while the index still has
    /// unmerged entries, before any index, worktree, ref or sequence write.
    #[error("cherry-pick is not possible because the index has unmerged entries")]
    UnmergedIndex(Vec<String>),

    /// A `--no-commit` pick stopped on conflicts. No resumable sequence is
    /// written, so the guidance points at `libra add`, not `--continue`.
    #[error(
        "failed to cherry-pick {commit}: conflicts in {paths} path(s); a '--no-commit' pick leaves no sequence to continue"
    )]
    NoCommitConflict { commit: String, paths: usize },

    /// ADR-HF-04 (U8-U10): a pick would overwrite an untracked working tree
    /// file. Refused before any index, worktree, ref or sequence write.
    #[error(
        "failed to cherry-pick {commit}: untracked working tree file would be overwritten: {path}"
    )]
    UntrackedOverwrite {
        commit: String,
        path: String,
        stop: UntrackedStop,
    },

    /// #477 HF-31: an interrupted `--skip`/`--abort` left its phase marker;
    /// the same verb must finish before the sequence can continue.
    #[error("an interrupted 'libra cherry-pick --{0}' has not finished")]
    ControlPending(ControlPhase),

    /// #477 HF-02 / M-CONT C4: `--continue` after an external conclusion
    /// refuses when the index already has staged changes the next pick would
    /// overwrite.
    #[error("your local changes would be overwritten by cherry-pick")]
    LocalChangesWouldBeOverwritten,

    /// #477 HF-01: the row claims an externally concluded stop but has no
    /// remaining commits, which no writer produces.
    #[error("cherry-pick state is inconsistent: {0}")]
    CorruptState(String),

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
    #[error("unsupported merge.conflictStyle '{0}' (expected 'merge', 'diff3', or 'zdiff3')")]
    InvalidConflictStyle(String),

    /// The `merge.conflictStyle` config could not be read (config-store I/O
    /// failure) — never a silent default-style fall-back.
    #[error("failed to read merge.conflictStyle config: {0}")]
    ConflictStyleRead(String),

    #[error("failed to read merge.default config: {0}")]
    MergeDriverConfigRead(String),
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
            Self::Unsupported(_) | Self::GitlinkUnsupported(_) => StableErrorCode::Unsupported,
            Self::EmptyCommit(_) => StableErrorCode::CliInvalidArguments,
            Self::RedundantCommit(_) => StableErrorCode::CliInvalidArguments,
            Self::EmptyMessage(_) => StableErrorCode::CliInvalidArguments,
            Self::Conflict { .. } => StableErrorCode::ConflictUnresolved,
            Self::UnmergedIndex(_)
            | Self::NoCommitConflict { .. }
            | Self::UntrackedOverwrite { .. }
            | Self::LocalChangesWouldBeOverwritten => StableErrorCode::ConflictUnresolved,
            Self::InProgress => StableErrorCode::ConflictOperationBlocked,
            Self::NoCherryPickInProgress => StableErrorCode::RepoStateInvalid,
            Self::CorruptState(_) => StableErrorCode::RepoCorrupt,
            Self::ControlPending(_) => StableErrorCode::RepoStateInvalid,
            Self::WrongBranch { .. } => StableErrorCode::RepoStateInvalid,
            Self::LoadObject(_) => StableErrorCode::IoReadFailed,
            Self::SaveFailed(_) => StableErrorCode::IoWriteFailed,
            Self::InvalidConflictStyle(_) => StableErrorCode::RepoStateInvalid,
            Self::ConflictStyleRead(_) => StableErrorCode::IoReadFailed,
            Self::MergeDriverConfigRead(_) => StableErrorCode::IoReadFailed,
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
            CherryPickError::GitlinkUnsupported(_) => CliError::failure(message)
                .with_stable_code(stable_code)
                .with_hint(
                    "submodule merging is a permanent non-goal; resolve the submodule pointer outside Libra",
                )
                .with_hint(
                    "or drop the gitlink entry from the commits involved so no submodule decision is needed",
                ),
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
            CherryPickError::UnmergedIndex(paths) => unmerged_index_cli_error(message, &paths),
            CherryPickError::NoCommitConflict { .. } => CliError::failure(message)
                .with_stable_code(stable_code)
                .with_hint(
                    "resolve the conflicts and 'libra add' (or 'libra rm') the paths, then 'libra commit'",
                )
                .with_hint("or discard the staged pick with 'libra reset --hard'"),
            CherryPickError::UntrackedOverwrite { path, stop, .. } => {
                let hint = match stop {
                    UntrackedStop::BeforeAnyWrite => format!(
                        "move or remove '{path}', then run the same command again; nothing was written"
                    ),
                    UntrackedStop::SequenceStopped => format!(
                        "the sequence stopped before this commit and nothing of it was written; move or remove '{path}', then run 'libra cherry-pick --continue' (or --skip / --abort)"
                    ),
                    UntrackedStop::NoCommitPartial => format!(
                        "earlier picks of this '--no-commit' run stay staged; move or remove '{path}', then pick the remaining commits again, or discard everything with 'libra reset --hard'"
                    ),
                };
                CliError::failure(message)
                    .with_stable_code(stable_code)
                    .with_hint(hint)
            }
            CherryPickError::InProgress => CliError::failure(message)
                .with_stable_code(stable_code)
                .with_hint(
                    "finish it with 'libra cherry-pick --continue'/--skip, or cancel with --abort/--quit",
                ),
            CherryPickError::NoCherryPickInProgress => CliError::failure(message)
                .with_stable_code(stable_code)
                .with_hint("there is no cherry-pick to --continue/--skip/--abort/--quit"),
            CherryPickError::CorruptState(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("cancel the sequence with 'libra cherry-pick --abort'")
                .with_hint("or forget it with 'libra cherry-pick --quit'"),
            CherryPickError::LocalChangesWouldBeOverwritten => CliError::failure(message)
                .with_stable_code(stable_code)
                .with_hint("commit or stash the staged changes before continuing")
                .with_hint("or skip the stopped commit with 'libra cherry-pick --skip'"),
            CherryPickError::ControlPending(phase) => CliError::failure(message)
                .with_stable_code(stable_code)
                .with_hint(format!("run 'libra cherry-pick --{phase}' again to finish it"))
                .with_hint("or forget the sequence with 'libra cherry-pick --quit'"),
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
                .with_hint("set merge.conflictStyle to 'merge' (default), 'diff3', or 'zdiff3'"),
            CherryPickError::ConflictStyleRead(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("check repository integrity and retry"),
            CherryPickError::MergeDriverConfigRead(_) => CliError::fatal(message)
                .with_stable_code(stable_code)
                .with_hint("check repository config readability and retry"),
        }
    }
}

/// Most conflicted paths named in the unmerged-index refusal hint (ADR-HF-04).
const UNMERGED_HINT_LIMIT: usize = 10;

/// Paths that still carry unmerged (stage 1-3) index entries, sorted by path.
/// A missing index file is a clean index.
pub(crate) fn unmerged_index_paths() -> Result<Vec<String>, String> {
    let index_file = path::index();
    if !index_file.exists() {
        return Ok(Vec::new());
    }
    let index = Index::load(&index_file).map_err(|e| e.to_string())?;
    Ok(crate::command::unmerged::collect(&index)
        .into_iter()
        .map(|entry| entry.path.display().to_string())
        .collect())
}

/// `unmerged paths: a, b (and N more)`, naming at most [`UNMERGED_HINT_LIMIT`] paths.
fn unmerged_paths_hint(paths: &[String]) -> String {
    let mut listed = paths
        .iter()
        .take(UNMERGED_HINT_LIMIT)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > UNMERGED_HINT_LIMIT {
        listed.push_str(&format!(
            " (and {} more)",
            paths.len() - UNMERGED_HINT_LIMIT
        ));
    }
    format!("unmerged paths: {listed}")
}

/// The refusal a new cherry-pick or revert returns on an unmerged index
/// (ADR-HF-04): exit 128 with `LBR-CONFLICT-001`, the conflicted paths, and how
/// to resolve or discard them.
pub(crate) fn unmerged_index_cli_error(message: String, paths: &[String]) -> CliError {
    CliError::fatal(message)
        .with_stable_code(StableErrorCode::ConflictUnresolved)
        .with_hint(unmerged_paths_hint(paths))
        .with_hint(
            "resolve each path and 'libra add' (or 'libra rm') it, or discard the conflict with 'libra reset --hard'",
        )
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
    /// A pick would overwrite an untracked working tree file; raised before the
    /// index or worktree is written (ADR-HF-04 U8-U10).
    UntrackedOverwrite(String),
    LoadObject(String),
    SaveFailed(String),
    /// Unsupported `merge.conflictStyle` value — raised BEFORE the conflicted
    /// index/worktree state is written, so nothing is mutated.
    InvalidConflictStyle(String),
    /// `merge.conflictStyle` config-store read failure.
    ConflictStyleRead(String),
    MergeDriverConfigRead(String),
    /// The pick's three-way inputs carry a gitlink the cherry-pick would have to
    /// arbitrate — refused before any index/worktree write (ADR-MG-01).
    GitlinkUnsupported(String),
}

/// Serializable snapshot of the commit-modifier options for a cherry-pick
/// sequence, persisted in the sequencer payload so `--continue`/`--skip`
/// rebuild the same commit shape after a conflict.
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
struct CherryPickOpts {
    /// §C.5 conflict-phase discriminator: `true` only when the sequence is
    /// STOPPED ON A CONFLICT. `resume_picks` persists position BEFORE every
    /// attempt, so the row alone cannot distinguish "stopped on conflict"
    /// from "stopped on a hard error mid-resume" — and `CHERRY_PICK_HEAD`
    /// is defined for the former only. Set on each conflict stop, stripped
    /// on every resume, absent (= false) in rows from older binaries.
    #[serde(default)]
    stopped_on_conflict: bool,
    /// #477 HF-31: set while an interrupted `--skip`/`--abort` still has to
    /// finish; every position write clears it. Absent in older rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    control_phase: Option<ControlPhase>,
    /// #477 HF-31: `--ff` is a sequence option, so resumed picks fast-forward
    /// the way the starting run would have. Absent (= false) in older rows.
    #[serde(default)]
    ff: bool,
    /// #477 HF-31: the commit a fast-forward pick is about to move HEAD to,
    /// written just before its `reset --hard`. `--continue` skips the stopped
    /// commit only when this names it and HEAD already points at it; every
    /// position write clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ff_landing: Option<String>,
    /// #477 HF-31: identifies the run that claimed a multi-commit sequence, so
    /// releasing that claim after an early refusal cannot erase a row another
    /// start claimed since. Absent in older rows and single-commit picks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claim_token: Option<String>,
    /// #477 HF-01: the stopped commit was concluded from outside the sequence
    /// (a later `reset`, and from HF-29 a later `commit`). Additive, so a row
    /// written here still loads on binaries that predate the field (ER-HF-02),
    /// which keep seeing an ordinary stopped sequence.
    #[serde(default)]
    stop_concluded: bool,
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
    /// Explicit rerere staging policy. `None` inherits `rerere.autoUpdate`;
    /// either value must survive a conflict + resumed sequence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rerere_autoupdate: Option<bool>,
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
    /// Effective (last supplied) `-X` side preference. Later commits in a
    /// resumed sequence must resolve overlapping hunks the same way.
    #[serde(default)]
    strategy_option: Option<MergeFavor>,
}

impl CherryPickOpts {
    fn from_args(args: &CherryPickArgs) -> Self {
        Self {
            stopped_on_conflict: false,
            control_phase: None,
            ff: args.ff,
            ff_landing: None,
            claim_token: None,
            stop_concluded: false,
            append_source: args.append_source,
            signoff: args.signoff,
            edit: args.edit,
            allow_empty: args.allow_empty,
            allow_empty_message: args.allow_empty_message,
            keep_redundant_commits: args.keep_redundant_commits,
            gpg_sign: args.gpg_sign,
            rerere_autoupdate: rerere_autoupdate_override(args),
            mainline: args.mainline,
            cleanup: args.cleanup.clone(),
            empty: args.empty.clone(),
            strategy_option: args.strategy_option.last().copied(),
        }
    }

    /// Rebuild a minimal [`CherryPickArgs`] carrying just these options (used to
    /// re-run the commit-assembly path during `--continue`/`--skip`). EVERY
    /// commit-shaping modifier must round-trip so resumed commits keep the same
    /// shape — e.g. a `-S` sequence stays signed and a `-m <n>` sequence keeps
    /// applying later merge commits along the chosen parent.
    fn into_args(self) -> CherryPickArgs {
        CherryPickArgs {
            ff: self.ff,
            append_source: self.append_source,
            signoff: self.signoff,
            edit: self.edit,
            allow_empty: self.allow_empty,
            allow_empty_message: self.allow_empty_message,
            keep_redundant_commits: self.keep_redundant_commits,
            gpg_sign: self.gpg_sign,
            rerere_autoupdate: self.rerere_autoupdate == Some(true),
            no_rerere_autoupdate: self.rerere_autoupdate == Some(false),
            mainline: self.mainline,
            cleanup: self.cleanup,
            empty: self.empty,
            strategy_option: self.strategy_option.into_iter().collect(),
            ..Default::default()
        }
    }
}

/// A `--skip`/`--abort` that recorded its intent before resetting (#477 HF-31).
/// An interruption after the reset leaves the marker, so the same verb can be
/// re-run to finish, and `--continue` refuses instead of committing the reset
/// index as the stopped commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum ControlPhase {
    Skip,
    Abort,
}

impl std::fmt::Display for ControlPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ControlPhase::Skip => "skip",
            ControlPhase::Abort => "abort",
        })
    }
}

const fn rerere_autoupdate_override(args: &CherryPickArgs) -> Option<bool> {
    if args.rerere_autoupdate {
        Some(true)
    } else if args.no_rerere_autoupdate {
        Some(false)
    } else {
        None
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

    /// Resolve only overlapping three-way merge hunks in favor of the selected
    /// side. Repeatable; the last value wins.
    #[clap(
        short = 'X',
        long = "strategy-option",
        value_name = "option",
        value_enum,
        action = clap::ArgAction::Append
    )]
    pub strategy_option: Vec<MergeFavor>,

    // ── Unsupported Git options captured for explicit rejection ──
    #[clap(long = "strategy", value_name = "name", hide = true)]
    pub strategy: Option<String>,
    /// Auto-stage a rerere-replayed resolution for this pick, overriding
    /// `rerere.autoUpdate`. The last rerere toggle wins.
    #[clap(long = "rerere-autoupdate", overrides_with = "no_rerere_autoupdate")]
    pub rerere_autoupdate: bool,
    /// Do not auto-stage a rerere-replayed resolution for this pick, overriding
    /// `rerere.autoUpdate`. The last rerere toggle wins.
    #[clap(long = "no-rerere-autoupdate", overrides_with = "rerere_autoupdate")]
    pub no_rerere_autoupdate: bool,
    #[clap(long = "commit", hide = true)]
    pub commit: bool,
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
    // Part C W1 (§C.4.2): cherry-pick is now safe in a LINKED worktree — its
    // entire state is worktree-scoped (`sequence_state` keyed by `worktree_id`
    // plus the local-gitdir `CHERRY_PICK_MSG`; there is no `CHERRY_PICK_HEAD`
    // file), the start-time mutex `detect_active_operation` no longer sees
    // another worktree's sequence, and it advances only THIS worktree's own
    // current branch. Two worktrees can cherry-pick concurrently without
    // interfering, so the `ensure_main_worktree` guard is lifted here.
    //
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
        CherryPickSingleError::UntrackedOverwrite(path) => CherryPickError::UntrackedOverwrite {
            commit: commit_label.to_string(),
            path,
            stop: UntrackedStop::BeforeAnyWrite,
        },
        CherryPickSingleError::Conflicted(paths) => CherryPickError::Conflict {
            commit: commit_label.to_string(),
            reason: format!("conflicts in {} path(s)", paths.len()),
        },
        CherryPickSingleError::LoadObject(r) => CherryPickError::LoadObject(r),
        CherryPickSingleError::SaveFailed(r) => CherryPickError::SaveFailed(r),
        CherryPickSingleError::InvalidConflictStyle(v) => CherryPickError::InvalidConflictStyle(v),
        CherryPickSingleError::ConflictStyleRead(r) => CherryPickError::ConflictStyleRead(r),
        CherryPickSingleError::MergeDriverConfigRead(r) => {
            CherryPickError::MergeDriverConfigRead(r)
        }
        CherryPickSingleError::GitlinkUnsupported(detail) => {
            CherryPickError::GitlinkUnsupported(detail)
        }
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

/// Fail closed on a row whose external-conclusion marker contradicts its todo
/// (#477 HF-01): no writer marks a sequence that has nothing left to pick, so
/// the row is corrupt. `--abort`/`--quit` stay available to clean it up.
fn reject_inconsistent_conclusion(state: &CherryPickState) -> Result<(), CherryPickError> {
    if state.stop_concluded && state.todo.is_empty() {
        return Err(CherryPickError::CorruptState(
            "the stopped commit is marked concluded but no commits remain".to_string(),
        ));
    }
    Ok(())
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
    crate::command::reset::execute_safe_internal(
        crate::command::reset::ResetArgs {
            target: Some(target.to_string()),
            soft: false,
            mixed: false,
            hard: true,
            merge: false,
            keep: false,
            pathspecs: Vec::new(),
            pathspec_separator: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            no_refresh: false,
            patch: false,
            auto_advance: false,
            no_auto_advance: false,
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

    // ADR-HF-04: refuse before resolving targets or writing anything while the
    // index still has unmerged entries (Git: "Cherry-picking is not possible
    // because you have unmerged files."). Sequencer controls returned above.
    let unmerged = unmerged_index_paths()
        .map_err(|e| CherryPickError::LoadObject(format!("failed to load index: {e}")))?;
    if !unmerged.is_empty() {
        return Err(CherryPickError::UnmergedIndex(unmerged));
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

    // ADR-MG-01: refuse the whole sequence before the first pick mutates the
    // index, the working tree, or HEAD.
    preflight_pick_gitlinks(&commit_ids, &args).await?;

    let mut acc = PickAccumulator::default();
    // #477 HF-31 X1/X7: a commit-per-pick run of several commits claims its
    // sequence row before the first pick writes anything (Git creates its
    // sequencer directory before the first pick, too) and moves the row with
    // every landing, so an interruption between two picks still leaves
    // `--continue`/`--skip`/`--abort` a sequence to act on.
    let sequence_anchor = match head_orig {
        Some(orig) if commit_ids.len() > 1 && !args.no_commit => Some(orig),
        _ => None,
    };
    let (opts_json, claim_needle) = if sequence_anchor.is_some() {
        opts_json_with_claim_token(&opts_json)
    } else {
        (opts_json, None)
    };
    if let Some(orig) = sequence_anchor {
        claim_sequence_start(&CherryPickState {
            head_name: head_name.clone(),
            head_orig: orig,
            current_oid: commit_ids[0],
            stop_concluded: false,
            todo: commit_ids[1..].iter().copied().collect(),
            opts_json: opts_json_with_conflict_flag(&opts_json, false),
        })
        .await?;
    }
    let row_exists = sequence_anchor.is_some();
    for (i, commit_id) in commit_ids.iter().enumerate() {
        let advance = match sequence_anchor {
            Some(orig) => {
                let rest: VecDeque<ObjectHash> = commit_ids[i + 1..].iter().copied().collect();
                SequenceAdvance::after(&head_name, orig, commit_id, &rest, &opts_json)
            }
            None => SequenceAdvance::NoRow,
        };
        match cherry_pick_single_commit(commit_id, &args, output, &advance).await {
            Ok(outcome) => {
                if matches!(outcome, PickOutcome::Dropped(_)) {
                    // HF-31 X6: a dropped pick moves no HEAD, so no transaction
                    // carried the advance; apply it here.
                    advance
                        .apply_without_head_move()
                        .await
                        .map_err(CherryPickError::SaveFailed)?;
                    after_drop_failpoint()?;
                }
                record_outcome(outcome, commit_id, &mut acc)
            }
            Err(CherryPickSingleError::UntrackedOverwrite(path)) if i > 0 => {
                // Earlier picks already landed (ADR-HF-04 U14). A commit-per-pick
                // run stops at this commit with its state saved, so `--continue`
                // re-attempts it once the file is moved.
                let label = args.commits[i].clone();
                if args.no_commit {
                    return Err(CherryPickError::UntrackedOverwrite {
                        commit: label,
                        path,
                        stop: UntrackedStop::NoCommitPartial,
                    });
                }
                let head_orig = head_orig.ok_or_else(|| {
                    CherryPickError::LoadObject("failed to resolve original HEAD".to_string())
                })?;
                let state = CherryPickState {
                    head_name: head_name.clone(),
                    head_orig,
                    current_oid: *commit_id,
                    stop_concluded: false,
                    todo: commit_ids[i + 1..].iter().copied().collect(),
                    opts_json: opts_json_with_conflict_flag(&opts_json, false),
                };
                let persisted = if row_exists {
                    state.save().await
                } else {
                    state.claim_start().await
                };
                persisted.map_err(CherryPickError::SaveFailed)?;
                return Err(CherryPickError::UntrackedOverwrite {
                    commit: label,
                    path,
                    stop: UntrackedStop::SequenceStopped,
                });
            }
            Err(CherryPickSingleError::Conflicted(paths)) => {
                let label = args.commits[i].clone();
                if args.no_commit {
                    // `--no-commit` sequences have no per-step snapshot, so a
                    // conflict is terminal: no resumable state is written.
                    return Err(CherryPickError::NoCommitConflict {
                        commit: label,
                        paths: paths.len(),
                    });
                }
                let head_orig = head_orig.ok_or_else(|| {
                    CherryPickError::LoadObject("failed to resolve original HEAD".to_string())
                })?;
                let state = CherryPickState {
                    head_name: head_name.clone(),
                    head_orig,
                    current_oid: *commit_id,
                    stop_concluded: false,
                    todo: commit_ids[i + 1..].iter().copied().collect(),
                    // This claim only happens on a conflict stop — say so
                    // durably (§C.5 conflict-phase discriminator).
                    opts_json: opts_json_with_conflict_flag(&opts_json, true),
                };
                // A multi-commit run already claimed its row before the first
                // pick (§C.4.4, HF-31), so the stop replaces the row it owns; a
                // single-commit stop makes the first, atomic claim here.
                let persisted = if row_exists {
                    state.save().await
                } else {
                    state.claim_start().await
                };
                persisted.map_err(CherryPickError::SaveFailed)?;
                return Err(CherryPickError::Conflict {
                    commit: label,
                    reason: format!("conflicts in {} path(s)", paths.len()),
                });
            }
            Err(other) => {
                // Nothing landed yet (HEAD unmoved): release this run's claim so
                // the refusal leaves no sequence, as before HF-31. Once HEAD has
                // moved, the row stays for `--continue`/`--skip`/`--abort`.
                if row_exists && Head::current_commit().await == head_orig {
                    release_sequence_claim(claim_needle.as_deref())
                        .await
                        .map_err(|error| {
                        CherryPickError::SaveFailed(format!(
                            "{error}; the refused pick left its sequence claim in place (run 'libra cherry-pick --quit')"
                        ))
                    })?;
                }
                return Err(map_single_error(other, &args.commits[i]));
            }
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
/// `opts_json` with the §C.5 conflict-phase flag set or cleared. Falls back
/// to the input on a parse failure — the option payload is validated where
/// it is USED; the flag must never turn a working sequence into an error.
/// Whether a persisted options blob carries the `stopped_on_conflict` key at
/// all. Rows from binaries that predate the flag omit it and must keep the old
/// finalize-on-continue behavior.
fn opts_json_has_conflict_flag(opts_json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(opts_json)
        .ok()
        .is_some_and(|value| value.get("stopped_on_conflict").is_some())
}

fn opts_json_with_conflict_flag(opts_json: &str, stopped_on_conflict: bool) -> String {
    match serde_json::from_str::<CherryPickOpts>(opts_json) {
        Ok(mut opts) => {
            opts.stopped_on_conflict = stopped_on_conflict;
            // A position write means no `--skip`/`--abort` is still pending,
            // no fast-forward is between its row write and its reset, and the
            // sequence is no longer sitting on an externally concluded stop.
            opts.control_phase = None;
            opts.ff_landing = None;
            opts.stop_concluded = false;
            serde_json::to_string(&opts).unwrap_or_else(|_| opts_json.to_string())
        }
        Err(_) => opts_json.to_string(),
    }
}

/// `opts_json` marked as externally concluded (#477 HF-01). Options this
/// binary cannot read are an error rather than a silent no-op: the row is left
/// byte-identical and the caller (`reset`) warns about the state it could not
/// update, naming `libra cherry-pick --quit`.
fn opts_json_with_stop_concluded(opts_json: &str) -> Result<String, String> {
    let mut opts: CherryPickOpts = serde_json::from_str(opts_json)
        .map_err(|e| format!("failed to read the stopped sequence's saved options: {e}"))?;
    opts.stop_concluded = true;
    serde_json::to_string(&opts).map_err(|e| format!("failed to record the concluded stop: {e}"))
}

/// End the cherry-pick item a stopped sequence is sitting on, because a later
/// `reset` concluded it (ADR-HF-03 items 1 and 4, #477 HF-01). Reused by HF-29
/// for the `commit` entry point.
pub(crate) async fn snapshot_stopped_cherry_pick()
-> Result<Option<crate::internal::sequencer::SequenceState>, String> {
    sequencer::snapshot_stopped_sequence(SequenceKind::CherryPick).await
}

/// Conclude exactly the row `snapshot` describes (see
/// [`sequencer::conclude_stopped_sequence`] for why the caller snapshots first).
pub(crate) async fn conclude_stopped_cherry_pick(
    snapshot: crate::internal::sequencer::SequenceState,
) -> Result<crate::internal::sequencer::ExternalConclusion, String> {
    sequencer::conclude_stopped_sequence(snapshot, opts_json_with_stop_concluded).await
}

/// The `--skip`/`--abort` still pending in a row's options (#477 HF-31); `None`
/// for unmarked rows, rows from older binaries and unreadable options.
fn opts_json_control_phase(opts_json: &str) -> Option<ControlPhase> {
    serde_json::from_str::<CherryPickOpts>(opts_json)
        .ok()
        .and_then(|opts| opts.control_phase)
}

/// `opts_json` marked with a pending control phase, or `None` when the options
/// cannot be read (the control verb then proceeds unmarked, as before).
fn opts_json_with_control_phase(opts_json: &str, phase: ControlPhase) -> Option<String> {
    let mut opts = serde_json::from_str::<CherryPickOpts>(opts_json).ok()?;
    opts.control_phase = Some(phase);
    serde_json::to_string(&opts).ok()
}

/// `opts_json` naming `landing` as the fast-forward about to move HEAD (HF-31
/// X2); unreadable options are returned unchanged (no marker, so `--continue`
/// re-attempts instead of skipping).
fn opts_json_with_ff_landing(opts_json: &str, landing: &ObjectHash) -> String {
    match serde_json::from_str::<CherryPickOpts>(opts_json) {
        Ok(mut opts) => {
            opts.ff_landing = Some(landing.to_string());
            serde_json::to_string(&opts).unwrap_or_else(|_| opts_json.to_string())
        }
        Err(_) => opts_json.to_string(),
    }
}

/// `opts_json` stamped with a fresh claim token, plus the payload fragment a
/// fenced release matches (HF-31). Unreadable options stay unstamped, and the
/// release then falls back to the scoped clear.
fn opts_json_with_claim_token(opts_json: &str) -> (String, Option<String>) {
    let Ok(mut opts) = serde_json::from_str::<CherryPickOpts>(opts_json) else {
        return (opts_json.to_string(), None);
    };
    let token = uuid::Uuid::new_v4().to_string();
    opts.claim_token = Some(token.clone());
    match serde_json::to_string(&opts) {
        Ok(stamped) => (stamped, Some(format!("\"claim_token\":\"{token}\""))),
        Err(_) => (opts_json.to_string(), None),
    }
}

/// Test-only stand-in for a concurrent `--quit` followed by a new start that
/// claims this worktree's sequence before a refused run releases its own
/// claim (HF-31 X7; gated on `LIBRA_TEST`).
async fn reclaim_before_release_failpoint() {
    if std::env::var_os("LIBRA_TEST").is_some()
        && std::env::var_os("LIBRA_TEST_CHERRY_PICK_RECLAIM_BEFORE_RELEASE").is_some()
        && let Ok(Some(mut row)) = CherryPickState::load().await
    {
        let _ = CherryPickState::clear().await;
        row.opts_json = opts_json_with_claim_token(&row.opts_json).0;
        let _ = row.claim_start().await;
    }
}

/// Test-only stand-in for a concurrent start that claims this worktree's
/// sequence between the in-progress check and this run's claim (HF-31 X7;
/// gated on `LIBRA_TEST`).
async fn race_before_claim_failpoint(state: &CherryPickState) {
    if std::env::var_os("LIBRA_TEST").is_some()
        && std::env::var_os("LIBRA_TEST_CHERRY_PICK_RACE_BEFORE_CLAIM").is_some()
    {
        let _ = state.claim_start().await;
    }
}

/// Claim a multi-commit sequence before its first pick writes anything
/// (HF-31 X1/X7). Losing the claim to a concurrent start is the ordinary
/// in-progress refusal, and nothing has been written yet.
async fn claim_sequence_start(state: &CherryPickState) -> Result<(), CherryPickError> {
    race_before_claim_failpoint(state).await;
    state.claim_start().await.map_err(|error| {
        if error.contains("already in progress") {
            CherryPickError::InProgress
        } else {
            CherryPickError::SaveFailed(error)
        }
    })
}

/// Test-only interruption right after a dropped pick advanced the sequence row
/// (HF-31 X6; gated on `LIBRA_TEST`).
fn after_drop_failpoint() -> Result<(), CherryPickError> {
    if std::env::var_os("LIBRA_TEST").is_some()
        && std::env::var_os("LIBRA_TEST_CHERRY_PICK_FAIL_AFTER_DROP").is_some()
    {
        return Err(CherryPickError::SaveFailed(
            "test-injected cherry-pick interruption after a dropped pick".to_string(),
        ));
    }
    Ok(())
}

/// Release this run's sequence claim after a refusal before anything landed
/// (HF-31 X7). `LIBRA_TEST_CHERRY_PICK_FAIL_RELEASE_CLAIM` (gated on
/// `LIBRA_TEST`) injects a failure so the caller's error path is testable.
async fn release_sequence_claim(claim_needle: Option<&str>) -> Result<(), String> {
    if std::env::var_os("LIBRA_TEST").is_some()
        && std::env::var_os("LIBRA_TEST_CHERRY_PICK_FAIL_RELEASE_CLAIM").is_some()
    {
        return Err("test-injected failure releasing the sequence claim".to_string());
    }
    reclaim_before_release_failpoint().await;
    match claim_needle {
        // Fenced by this run's token: a row claimed by a later start stays.
        Some(needle) => sequencer::clear_if_payload_contains(SequenceKind::CherryPick, needle)
            .await
            .map(|_| ()),
        None => CherryPickState::clear().await,
    }
}

/// Test-only interruption right after `--skip`/`--abort` reset the index and
/// worktree, before the sequence row changes (gated on `LIBRA_TEST`).
fn after_control_reset_failpoint() -> Result<(), CherryPickError> {
    if std::env::var_os("LIBRA_TEST").is_some()
        && std::env::var_os("LIBRA_TEST_CHERRY_PICK_FAIL_AFTER_CONTROL_RESET").is_some()
    {
        return Err(CherryPickError::SaveFailed(
            "test-injected cherry-pick interruption after the control reset".to_string(),
        ));
    }
    Ok(())
}

/// ADR-MG-01 gate for a WHOLE cherry-pick sequence, ahead of its first write.
///
/// The per-pick guard inside `cherry_pick_single_commit` runs after earlier
/// picks have already been committed and after `resume_picks` has persisted the
/// sequencer row, so a later refusal would leave the sequence half-applied.
/// This asks the conservative whole-sequence question first: every input tree of
/// every pick, plus the index the picks land on, must record the same pointer
/// for a given gitlink path (`merge::ensure_gitlinks_uniform_across_inputs`).
/// Resolution failures are deliberately NOT reported here (an unresolvable
/// object, an out-of-range `-m`): `cherry_pick_single_commit` owns those
/// messages, and a preflight that failed first would change them. Only a
/// gitlink refusal escapes.
async fn preflight_pick_gitlinks(
    commit_ids: &[ObjectHash],
    args: &CherryPickArgs,
) -> Result<(), CherryPickError> {
    let mut inputs: Vec<merge::GitlinkEntries> = Vec::new();
    // Mirror the HEAD *and the index* the sequence will actually see. `--ff`
    // eligibility is judged exactly as `cherry_pick_single_commit` judges it:
    // an eligible pick fast-forwards, adopting the picked tree wholesale
    // (deciding nothing) while REPLACING the index — so the "ours" side of any
    // later pick is that tree, not the index we started from. A pick that
    // replays creates a new commit, after which no later pick can fast-forward.
    let mut simulated_head = Head::current_commit().await;
    let index = Index::load(path::index())
        .map_err(|e| CherryPickError::LoadObject(format!("failed to load current index: {e}")))?;
    let mut simulated_ours = merge::index_gitlink_entries(&index);
    // Every early exit below stops modelling FURTHER picks — it must never
    // discard the picks already modelled, or a divergent pointer in an earlier
    // pick would escape the gate and be applied before the per-pick guard
    // refused it.
    macro_rules! stop_modelling {
        () => {
            return decide_pick_gitlinks(&inputs)
        };
    }
    for commit_id in commit_ids {
        let Ok(commit) = load_object::<Commit>(commit_id) else {
            stop_modelling!();
        };
        let parent_count = commit.parent_commit_ids.len();
        if args.ff
            && !args.no_commit
            && !args.append_source
            && !args.signoff
            && !args.edit
            && args.mainline.is_none()
            && parent_count == 1
            && simulated_head == Some(commit.parent_commit_ids[0])
        {
            let Ok(adopted) = merge::commit_gitlink_entries(&commit) else {
                stop_modelling!();
            };
            simulated_head = Some(*commit_id);
            simulated_ours = adopted;
            continue;
        }
        // The diff base, honoring `-m <n>` exactly as the pick does. Any shape
        // the pick would reject as a usage error leaves the preflight silent.
        let base_parent = match (parent_count, args.mainline) {
            (0, None) => None,
            (1, None) => Some(commit.parent_commit_ids[0]),
            (n, Some(m)) if n > 1 && m >= 1 && m <= n => Some(commit.parent_commit_ids[m - 1]),
            _ => stop_modelling!(),
        };
        // A root commit diffs against the CANONICAL EMPTY TREE, which declares
        // no gitlink at all — so a submodule the picked commit does declare is
        // an addition the pick would have to arbitrate. Modelling that as an
        // empty base is what makes the refusal land here rather than mid-sequence.
        let base_gitlinks = match base_parent {
            Some(parent_id) => {
                let Ok(parent) = load_object::<Commit>(&parent_id) else {
                    stop_modelling!();
                };
                let Ok(parent_gitlinks) = merge::commit_gitlink_entries(&parent) else {
                    stop_modelling!();
                };
                parent_gitlinks
            }
            None => merge::GitlinkEntries::new(),
        };
        // `--allow-empty` precedence: a pick whose change set is empty is
        // rejected as `EmptyCommit` BEFORE the pick's own gitlink gate runs, so
        // the preflight must not overtake that error.
        let parent_tree_id = match base_parent {
            Some(parent_id) => match load_object::<Commit>(&parent_id) {
                Ok(parent) => parent.tree_id,
                Err(_) => stop_modelling!(),
            },
            None => ObjectHash::from_type_and_data(ObjectType::Tree, &[]),
        };
        if commit.tree_id == parent_tree_id && !args.allow_empty {
            stop_modelling!();
        }
        let Ok(picked) = merge::commit_gitlink_entries(&commit) else {
            stop_modelling!();
        };
        inputs.push(simulated_ours.clone());
        inputs.push(picked.clone());
        inputs.push(base_gitlinks);
        // Whatever this pick produces, the uniform gate below requires it to
        // agree with `picked` — so that is the "ours" side the next pick sees.
        simulated_ours = picked;
        simulated_head = None;
    }
    decide_pick_gitlinks(&inputs)
}

/// Verdict over the inputs modelled so far. An empty set means no pick performs
/// a three-way apply at all (every one fast-forwards), so there is nothing to
/// arbitrate.
fn decide_pick_gitlinks(inputs: &[merge::GitlinkEntries]) -> Result<(), CherryPickError> {
    if inputs.is_empty() {
        return Ok(());
    }
    merge::ensure_gitlinks_uniform_across_inputs("cherry-pick", inputs)
        .map_err(|refusal| CherryPickError::GitlinkUnsupported(refusal.to_string()))
}

async fn resume_picks(
    head_name: &str,
    head_orig: ObjectHash,
    mut todo: VecDeque<ObjectHash>,
    opts_args: &CherryPickArgs,
    opts_json: &str,
    output: &OutputConfig,
    acc: &mut PickAccumulator,
) -> Result<(), CherryPickError> {
    // Before the first `pending.save()` below: a gitlink refusal must not leave
    // the sequencer row advanced onto a pick that can never run (ADR-MG-01).
    let remaining: Vec<ObjectHash> = todo.iter().copied().collect();
    preflight_pick_gitlinks(&remaining, opts_args).await?;
    while let Some(commit_id) = todo.pop_front() {
        // Persist the position BEFORE attempting each commit so that whatever
        // happens — clean success, conflict, or a non-conflict hard error — the
        // `sequence_state` row already reflects `current_oid = commit_id` and
        // the remaining `todo`. This keeps state accurate even when the pick
        // fails with a non-conflict error after earlier resumed commits landed.
        // A pick that lands moves the row past `commit_id` in the same
        // transaction as HEAD (`SequenceAdvance`), so no crash can leave the row
        // naming a commit that already landed.
        let pending = CherryPickState {
            head_name: head_name.to_string(),
            head_orig,
            current_oid: commit_id,
            stop_concluded: false,
            todo: todo.clone(),
            // STRIPPED flag: this save happens before the attempt, so if the
            // pick stops on a NON-conflict error the row must not carry a
            // stale conflict claim from an earlier stop.
            opts_json: opts_json_with_conflict_flag(opts_json, false),
        };
        pending.save().await.map_err(CherryPickError::SaveFailed)?;
        let advance = SequenceAdvance::after(head_name, head_orig, &commit_id, &todo, opts_json);

        match cherry_pick_single_commit(&commit_id, opts_args, output, &advance).await {
            Ok(outcome) => {
                if matches!(outcome, PickOutcome::Dropped(_)) {
                    // HF-31 X6: a dropped pick moves no HEAD; advance the row here.
                    advance
                        .apply_without_head_move()
                        .await
                        .map_err(CherryPickError::SaveFailed)?;
                    after_drop_failpoint()?;
                }
                record_outcome(outcome, &commit_id, acc)
            }
            Err(CherryPickSingleError::UntrackedOverwrite(path)) => {
                // `pending` (conflict flag stripped) already records this commit
                // as the stop, so `--continue` re-attempts it (ADR-HF-04 U12/U13).
                return Err(CherryPickError::UntrackedOverwrite {
                    commit: commit_id.to_string(),
                    path,
                    stop: UntrackedStop::SequenceStopped,
                });
            }
            Err(CherryPickSingleError::Conflicted(paths)) => {
                // Re-persist WITH the conflict flag: `CHERRY_PICK_HEAD` is
                // defined only for a conflict stop (§C.5), and this is the
                // one place that knows which stop this is.
                let conflicted = CherryPickState {
                    opts_json: opts_json_with_conflict_flag(opts_json, true),
                    ..pending
                };
                conflicted
                    .save()
                    .await
                    .map_err(CherryPickError::SaveFailed)?;
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

/// #477 HF-02 / M-CONT C4: after an external conclusion the next pick applies
/// onto the current index. Staged changes would be overwritten, so refuse
/// before touching the sequence.
async fn refuse_staged_changes_on_concluded_continue() -> Result<(), CherryPickError> {
    let staged = crate::command::status::changes_to_be_committed_safe()
        .await
        .map_err(|e| CherryPickError::LoadObject(e.to_string()))?;
    if staged.is_empty() {
        Ok(())
    } else {
        Err(CherryPickError::LocalChangesWouldBeOverwritten)
    }
}

async fn run_cherry_pick_continue(
    output: &OutputConfig,
) -> Result<CherryPickOutput, CherryPickError> {
    let state = load_state_or_err().await?;
    ensure_on_state_branch(&state).await?;
    // The corrupt-row check runs before the control-phase one: a row that is
    // both marked concluded with nothing left and mid-`--skip` is corrupt, and
    // must fail closed (LBR-REPO-002) rather than look like a resumable phase.
    reject_inconsistent_conclusion(&state)?;
    if let Some(phase) = opts_json_control_phase(&state.opts_json) {
        return Err(CherryPickError::ControlPending(phase));
    }
    if state.stop_concluded {
        // #477 HF-02: a later reset/commit already concluded this stop. Do not
        // record the current index as that commit; apply the remaining todo.
        refuse_staged_changes_on_concluded_continue().await?;
        let opts: CherryPickOpts = serde_json::from_str(&state.opts_json).map_err(|e| {
            CherryPickError::LoadObject(format!("failed to read saved options: {e}"))
        })?;
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
        return Ok(CherryPickOutput {
            picked: acc.picked,
            dropped: acc.dropped,
            action: Some("continue".to_string()),
            ..Default::default()
        });
    }

    // The conflicted index must be fully resolved (no stage 1/2/3 left).
    let index = Index::load(path::index())
        .map_err(|e| CherryPickError::LoadObject(format!("failed to load index: {e}")))?;
    // Same guard: `--continue` builds a commit from the current index.
    crate::internal::layer::reject_layer_owned_entries(&index, "to continue the cherry-pick")
        .await
        .map_err(CherryPickError::LoadObject)?;
    if !merge::unresolved_conflicted_paths(&index, &[]).is_empty() {
        return Err(CherryPickError::Conflict {
            commit: short_display_hash(&state.current_oid.to_string()).to_string(),
            reason: "unresolved conflicts remain in the index".to_string(),
        });
    }

    let opts: CherryPickOpts = serde_json::from_str(&state.opts_json)
        .map_err(|e| CherryPickError::LoadObject(format!("failed to read saved options: {e}")))?;
    // A stop that was not a conflict (for example an untracked-overwrite
    // refusal, ADR-HF-04 U12-U14) never applied `current_oid`: re-attempt it
    // instead of recording the untouched index as that commit. Rows written
    // before the flag existed keep finalizing from the index.
    let reattempt_current =
        !opts.stopped_on_conflict && opts_json_has_conflict_flag(&state.opts_json);
    let ff_landed = opts.ff_landing.as_deref() == Some(state.current_oid.to_string().as_str());
    let opts_args = opts.into_args();

    if reattempt_current {
        let mut todo = state.todo;
        let mut acc = PickAccumulator::default();
        if ff_landed && Head::current_commit().await == Some(state.current_oid) {
            // HF-31 X2: a fast-forward pick marks its commit in the row, then
            // moves HEAD through `reset --hard` outside that write. A marked
            // commit that HEAD already points at landed before an interruption;
            // anything else (HF-31 X8: HEAD moved by hand) is re-attempted.
            acc.picked
                .push(make_entry(&state.current_oid, Some(state.current_oid)));
        } else {
            todo.push_front(state.current_oid);
        }
        resume_picks(
            &state.head_name,
            state.head_orig,
            todo,
            &opts_args,
            &state.opts_json,
            output,
            &mut acc,
        )
        .await?;
        return Ok(CherryPickOutput {
            picked: acc.picked,
            dropped: acc.dropped,
            action: Some("continue".to_string()),
            ..Default::default()
        });
    }

    // rerere: the conflict is resolved — record its postimage so an identical
    // conflict is auto-resolved next time. A no-op unless `rerere.enabled`.
    if let Err(error) =
        crate::command::rerere::auto_update(rerere_autoupdate_override(&opts_args)).await
    {
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
    // The resolved commit lands and the row moves past it in one transaction.
    let advance = SequenceAdvance::after(
        &state.head_name,
        state.head_orig,
        &state.current_oid,
        &state.todo,
        &state.opts_json,
    );
    let new_commit =
        create_cherry_pick_commit(&original, &parent, tree_id, &opts_args, output, &advance)
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
    reject_inconsistent_conclusion(&state)?;
    if opts_json_control_phase(&state.opts_json) == Some(ControlPhase::Abort) {
        return Err(CherryPickError::ControlPending(ControlPhase::Abort));
    }

    // HF-31 X3: record the skip before resetting, so an interruption after the
    // reset leaves a row that `--skip` finishes and `--continue` refuses.
    if let Some(opts_json) = opts_json_with_control_phase(&state.opts_json, ControlPhase::Skip) {
        let marked = CherryPickState {
            opts_json,
            ..state.clone()
        };
        marked.save().await.map_err(CherryPickError::SaveFailed)?;
    }

    // Drop the current conflicted commit: restore index+worktree to the last
    // successful tip (current HEAD), discarding the conflict markers/stages.
    reset_hard("HEAD", output).await?;
    after_control_reset_failpoint()?;

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

    // HF-31 X4: record the abort before resetting; re-running `--abort` after an
    // interruption resets to the same commit again and then clears the row.
    if let Some(opts_json) = opts_json_with_control_phase(&state.opts_json, ControlPhase::Abort) {
        let marked = CherryPickState {
            opts_json,
            ..state.clone()
        };
        marked.save().await.map_err(CherryPickError::SaveFailed)?;
    }
    let restored = state.head_orig.to_string();
    reset_hard(&restored, output).await?;
    after_control_reset_failpoint()?;
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
    advance: &SequenceAdvance,
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
        // ADR-HF-04 U11: the fast-forward goes through `reset --hard`, so refuse
        // an untracked overwrite before that reset rewrites anything.
        let target_tree: Tree = load_object(&commit_to_pick.tree_id).map_err(|e| {
            CherryPickSingleError::LoadObject(format!("failed to load fast-forward tree: {e}"))
        })?;
        let mut target_index = Index::new();
        crate::command::reset::rebuild_index_from_tree(&target_tree, &mut target_index, "")
            .map_err(CherryPickSingleError::LoadObject)?;
        let index_file = path::index();
        let current_index = if index_file.exists() {
            Index::load(&index_file).map_err(|e| {
                CherryPickSingleError::LoadObject(format!("failed to load index: {e}"))
            })?
        } else {
            Index::new()
        };
        ensure_no_untracked_overwrite(&current_index, &target_index)?;
        // HF-31 X2: `reset --hard` moves HEAD outside the sequence row's
        // transaction, so first write a row naming this commit with the
        // `ff_landing` marker; an interruption before the advance below leaves
        // HEAD at the marked commit, which `--continue` resumes after.
        if let Some(row) = advance.landing_row(commit_id) {
            sequencer::save(&row)
                .await
                .map_err(CherryPickSingleError::SaveFailed)?;
        }
        reset_hard(&commit_id.to_string(), output)
            .await
            .map_err(|e| CherryPickSingleError::SaveFailed(e.to_string()))?;
        after_head_move_failpoint()?;
        advance
            .apply_without_head_move()
            .await
            .map_err(CherryPickSingleError::SaveFailed)?;
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

    // ADR-MG-01 fail-closed gate, shared with `merge` and `rebase`: a submodule
    // pointer that diverged between the parent, the picked commit and the index
    // stops the pick before anything is written. Pointers all three sides agree
    // on need no action — the pick only rewrites paths it changes, so the
    // existing index entry is carried through untouched.
    merge::ensure_gitlinks_not_arbitrated(
        "cherry-pick",
        &merge::tree_gitlink_entries(&parent_tree),
        &merge::index_gitlink_entries(&current_index),
        &merge::tree_gitlink_entries(&their_tree),
    )
    .map_err(|refusal| CherryPickSingleError::GitlinkUnsupported(refusal.to_string()))?;

    let their_index_modes = tree_index_modes(&their_tree);
    let base_index_modes = tree_index_modes(&parent_tree);
    let changes = diff_trees(&their_tree, &parent_tree);
    let needs_content_driver = changes.iter().any(|(path, their_hash, base_hash)| {
        let ours_hash = ours_items.get(path).copied();
        ours_hash != *base_hash
            && ours_hash != *their_hash
            && (ours_hash.is_some() || their_hash.is_some())
    });
    let default_driver = if needs_content_driver {
        merge::read_merge_default_driver()
            .await
            .map_err(CherryPickSingleError::MergeDriverConfigRead)?
    } else {
        None
    };
    let (preflighted_conflict_style, mut deferred_conflict_style_error) = if needs_content_driver {
        match merge::conflict_style_from_config().await {
            Ok(style) => (Some(style), None),
            Err(error) => (Some(merge::ConflictStyle::Merge), Some(error)),
        }
    } else {
        (None, None)
    };

    let mut conflicts: Vec<ConflictEntry> = Vec::new();
    for (path, their_hash, base_hash) in changes {
        let ours_hash = ours_items.get(&path).cloned();
        if ours_hash == base_hash {
            match their_hash {
                Some(th) => update_index_entry(
                    &mut index,
                    &path,
                    th,
                    their_index_modes.get(&path).copied().unwrap_or(0o100644),
                )?,
                None => {
                    index.remove(path_to_utf8(&path)?, 0);
                }
            }
        } else if ours_hash == their_hash {
            // Both sides already converged on the same content — nothing to do.
        } else if let (Some(ours_hash), Some(their_hash)) = (ours_hash, their_hash) {
            let driver = merge::builtin_merge_driver_for_path(&path, default_driver.as_deref());
            // Preserve the pre-driver add/add behavior for the implicit text
            // fallback (MG-08 G15). An explicitly selected binary/union driver
            // still owns the base-less content merge below.
            if base_hash.is_none() && driver == merge::BuiltinMergeDriver::Text {
                if let Some(favor) = args.strategy_option.last().copied() {
                    apply_favored_pick_resolution(
                        &mut index,
                        &path,
                        base_hash,
                        Some(ours_hash),
                        Some(their_hash),
                        favor,
                        (
                            current_index_mode(&current_index, &path),
                            their_index_modes.get(&path).copied().unwrap_or(0o100644),
                        ),
                    )?;
                } else {
                    index.remove(path_to_utf8(&path)?, 0);
                    add_stage_entry(
                        &mut index,
                        &path,
                        ours_hash,
                        2,
                        current_index_mode(&current_index, &path),
                    )?;
                    add_stage_entry(
                        &mut index,
                        &path,
                        their_hash,
                        3,
                        their_index_modes.get(&path).copied().unwrap_or(0o100644),
                    )?;
                    conflicts.push((path, Some(ours_hash), Some(their_hash), None, driver));
                }
                continue;
            }
            let base_data = match base_hash {
                Some(base_hash) => {
                    let base: Blob = load_object(&base_hash)
                        .map_err(|error| CherryPickSingleError::LoadObject(error.to_string()))?;
                    base.data
                }
                None => Vec::new(),
            };
            let ours: Blob = load_object(&ours_hash)
                .map_err(|error| CherryPickSingleError::LoadObject(error.to_string()))?;
            let theirs: Blob = load_object(&their_hash)
                .map_err(|error| CherryPickSingleError::LoadObject(error.to_string()))?;
            match merge::merge_bytes_with_refined_driver(
                driver,
                &base_data,
                &ours.data,
                &theirs.data,
                args.strategy_option.last().copied(),
                preflighted_conflict_style.unwrap_or(merge::ConflictStyle::Merge),
                0,
            )
            .map_err(CherryPickSingleError::SaveFailed)?
            {
                merge::BuiltinMergeOutcome::Clean(bytes) => {
                    let blob = Blob::from_content_bytes(bytes);
                    save_object(&blob, &blob.id).map_err(|error| {
                        CherryPickSingleError::SaveFailed(format!(
                            "failed to save merged cherry-pick result for '{}': {error}",
                            path.display()
                        ))
                    })?;
                    let mode = merged_entry_mode(
                        current_index_mode(&current_index, &path),
                        base_index_modes.get(&path).copied(),
                        their_index_modes.get(&path).copied(),
                    );
                    update_index_entry(&mut index, &path, blob.id, mode)?;
                }
                merge::BuiltinMergeOutcome::Conflict(_) => {
                    index.remove(path_to_utf8(&path)?, 0);
                    if let Some(base_hash) = base_hash {
                        add_stage_entry(
                            &mut index,
                            &path,
                            base_hash,
                            1,
                            base_index_modes.get(&path).copied().unwrap_or(0o100644),
                        )?;
                    }
                    add_stage_entry(
                        &mut index,
                        &path,
                        ours_hash,
                        2,
                        current_index_mode(&current_index, &path),
                    )?;
                    add_stage_entry(
                        &mut index,
                        &path,
                        their_hash,
                        3,
                        their_index_modes.get(&path).copied().unwrap_or(0o100644),
                    )?;
                    conflicts.push((path, Some(ours_hash), Some(their_hash), base_hash, driver));
                }
            }
        } else if let Some(favor) = args.strategy_option.last().copied() {
            apply_favored_pick_resolution(
                &mut index,
                &path,
                base_hash,
                ours_hash,
                their_hash,
                favor,
                (
                    current_index_mode(&current_index, &path),
                    their_index_modes.get(&path).copied().unwrap_or(0o100644),
                ),
            )?;
        } else {
            let driver = merge::builtin_merge_driver_for_path(&path, default_driver.as_deref());
            index.remove(path_to_utf8(&path)?, 0);
            if let Some(b) = base_hash {
                add_stage_entry(
                    &mut index,
                    &path,
                    b,
                    1,
                    base_index_modes.get(&path).copied().unwrap_or(0o100644),
                )?;
            }
            if let Some(o) = ours_hash {
                add_stage_entry(
                    &mut index,
                    &path,
                    o,
                    2,
                    current_index_mode(&current_index, &path),
                )?;
            }
            if let Some(t) = their_hash {
                add_stage_entry(
                    &mut index,
                    &path,
                    t,
                    3,
                    their_index_modes.get(&path).copied().unwrap_or(0o100644),
                )?;
            }
            conflicts.push((path, ours_hash, their_hash, base_hash, driver));
        }
    }

    if !conflicts.is_empty() {
        // Honor the shared merge/diff3/zdiff3 renderer. A bad setting was
        // deferred while deciding whether a real conflict existed; surface it
        // now, before the conflicted index or worktree is written.
        if let Some(error) = deferred_conflict_style_error.take() {
            return Err(match error {
                super::merge::ConflictStyleError::Invalid(value) => {
                    CherryPickSingleError::InvalidConflictStyle(value)
                }
                super::merge::ConflictStyleError::Read(detail) => {
                    CherryPickSingleError::ConflictStyleRead(detail)
                }
            });
        }
        let conflict_style =
            match preflighted_conflict_style {
                Some(style) => style,
                None => super::merge::conflict_style_from_config()
                    .await
                    .map_err(|error| match error {
                        super::merge::ConflictStyleError::Invalid(value) => {
                            CherryPickSingleError::InvalidConflictStyle(value)
                        }
                        super::merge::ConflictStyleError::Read(detail) => {
                            CherryPickSingleError::ConflictStyleRead(detail)
                        }
                    })?,
            };
        ensure_no_untracked_overwrite(&current_index, &index)?;
        index
            .save(&index_file)
            .map_err(|e| CherryPickSingleError::SaveFailed(format!("failed to save index: {e}")))?;
        // Sync the cleanly-applied stage-0 paths, then overlay conflict markers
        // onto each divergent path so the user can resolve them in the worktree.
        reset_workdir_tracked_only(&current_index, &index)?;
        let labels =
            super::merge::GitConflictLabels::for_cherry_pick(commit_id, &commit_to_pick.message);
        for (path, ours_hash, their_hash, base_hash, driver) in &conflicts {
            write_conflict_markers_file(
                path,
                ours_hash,
                their_hash,
                base_hash,
                &labels,
                conflict_style,
                *driver,
            )?;
        }
        // rerere: record the preimage of each just-written conflict and replay a
        // previously recorded resolution if one matches. A no-op unless
        // `rerere.enabled` is set, so default cherry-pick behaviour is unchanged.
        if let Err(error) =
            crate::command::rerere::auto_update(rerere_autoupdate_override(args)).await
        {
            tracing::warn!("rerere auto-update after cherry-pick conflict failed: {error}");
        }
        let mut paths: Vec<String> = conflicts
            .iter()
            .map(|(path, _, _, _, _)| path.display().to_string())
            .collect();
        paths.sort();
        return Err(CherryPickSingleError::Conflicted(paths));
    }

    // Build the candidate tree first (saves tree objects, but does NOT touch the
    // on-disk index or worktree yet) so the "redundant after replay" check can
    // bail out before mutating any state.
    let tree_id = create_tree_from_index(&index)?;

    if args.no_commit {
        ensure_no_untracked_overwrite(&current_index, &index)?;
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

    ensure_no_untracked_overwrite(&current_index, &index)?;
    index
        .save(&index_file)
        .map_err(|e| CherryPickSingleError::SaveFailed(format!("failed to save index: {e}")))?;
    reset_workdir_tracked_only(&current_index, &index)?;

    let cherry_pick_commit_id = create_cherry_pick_commit(
        &commit_to_pick,
        &current_head,
        tree_id,
        args,
        output,
        advance,
    )
    .await?;
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

    let (body, _) = parse_commit_msg(&original_commit.message);
    let body = body.trim();

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
    // Part C §C.4.3: the editor buffer is transient per-worktree scratch, so it
    // lives in THIS worktree's gitdir. On shared storage two worktrees editing a
    // message concurrently would truncate each other's buffer. Unchanged for the
    // main worktree, where local and common storage are the same directory.
    let path = util::request_worktree_gitdir()
        .map(|gitdir| gitdir.join("CHERRY_PICK_MSG"))
        .map_err(|error| CherryPickSingleError::SaveFailed(error.to_string()))?;
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
    advance: &SequenceAdvance,
) -> Result<ObjectHash, CherryPickSingleError> {
    let message = build_cherry_pick_message(original_commit, args, output).await?;

    if message.trim().is_empty() && !args.allow_empty_message {
        return Err(CherryPickSingleError::EmptyMessage(
            original_commit.id.to_string(),
        ));
    }

    let parents = vec![*parent_id];
    let author = original_commit.author.clone();
    let (committer, _identity) = create_committer_signature()
        .await
        .map_err(|e| CherryPickSingleError::SaveFailed(e.to_string()))?;
    let commit = if args.gpg_sign {
        // Sign via the libra vault (force=true so it signs regardless of the
        // `vault.signing` default).
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
        Commit::new(
            author,
            committer,
            tree_id,
            parents,
            &format_commit_msg(&message, None),
        )
    };

    save_object(&commit, &commit.id)
        .map_err(|e| CherryPickSingleError::SaveFailed(format!("failed to save commit: {e}")))?;
    record_current_repo_commit_revision_for_active_operation(
        commit.id.to_string(),
        Some((original_commit.id.to_string(), RelationKind::CherryPick)),
    )
    .await
    .map_err(|error| CherryPickSingleError::SaveFailed(error.to_string()))?;

    let action = ReflogAction::CherryPick {
        source_message: original_commit.message.clone(),
    };
    let context = ReflogContext {
        old_oid: parent_id.to_string(),
        new_oid: commit.id.to_string(),
        action,
    };

    let advance = advance.clone();
    with_reflog(
        context,
        move |txn| {
            Box::pin(async move {
                update_head(txn, &commit.id.to_string()).await?;
                match &advance {
                    SequenceAdvance::NoRow => {}
                    SequenceAdvance::Save(next) => sequencer::save_with_conn(txn, next).await?,
                    SequenceAdvance::Clear(_) => {
                        sequencer::clear_with_conn(txn, SequenceKind::CherryPick).await?
                    }
                }
                Ok(())
            })
        },
        true,
    )
    .await
    .map_err(|e| {
        CherryPickSingleError::SaveFailed(format!("failed to update branch and reflog: {e}"))
    })?;
    after_head_move_failpoint()?;
    Ok(commit.id)
}

/// Paths whose blob content differs between the picked commit and its parent.
///
/// Gitlinks are excluded on purpose: submodule content is never merged
/// (ADR-MG-01), and a diverged one is already refused by
/// [`merge::ensure_gitlinks_not_arbitrated`] before this runs, so anything left
/// here is a pointer all three sides agree on and needs no index change.
fn diff_trees(
    theirs: &Tree,
    base: &Tree,
) -> Vec<(PathBuf, Option<ObjectHash>, Option<ObjectHash>)> {
    let mut diffs = Vec::new();
    let their_items = mergeable_tree_items(theirs);
    let base_items = mergeable_tree_items(base);

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

/// Flatten a tree to its non-gitlink entries. Unlike `TreeExt::get_plain_items`
/// this drops submodule pointers silently rather than warning on stderr: the
/// ADR-MG-01 guard has already decided whether they are acceptable.
fn mergeable_tree_items(tree: &Tree) -> HashMap<PathBuf, ObjectHash> {
    tree.get_plain_items_with_mode()
        .into_iter()
        .filter(|(_, _, mode)| *mode != TreeItemMode::Commit)
        .map(|(path, hash, _)| (path, hash))
        .collect()
}

/// Index-mode map for a tree's non-gitlink entries.
fn tree_index_modes(tree: &Tree) -> HashMap<PathBuf, u32> {
    tree.get_plain_items_with_mode()
        .into_iter()
        .filter(|(_, _, mode)| *mode != TreeItemMode::Commit)
        .map(|(path, _, mode)| (path, tree_mode_to_index_mode(mode)))
        .collect()
}

fn tree_mode_to_index_mode(mode: TreeItemMode) -> u32 {
    match mode {
        TreeItemMode::Blob => 0o100644,
        TreeItemMode::BlobExecutable => 0o100755,
        TreeItemMode::Link => 0o120000,
        TreeItemMode::Commit => 0o160000,
        TreeItemMode::Tree => 0o040000,
    }
}

/// Stage-0 mode currently recorded in the index, defaulting to a plain file.
fn current_index_mode(index: &Index, path: &Path) -> u32 {
    path.to_str()
        .and_then(|name| index.get(name, 0))
        .map(|entry| entry.mode)
        .unwrap_or(0o100644)
}

/// Git's `merge_mode` behaviour: a side that matches the base yields to the
/// other side's mode; otherwise ours wins.
fn merged_entry_mode(ours: u32, base: Option<u32>, theirs: Option<u32>) -> u32 {
    match (base, theirs) {
        (Some(base), Some(theirs)) if ours == base => theirs,
        (Some(base), Some(theirs)) if theirs == base => ours,
        _ => ours,
    }
}

/// Resolve a divergent cherry-pick path using the same hunk-level side
/// preference as merge. A true three-sided content conflict preserves clean
/// ranges; add/add and modify/delete conflicts select the requested whole side.
fn apply_favored_pick_resolution(
    index: &mut Index,
    path: &Path,
    base_hash: Option<ObjectHash>,
    ours_hash: Option<ObjectHash>,
    theirs_hash: Option<ObjectHash>,
    favor: MergeFavor,
    modes: (u32, u32),
) -> Result<(), CherryPickSingleError> {
    let selected_hash = match (base_hash, ours_hash, theirs_hash) {
        (Some(base_hash), Some(ours_hash), Some(theirs_hash)) => {
            let base: Blob = load_object(&base_hash)
                .map_err(|error| CherryPickSingleError::LoadObject(error.to_string()))?;
            let ours: Blob = load_object(&ours_hash)
                .map_err(|error| CherryPickSingleError::LoadObject(error.to_string()))?;
            let theirs: Blob = load_object(&theirs_hash)
                .map_err(|error| CherryPickSingleError::LoadObject(error.to_string()))?;
            let merged = merge::merge_bytes_with_favor(&base.data, &ours.data, &theirs.data, favor)
                .map_err(CherryPickSingleError::SaveFailed)?;
            let blob = Blob::from_content_bytes(merged);
            save_object(&blob, &blob.id).map_err(|error| {
                CherryPickSingleError::SaveFailed(format!(
                    "failed to save favored merge result for '{}': {error}",
                    path.display()
                ))
            })?;
            Some(blob.id)
        }
        _ => match favor {
            MergeFavor::Ours => ours_hash,
            MergeFavor::Theirs => theirs_hash,
        },
    };

    let selected_mode = match favor {
        MergeFavor::Ours => modes.0,
        MergeFavor::Theirs => modes.1,
    };
    let path_str = path_to_utf8(path)?;
    index.remove(path_str, 0);
    if let Some(hash) = selected_hash {
        update_index_entry(index, path, hash, selected_mode)?;
    }
    Ok(())
}

fn update_index_entry(
    index: &mut Index,
    path: &Path,
    hash: ObjectHash,
    mode: u32,
) -> Result<(), CherryPickSingleError> {
    let blob = git_internal::internal::object::blob::Blob::load(&hash);
    let mut entry = IndexEntry::new_from_blob(
        path_to_utf8(path)?.to_string(),
        hash,
        blob.data.len() as u32,
    );
    entry.mode = mode;
    index.add(entry);
    Ok(())
}

/// Add a conflict-stage (1=base / 2=ours / 3=theirs) index entry for `path`.
fn add_stage_entry(
    index: &mut Index,
    path: &Path,
    hash: ObjectHash,
    stage: u8,
    mode: u32,
) -> Result<(), CherryPickSingleError> {
    let blob = git_internal::internal::object::blob::Blob::load(&hash);
    let mut entry = IndexEntry::new_from_blob(
        path_to_utf8(path)?.to_string(),
        hash,
        blob.data.len() as u32,
    );
    entry.mode = mode;
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
    labels: &merge::GitConflictLabels,
    conflict_style: merge::ConflictStyle,
    driver: merge::BuiltinMergeDriver,
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
    let binary_conflict = driver == merge::BuiltinMergeDriver::Binary
        || (driver == merge::BuiltinMergeDriver::Union
            && ours_bytes.is_some()
            && theirs_bytes.is_some()
            && [
                base_bytes.as_deref(),
                ours_bytes.as_deref(),
                theirs_bytes.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(merge::merge_input_is_binary));
    let content: Vec<u8> = if binary_conflict {
        // A binary modify/delete conflict keeps whichever complete side still
        // exists. Choosing only ours would turn an ours-deleted/theirs-modified
        // path into an empty worktree file even though stage 3 retained data.
        ours_bytes
            .clone()
            .or_else(|| theirs_bytes.clone())
            .unwrap_or_default()
    } else {
        match (&ours_bytes, &theirs_bytes) {
            (Some(ours), Some(theirs)) => super::merge::render_line_level_conflict_labeled(
                base_bytes.as_deref(),
                ours,
                theirs,
                labels,
                conflict_style,
            )
            .map_err(CherryPickSingleError::SaveFailed)?
            .unwrap_or_else(|| whole_file_conflict(ours, theirs, &labels.theirs)),
            _ => whole_file_conflict(
                ours_bytes.as_deref().unwrap_or(&[]),
                theirs_bytes.as_deref().unwrap_or(&[]),
                &labels.theirs,
            ),
        }
    };

    let target = util::working_dir().join(path);
    let executable = index_entry_executable(path);
    crate::utils::worktree_blob::write_worktree_blob(&target, &content, executable).map_err(
        |e| {
            CherryPickSingleError::SaveFailed(format!(
                "failed to write conflict markers to '{}': {e}",
                target.display()
            ))
        },
    )?;
    Ok(())
}

/// Executable bit of the index entry for `path` (stage 2, else 3/0), used by
/// conflict-marker materialization (ADR-FM-02/03).
fn index_entry_executable(path: &Path) -> bool {
    let Some(name) = path.to_str() else {
        return false;
    };
    Index::load(crate::utils::path::index())
        .ok()
        .and_then(|index| {
            [2u8, 3, 0]
                .iter()
                .find_map(|stage| index.get(name, *stage))
                .map(|entry| entry.mode & 0o111 != 0)
        })
        .unwrap_or(false)
}

/// Whole-file conflict presentation, used when a line-level merge does not apply
/// (a delete/modify conflict, or binary content): ours between `<<<<<<< HEAD`
/// and `=======`, theirs up to `>>>>>>> <short-source>`.
fn whole_file_conflict(ours: &[u8], theirs: &[u8], short_src: &str) -> Vec<u8> {
    // Preserve cherry-pick's established lossy binary/delete-modify fallback;
    // the shared renderer only owns marker framing and line endings here.
    let ours = String::from_utf8_lossy(ours);
    let theirs = String::from_utf8_lossy(theirs);
    merge::render_whole_file_conflict(ours.as_bytes(), theirs.as_bytes(), "HEAD", short_src)
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

/// Refuse a pick that would overwrite an untracked working tree file. Callers
/// run this before saving the new index, so the refusal writes nothing
/// (ADR-HF-04 U8-U10; Git: "The following untracked working tree files would be
/// overwritten by merge").
fn ensure_no_untracked_overwrite(
    current_index: &Index,
    new_index: &Index,
) -> Result<(), CherryPickSingleError> {
    let untracked_paths = worktree::untracked_workdir_paths(current_index).map_err(|e| {
        CherryPickSingleError::LoadObject(format!("failed to inspect untracked files: {e}"))
    })?;
    match worktree::untracked_overwrite_path(&untracked_paths, new_index) {
        Some(path) => Err(CherryPickSingleError::UntrackedOverwrite(
            path.display().to_string(),
        )),
        None => Ok(()),
    }
}

/// Sync the worktree to `new_index`. Callers have already run
/// [`ensure_no_untracked_overwrite`] before saving `new_index`.
fn reset_workdir_tracked_only(
    current_index: &Index,
    new_index: &Index,
) -> Result<(), CherryPickSingleError> {
    let workdir = util::working_dir();
    let new_tracked_paths: HashSet<_> = new_index.tracked_files().into_iter().collect();

    for path_buf in current_index.tracked_files() {
        if !new_tracked_paths.contains(&path_buf) {
            // A submodule directory is not Libra's to unlink, and a gitlink can
            // only leave the index through a decision the ADR-MG-01 guard has
            // already refused.
            if is_gitlink_entry(current_index, path_to_utf8(&path_buf)?) {
                continue;
            }
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
            // A gitlink names a SUBMODULE's commit, which is not an object of
            // this repository — loading it as a blob would panic — and Libra
            // materializes no submodule working tree. The pointer stays an
            // index/tree fact only (ADR-MG-01).
            if is_submodule_index_mode(entry.mode) {
                continue;
            }
            let blob = git_internal::internal::object::blob::Blob::load(&entry.hash);
            let target_path = workdir.join(path_str);
            if entry.mode & 0o170000 == 0o120000 {
                crate::utils::worktree_blob::write_worktree_symlink(&target_path, &blob.data)
                    .map_err(|e| {
                        CherryPickSingleError::SaveFailed(format!(
                            "failed to write symlink '{}': {e}",
                            target_path.display()
                        ))
                    })?;
            } else {
                crate::utils::worktree_blob::write_worktree_blob(
                    &target_path,
                    &blob.data,
                    entry.mode & 0o111 != 0,
                )
                .map_err(|e| {
                    CherryPickSingleError::SaveFailed(format!(
                        "failed to write file '{}': {e}",
                        target_path.display()
                    ))
                })?;
            }
        }
    }
    Ok(())
}

/// Whether an index stat mode records a `160000` gitlink (submodule) entry.
fn is_submodule_index_mode(mode: u32) -> bool {
    mode & 0o170000 == 0o160000
}

/// Whether `path` is recorded at stage 0 of `index` as a gitlink.
fn is_gitlink_entry(index: &Index, path: &str) -> bool {
    index
        .get(path, 0)
        .is_some_and(|entry| is_submodule_index_mode(entry.mode))
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

/// Test-only interruption right after a pick moved HEAD, before any later
/// sequencer write (gated on the `LIBRA_TEST` sentinel like every failpoint).
fn after_head_move_failpoint() -> Result<(), CherryPickSingleError> {
    if std::env::var_os("LIBRA_TEST").is_some()
        && std::env::var_os("LIBRA_TEST_CHERRY_PICK_FAIL_AFTER_HEAD").is_some()
    {
        return Err(CherryPickSingleError::SaveFailed(
            "test-injected cherry-pick interruption after moving HEAD".to_string(),
        ));
    }
    Ok(())
}

/// Sequencer row write committed in the same transaction as a pick's HEAD
/// move, so an interruption between the two can never leave `current_oid`
/// naming a commit that already landed (which `--continue` would replay).
#[derive(Debug, Clone)]
enum SequenceAdvance {
    /// No sequence row is involved (a single-commit or `--no-commit` pick).
    NoRow,
    /// Point the existing row at the next commit to attempt.
    Save(SequenceState),
    /// The landed commit was the last one: remove the row. Carries the row that
    /// names the landing commit, written first by a fast-forward pick.
    Clear(SequenceState),
}

impl SequenceAdvance {
    /// The row once `landing` (the commit ahead of `todo`) lands: the next
    /// commit with the conflict flag cleared (a later `--continue` re-attempts
    /// it), or no row.
    fn after(
        head_name: &str,
        head_orig: ObjectHash,
        landing: &ObjectHash,
        todo: &VecDeque<ObjectHash>,
        opts_json: &str,
    ) -> Self {
        let row = |current_oid: ObjectHash, todo: VecDeque<ObjectHash>| {
            CherryPickState {
                head_name: head_name.to_string(),
                head_orig,
                current_oid,
                stop_concluded: false,
                todo,
                opts_json: opts_json_with_conflict_flag(opts_json, false),
            }
            .to_sequence()
        };
        let mut rest = todo.clone();
        match rest.pop_front() {
            Some(next) => SequenceAdvance::Save(row(next, rest)),
            None => SequenceAdvance::Clear(row(*landing, VecDeque::new())),
        }
    }

    /// The row naming `landing` itself, written before a fast-forward pick
    /// moves HEAD outside the transaction (HF-31 X2).
    fn landing_row(&self, landing: &ObjectHash) -> Option<SequenceState> {
        let mut row = match self {
            SequenceAdvance::NoRow => return None,
            SequenceAdvance::Save(next) => {
                let mut row = next.clone();
                row.todo.insert(0, row.current_oid.clone());
                row.current_oid = landing.to_string();
                row
            }
            SequenceAdvance::Clear(landed) => landed.clone(),
        };
        row.payload = opts_json_with_ff_landing(&row.payload, landing);
        Some(row)
    }

    /// Apply the transition when no HEAD transaction carries it: after a
    /// fast-forward's `reset --hard`, or for a pick dropped by `--empty=drop`
    /// (HF-31 X6).
    async fn apply_without_head_move(&self) -> Result<(), String> {
        match self {
            SequenceAdvance::NoRow => Ok(()),
            SequenceAdvance::Save(next) => sequencer::save(next).await,
            SequenceAdvance::Clear(_) => sequencer::clear(SequenceKind::CherryPick).await,
        }
    }
}

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
    /// #477 HF-01: the stopped item was concluded from outside (a later
    /// `reset`), recorded as `stop_concluded` in the row's serialized options.
    /// The remaining `todo` is kept; `current_oid` still names the stopped
    /// commit, which `--continue` must no longer record.
    /// Derived from `opts_json` when loading; `save()` persists `opts_json`,
    /// not changes made directly to this derived field.
    pub stop_concluded: bool,
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
        // #477 HF-01: the external-conclusion marker rides in the options, so
        // the row itself stays valid for binaries that predate it.
        let stop_concluded = serde_json::from_str::<CherryPickOpts>(&state.payload)
            .map(|opts| opts.stop_concluded)
            .unwrap_or(false);
        let todo = VecDeque::from(Self::parse_todo(&state.todo.join("\n"))?);
        Ok(CherryPickState {
            head_name: state.head_name,
            head_orig,
            current_oid,
            stop_concluded,
            todo,
            opts_json: state.payload,
        })
    }

    /// Persist via the unified sequencer (atomic DELETE+INSERT txn).
    pub async fn save(&self) -> Result<(), String> {
        sequencer::save(&self.to_sequence()).await
    }

    /// Persist the FIRST write of a starting sequence as an atomic claim, so
    /// two starts racing in one worktree cannot both proceed (§C.4.4).
    pub async fn claim_start(&self) -> Result<(), String> {
        sequencer::claim_start(&self.to_sequence()).await
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
        // ADR-MG-01: the refusal forwards the shared guard's wording verbatim
        // so `merge`, `rebase` and `cherry-pick` read identically.
        assert_eq!(
            CherryPickError::GitlinkUnsupported(
                "cherry-pick would have to merge the submodule (gitlink) entry 'vendor': Libra does not support submodules"
                    .to_string(),
            )
            .to_string(),
            "cherry-pick would have to merge the submodule (gitlink) entry 'vendor': Libra does not support submodules",
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
            CherryPickError::UntrackedOverwrite {
                commit: "abc123".to_string(),
                path: "new.txt".to_string(),
                stop: UntrackedStop::BeforeAnyWrite,
            }
            .to_string(),
            "failed to cherry-pick abc123: untracked working tree file would be overwritten: new.txt",
        );
        assert_eq!(
            CherryPickError::UnmergedIndex(vec!["a.txt".to_string()]).to_string(),
            "cherry-pick is not possible because the index has unmerged entries",
        );
        assert_eq!(
            CherryPickError::NoCommitConflict {
                commit: "abc123".to_string(),
                paths: 2,
            }
            .to_string(),
            "failed to cherry-pick abc123: conflicts in 2 path(s); a '--no-commit' pick leaves no sequence to continue",
        );
        assert_eq!(
            CherryPickError::InProgress.to_string(),
            "a cherry-pick is already in progress",
        );
        assert_eq!(
            CherryPickError::CorruptState("bad".to_string()).to_string(),
            "cherry-pick state is inconsistent: bad",
        );
        assert_eq!(
            CherryPickError::LocalChangesWouldBeOverwritten.to_string(),
            "your local changes would be overwritten by cherry-pick",
        );
        assert_eq!(
            CherryPickError::ControlPending(ControlPhase::Skip).to_string(),
            "an interrupted 'libra cherry-pick --skip' has not finished",
        );
        assert_eq!(
            CherryPickError::ControlPending(ControlPhase::Abort).to_string(),
            "an interrupted 'libra cherry-pick --abort' has not finished",
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
            CherryPickError::GitlinkUnsupported("vendor".to_string()).stable_code(),
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
            CherryPickError::UntrackedOverwrite {
                commit: "abc123".to_string(),
                path: "new.txt".to_string(),
                stop: UntrackedStop::BeforeAnyWrite,
            }
            .stable_code(),
            StableErrorCode::ConflictUnresolved,
        );
        assert_eq!(
            CherryPickError::UnmergedIndex(vec!["a.txt".to_string()]).stable_code(),
            StableErrorCode::ConflictUnresolved,
        );
        assert_eq!(
            CherryPickError::NoCommitConflict {
                commit: "abc123".to_string(),
                paths: 1,
            }
            .stable_code(),
            StableErrorCode::ConflictUnresolved,
        );
        assert_eq!(
            CherryPickError::InProgress.stable_code(),
            StableErrorCode::ConflictOperationBlocked,
        );
        assert_eq!(
            CherryPickError::CorruptState("bad".to_string()).stable_code(),
            StableErrorCode::RepoCorrupt,
        );
        assert_eq!(
            CherryPickError::LocalChangesWouldBeOverwritten.stable_code(),
            StableErrorCode::ConflictUnresolved,
        );
        assert_eq!(
            CherryPickError::ControlPending(ControlPhase::Skip).stable_code(),
            StableErrorCode::RepoStateInvalid,
        );
        assert_eq!(
            CherryPickError::ControlPending(ControlPhase::Abort).stable_code(),
            StableErrorCode::RepoStateInvalid,
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

    /// ADR-HF-04: the refusal names at most ten unmerged paths and counts the rest.
    #[test]
    fn unmerged_paths_hint_lists_at_most_ten_paths() {
        let paths: Vec<String> = (1..=12).map(|i| format!("p{i}.txt")).collect();
        assert_eq!(
            unmerged_paths_hint(&paths[..2]),
            "unmerged paths: p1.txt, p2.txt"
        );
        let hint = unmerged_paths_hint(&paths);
        assert!(
            hint.contains("p10.txt") && !hint.contains("p11.txt"),
            "{hint}"
        );
        assert!(hint.ends_with("(and 2 more)"), "{hint}");
    }

    /// ADR-HF-04: the unmerged-index refusal exits 128 with `LBR-CONFLICT-001`
    /// and lists the paths in a hint.
    #[test]
    fn unmerged_index_refusal_is_fatal_conflict_with_paths() {
        let error = CliError::from(CherryPickError::UnmergedIndex(vec!["a.txt".to_string()]));
        assert_eq!(error.exit_code(), 128);
        let rendered = error.render_json();
        assert!(rendered.contains("LBR-CONFLICT-001"), "{rendered}");
        assert!(rendered.contains("unmerged paths: a.txt"), "{rendered}");
    }

    /// M-UNMERGED U8-U10: the untracked-overwrite refusal says nothing was
    /// written and points at moving the file, never at `--continue`.
    #[test]
    fn untracked_overwrite_refusal_guides_to_move_the_file() {
        let error = CliError::from(CherryPickError::UntrackedOverwrite {
            commit: "abc123".to_string(),
            path: "new.txt".to_string(),
            stop: UntrackedStop::BeforeAnyWrite,
        });
        assert_eq!(error.exit_code(), 128);
        let rendered = error.render_json();
        assert!(rendered.contains("move or remove 'new.txt'"), "{rendered}");
        assert!(rendered.contains("nothing was written"), "{rendered}");
        assert!(!rendered.contains("--continue"), "{rendered}");
    }

    /// ADR-HF-04 U12-U14: an untracked stop inside a sequence points at
    /// `--continue`, and a partial `--no-commit` run says earlier picks stay staged.
    #[test]
    fn untracked_overwrite_hint_follows_the_stop() {
        let hint = |stop| {
            CliError::from(CherryPickError::UntrackedOverwrite {
                commit: "abc123".to_string(),
                path: "new.txt".to_string(),
                stop,
            })
            .render_json()
        };
        let sequence = hint(UntrackedStop::SequenceStopped);
        assert!(
            sequence.contains("libra cherry-pick --continue"),
            "{sequence}"
        );
        assert!(
            !sequence.contains("run the same command again"),
            "{sequence}"
        );
        let partial = hint(UntrackedStop::NoCommitPartial);
        assert!(partial.contains("stay staged"), "{partial}");
        assert!(!partial.contains("--continue"), "{partial}");
    }

    /// `--continue` re-attempts a non-conflict stop only when the persisted row
    /// carries the conflict flag; rows from older binaries keep finalizing.
    #[test]
    fn opts_json_conflict_flag_presence_distinguishes_legacy_rows() {
        let args = CherryPickArgs::try_parse_from(["cherry-pick", "abc"]).unwrap();
        let current = serde_json::to_string(&CherryPickOpts::from_args(&args)).unwrap();
        assert!(opts_json_has_conflict_flag(&current));
        assert!(!opts_json_has_conflict_flag(r#"{"signoff":false}"#));
        assert!(!opts_json_has_conflict_flag("not json"));
    }

    /// The row written with a landed pick names the next commit with the
    /// conflict flag cleared, or clears the sequence after the last commit; a
    /// fast-forward first writes the row naming the landing commit itself with
    /// the `ff_landing` marker (#477 HF-31).
    #[test]
    fn sequence_advance_points_at_next_commit_or_clears() {
        let oid = |c: char| ObjectHash::from_str(&c.to_string().repeat(40)).unwrap();
        let args = CherryPickArgs::try_parse_from(["cherry-pick", "abc"]).unwrap();
        let mut opts = CherryPickOpts::from_args(&args);
        opts.stopped_on_conflict = true;
        opts.control_phase = Some(ControlPhase::Skip);
        let opts_json = serde_json::to_string(&opts).unwrap();
        let todo = VecDeque::from([oid('b'), oid('c')]);
        let advance = SequenceAdvance::after("main", oid('a'), &oid('d'), &todo, &opts_json);
        match &advance {
            SequenceAdvance::Save(row) => {
                assert_eq!(row.current_oid, oid('b').to_string());
                assert_eq!(row.todo, vec![oid('c').to_string()]);
                let saved: CherryPickOpts = serde_json::from_str(&row.payload).unwrap();
                assert!(!saved.stopped_on_conflict);
                assert_eq!(saved.control_phase, None);
                assert!(opts_json_has_conflict_flag(&row.payload));
            }
            other => panic!("expected Save, got {other:?}"),
        }
        let landing = advance
            .landing_row(&oid('d'))
            .expect("a row names the landing commit");
        assert_eq!(landing.current_oid, oid('d').to_string());
        assert_eq!(
            landing.todo,
            vec![oid('b').to_string(), oid('c').to_string()]
        );
        let marked: CherryPickOpts = serde_json::from_str(&landing.payload).unwrap();
        assert_eq!(marked.ff_landing, Some(oid('d').to_string()));
        let cleared: CherryPickOpts =
            serde_json::from_str(&opts_json_with_conflict_flag(&landing.payload, false)).unwrap();
        assert_eq!(cleared.ff_landing, None);

        let last =
            SequenceAdvance::after("main", oid('a'), &oid('d'), &VecDeque::new(), &opts_json);
        match &last {
            SequenceAdvance::Clear(row) => {
                assert_eq!(row.current_oid, oid('d').to_string());
                assert!(row.todo.is_empty());
            }
            other => panic!("expected Clear, got {other:?}"),
        }
        assert!(SequenceAdvance::NoRow.landing_row(&oid('d')).is_none());
    }

    /// M-CRASH X5 (#477 HF-31): rows without a control phase (older binaries)
    /// report none, a marked row reports its verb, and a position write clears it.
    #[test]
    #[serial_test::serial(env)]
    fn control_phase_round_trips_and_legacy_rows_have_none() {
        let args = CherryPickArgs::try_parse_from(["cherry-pick", "abc"]).unwrap();
        let current = serde_json::to_string(&CherryPickOpts::from_args(&args)).unwrap();
        assert!(!current.contains("control_phase"), "{current}");
        assert_eq!(opts_json_control_phase(&current), None);
        assert_eq!(
            opts_json_control_phase(r#"{"stopped_on_conflict":true,"signoff":false}"#),
            None
        );
        let skipping = opts_json_with_control_phase(&current, ControlPhase::Skip).unwrap();
        assert_eq!(opts_json_control_phase(&skipping), Some(ControlPhase::Skip));
        assert_eq!(
            opts_json_control_phase(&opts_json_with_conflict_flag(&skipping, false)),
            None
        );
        assert!(opts_json_with_control_phase("not json", ControlPhase::Abort).is_none());
        let ff_args = CherryPickArgs::try_parse_from(["cherry-pick", "--ff", "abc"]).unwrap();
        let ff_opts = CherryPickOpts::from_args(&ff_args);
        assert!(ff_opts.ff, "--ff is persisted with the sequence");
        assert!(ff_opts.into_args().ff, "--ff is restored for resumed picks");
        let (stamped, needle) = opts_json_with_claim_token(&current);
        let needle = needle.expect("readable options get a claim token");
        assert!(stamped.contains(&needle), "{stamped}");
        assert!(
            opts_json_with_conflict_flag(&stamped, true).contains(&needle),
            "position writes keep the claim token"
        );
        assert_eq!(opts_json_with_claim_token("not json").1, None);
        let legacy: CherryPickOpts = serde_json::from_str(r#"{"signoff":false}"#).unwrap();
        assert!(!legacy.ff && legacy.ff_landing.is_none());
        assert_eq!(opts_json_control_phase("not json"), None);
    }

    /// HF-31: the refusal names the control verb to re-run.
    #[test]
    fn control_pending_names_the_verb_to_rerun() {
        for (phase, verb) in [
            (ControlPhase::Skip, "--skip"),
            (ControlPhase::Abort, "--abort"),
        ] {
            let rendered = CliError::from(CherryPickError::ControlPending(phase)).render_json();
            assert!(
                rendered.contains(&format!("libra cherry-pick {verb}")),
                "{rendered}"
            );
            assert!(rendered.contains("LBR-REPO-003"), "{rendered}");
        }
    }

    /// M-UNMERGED U5: a `--no-commit` conflict mentions neither a multi-commit
    /// sequence nor `--continue`; it points at `libra add` and `reset --hard`.
    #[test]
    fn no_commit_conflict_guides_to_add_instead_of_continue() {
        let error = CliError::from(CherryPickError::NoCommitConflict {
            commit: "abc123".to_string(),
            paths: 1,
        });
        let rendered = error.render_json();
        assert!(!rendered.contains("multi-commit"), "{rendered}");
        assert!(!rendered.contains("--continue"), "{rendered}");
        assert!(rendered.contains("libra add"), "{rendered}");
        assert!(rendered.contains("libra reset --hard"), "{rendered}");
        assert!(rendered.contains("LBR-CONFLICT-001"), "{rendered}");
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
            rerere_autoupdate: true,
            mainline: Some(2),
            cleanup: Some("strip".to_string()),
            empty: Some("drop".to_string()),
            strategy_option: vec![MergeFavor::Theirs, MergeFavor::Ours],
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
        assert!(rebuilt.rerere_autoupdate);
        assert!(!rebuilt.no_rerere_autoupdate);
        assert_eq!(rebuilt.mainline, Some(2));
        assert_eq!(rebuilt.cleanup.as_deref(), Some("strip"));
        assert_eq!(rebuilt.empty.as_deref(), Some("drop"));
        assert_eq!(rebuilt.strategy_option, vec![MergeFavor::Ours]);

        let old: CherryPickOpts = serde_json::from_str("{}")
            .expect("options written before rerere override remain readable");
        assert_eq!(old.rerere_autoupdate, None);
    }

    #[test]
    fn short_gpg_sign_flag_parses_as_enabled() {
        let args = CherryPickArgs::try_parse_from(["cherry-pick", "-S", "deadbeef"])
            .expect("valid cherry-pick arguments should parse");
        assert!(args.gpg_sign);
        assert!(!args.no_gpg_sign);
    }

    #[test]
    fn rerere_autoupdate_flags_are_last_wins_and_round_trip() {
        let args = CherryPickArgs::try_parse_from([
            "cherry-pick",
            "--rerere-autoupdate",
            "--no-rerere-autoupdate",
            "deadbeef",
        ])
        .expect("the last rerere toggle must win");
        assert_eq!(rerere_autoupdate_override(&args), Some(false));

        let rebuilt = CherryPickOpts::from_args(&args).into_args();
        assert!(!rebuilt.rerere_autoupdate);
        assert!(rebuilt.no_rerere_autoupdate);
    }
}
