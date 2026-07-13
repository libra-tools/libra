//! Merge command orchestration that resolves base/target commits, performs recursive merge, stages results, and updates refs or surfaces conflicts.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use clap::Parser;
use git_internal::{
    hash::{ObjectHash, get_hash_kind},
    internal::{
        index::{Index, IndexEntry},
        object::{
            blob::Blob,
            commit::Commit,
            tree::{Tree, TreeItemMode},
        },
    },
};
use serde::{Deserialize, Serialize};

use super::{
    get_target_commit, load_object, reset,
    restore::{self, RestoreArgs},
    save_object, status, switch,
};
use crate::{
    common_utils::format_commit_msg,
    info_println,
    internal::{
        branch::{Branch, BranchStoreError},
        config::ConfigKv,
        db::get_db_conn_instance,
        head::Head,
        merge_base,
        reflog::{ReflogAction, ReflogContext, with_reflog},
        tree_plumbing,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        object_ext::TreeExt,
        output::{OutputConfig, emit_json_data},
        path, util, worktree,
    },
};

/// `--help` examples shown in `libra merge --help` output.
///
pub const MERGE_EXAMPLES: &str = "\
EXAMPLES:
    libra merge feature-x          Fast-forward current branch onto feature-x if possible
    libra merge origin/main        Fast-forward onto a remote-tracking branch
    libra merge feature-x --no-edit  Accept the default merge message (no editor)
    libra merge --verify-signatures feature-x  Require a valid PGP signature on the merged tip
    libra merge --continue         Finish an in-progress merge after resolving conflicts
    libra merge --abort            Restore the pre-merge HEAD, index, and worktree
    libra merge --dry-run feature-x  Preview the outcome (ff/clean/conflict) writing nothing
    libra merge --restart          Abort the conflicted merge and re-run it fresh
    libra merge --json feature-x   Structured JSON output for agents

NOTES:
    Divergent single-head merges create a merge commit when paths do not
    conflict. Conflicts write markers and can be finished with --continue
    or restored with --abort. --dry-run exits 1 when the merge would
    conflict (0 for ff/up-to-date/clean); --restart discards resolution
    work done so far, exactly like --abort, before re-running.";

#[derive(Parser, Debug)]
#[command(after_help = MERGE_EXAMPLES)]
pub struct MergeArgs {
    /// The branch to merge into the current branch, could be remote branch
    pub branch: Option<String>,

    /// Continue an in-progress merge after resolving conflicts
    #[arg(long = "continue", conflicts_with = "abort")]
    pub continue_merge: bool,

    /// Abort an in-progress merge and restore the pre-merge state
    #[arg(long, conflicts_with = "continue_merge")]
    pub abort: bool,

    /// Preview the merge outcome without writing anything (Libra extension —
    /// Git has no true merge dry-run): reports whether merging `<branch>` would
    /// fast-forward, already be up to date, merge cleanly, or conflict (and on
    /// which paths). No index, worktree, HEAD, reflog, object, or merge-state
    /// write happens. Exits 0 for a clean preview and 1 when the merge would
    /// conflict.
    #[arg(long = "dry-run", conflicts_with_all = ["continue_merge", "abort", "restart", "squash", "no_commit"])]
    pub dry_run: bool,

    /// Restart the in-progress conflicted merge from scratch (Libra extension,
    /// porting Lore's `branch merge restart`): abort it — restoring the
    /// pre-merge HEAD, index, and working tree exactly like `--abort`, which
    /// DISCARDS any conflict resolution done so far — then immediately re-run
    /// the same merge against the recorded target commit, regenerating fresh
    /// conflict markers. The re-run uses default merge options (an original
    /// `-m`/`--no-ff`/`--squash`/`--no-commit` is not replayed).
    #[arg(long, conflicts_with_all = ["branch", "continue_merge", "abort", "ff", "ff_only", "no_ff", "message", "squash", "no_commit", "verify_signatures"])]
    pub restart: bool,

    /// Refuse to merge unless the current branch can fast-forward to the target.
    #[arg(long = "ff-only", conflicts_with_all = ["ff", "no_ff", "continue_merge", "abort"])]
    pub ff_only: bool,

    /// Allow fast-forwarding when possible, overriding `merge.ff`.
    #[arg(long, conflicts_with_all = ["ff_only", "no_ff", "continue_merge", "abort"])]
    pub ff: bool,

    /// Always create a merge commit, even when a fast-forward would be possible.
    #[arg(long = "no-ff", conflicts_with_all = ["ff", "ff_only", "continue_merge", "abort"])]
    pub no_ff: bool,

    /// Use the given message for the merge commit instead of the default.
    #[arg(short = 'm', long = "message", value_name = "MSG", conflicts_with_all = ["continue_merge", "abort"])]
    pub message: Option<String>,

    /// Merge changes but stage the result without committing or moving HEAD
    /// (no merge info recorded); finalize with a normal `commit`.
    #[arg(long, conflicts_with_all = ["continue_merge", "abort"])]
    pub squash: bool,

    /// Perform the merge and stage the result but stop before committing,
    /// recording merge state; finalize with `libra merge --continue`.
    #[arg(long = "no-commit", conflicts_with_all = ["squash", "continue_merge", "abort"])]
    pub no_commit: bool,

    /// Automatically stash local changes before the merge and re-apply them
    /// when it concludes (also on failure to start). On a merge conflict the
    /// stash is HELD (not in `stash list`) and re-applied by `--continue` or
    /// `--abort`; if the re-apply itself conflicts, the stash is saved to the
    /// stash list and a notice is printed — changes are never lost. Config:
    /// `merge.autostash` (this flag and `--no-autostash` override it).
    #[arg(long = "autostash", overrides_with = "no_autostash", conflicts_with_all = ["continue_merge", "abort", "restart", "dry_run"])]
    pub autostash: bool,

    /// Disable autostash even when `merge.autostash` is configured.
    #[arg(long = "no-autostash", overrides_with = "autostash", conflicts_with_all = ["continue_merge", "abort", "restart"])]
    pub no_autostash: bool,

    /// Accept the auto-generated merge message without launching an editor.
    /// Libra never opens an editor for merge (it uses `-m` or the default
    /// message), so this is accepted for Git parity and is a no-op.
    #[arg(long = "no-edit")]
    pub no_edit: bool,

    /// Show a diffstat of the merge result at the end (what the merge changed,
    /// pre-merge HEAD vs the new commit). Git shows this by default; Libra
    /// defaults to no diffstat, so `--stat` opts in. Toggle pair with
    /// `--no-stat`/`-n`; the last one wins.
    #[arg(long = "stat", overrides_with = "no_stat")]
    pub stat: bool,

    /// Do not show a diffstat at the end of the merge (Libra's default).
    /// Accepted for Git parity. Toggle pair with `--stat`; the last one wins.
    #[arg(short = 'n', long = "no-stat", overrides_with = "stat")]
    pub no_stat: bool,

    /// Do not show a progress meter. Accepted for Git parity and is a no-op:
    /// Libra's merge never renders a progress meter, so there is nothing to
    /// suppress.
    #[arg(long = "no-progress")]
    pub no_progress: bool,

    /// Verify that the tip commit of the branch being merged carries a valid PGP
    /// signature, aborting the merge if it is unsigned or the signature is bad.
    /// Like `tag -v`, only signatures made by this repository's vault PGP key can
    /// be validated (Libra has no external GPG keyring), so a commit signed
    /// elsewhere — or with an SSH signature — is treated as not verifiable.
    #[arg(long = "verify-signatures", overrides_with = "no_verify_signatures", conflicts_with_all = ["continue_merge", "abort"])]
    pub verify_signatures: bool,

    /// Do not verify that the merged commits carry a valid GPG signature (the
    /// default). The inverse of `--verify-signatures`; last one wins.
    #[arg(long = "no-verify-signatures", overrides_with = "verify_signatures")]
    pub no_verify_signatures: bool,

    /// Do not update the rerere (reuse recorded resolution) index after the
    /// merge. Accepted for Git parity and is a no-op: `libra rerere` exists as a
    /// standalone command but is not yet auto-integrated into merge, so there is
    /// nothing to update here. (Git's `--rerere-autoupdate` is not exposed.)
    #[arg(long = "no-rerere-autoupdate")]
    pub no_rerere_autoupdate: bool,

    /// Do not GPG-sign the merge commit. Accepted for Git parity and is a no-op:
    /// Libra's merge never signs, so this already matches the default. (Git's
    /// opposite `-S`/`--gpg-sign` is not implemented.)
    #[arg(long = "no-gpg-sign")]
    pub no_gpg_sign: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PullMergeSummary {
    pub strategy: String,
    /// The previous HEAD commit before merge (None for root commits).
    pub old_commit: Option<String>,
    pub commit: Option<String>,
    pub files_changed: usize,
    pub up_to_date: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parents: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicted_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub aborted: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub continued: bool,
    /// `--dry-run`: this summary is a preview; nothing was written. Absent from
    /// JSON for every real merge (schema-frozen additive field).
    #[serde(default, skip_serializing_if = "is_false")]
    pub dry_run: bool,
    /// `--dry-run` only: the merge would stop on conflicts (in
    /// `conflicted_paths`). Absent from JSON for every real merge.
    #[serde(default, skip_serializing_if = "is_false")]
    pub would_conflict: bool,
    /// Autostash outcome (lore.md §1.8): `applied` (re-applied cleanly),
    /// `stashed` (re-apply conflicted; entry promoted to the stash list), or
    /// `kept` (held while merge state persists, e.g. `--no-commit`). Absent
    /// whenever autostash was off or the tree was clean (schema-additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autostash: Option<String>,
}

pub(crate) type MergeOutput = PullMergeSummary;

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Default)]
pub(crate) struct PullMergeOptions {
    pub ff_only: bool,
    /// Force a real merge commit even when the integration could fast-forward
    /// (`libra pull --no-ff`). When set, the fast-forward short-circuit is
    /// skipped and a two-parent merge commit is recorded instead.
    pub no_ff: bool,
    /// Override the merge-commit message (`libra merge -m <msg>`). `None` uses
    /// the default `Merge <upstream> into <head>` message.
    pub message: Option<String>,
    /// `libra merge --squash`: produce the merged index/worktree but do NOT
    /// create a commit or move HEAD (and never fast-forward), leaving the result
    /// staged for a subsequent normal `commit`.
    pub squash: bool,
    /// `libra merge --no-commit`: perform the merge and stage the result (never
    /// fast-forward) but stop before committing, recording a MergeState so
    /// `libra merge --continue` can finalize the two-parent commit.
    pub no_commit: bool,
    /// `libra merge --verify-signatures`: verify the resolved tip commit's PGP
    /// signature before mutating any state and abort if it is unsigned or invalid.
    /// Checked on the SAME loaded commit that is merged (no re-resolution), so the
    /// verified object is exactly the merged object. Always `false` for `pull`.
    pub verify_signatures: bool,
    /// Number of target-side subjects appended to an auto-generated merge
    /// message (`merge.log`). Always `0` for `pull`: its auto-merge keeps the
    /// plain message form; only `libra merge` reads the config.
    pub merge_log: usize,
    /// `libra merge --dry-run`: report the would-be outcome and write NOTHING —
    /// no index/worktree/HEAD/reflog/merge-state mutation and no object-store
    /// writes (auto-merged blobs are computed in memory only). Always `false`
    /// for `pull`.
    pub dry_run: bool,
    /// `merge --autostash` (lore.md §1.8): `Some(true)` = --autostash,
    /// `Some(false)` = --no-autostash, `None` = resolve `merge.autostash`
    /// config (git-bool; an invalid value is a hard error). Under --dry-run a
    /// config-enabled autostash is silently suppressed (dry-run writes nothing).
    pub autostash: Option<bool>,
    /// `--restart` re-entry only: skip the stale-sidecar recovery so the HELD
    /// autostash of the restarted merge is preserved (not demoted to the
    /// stash list as stale).
    pub preserve_held_autostash: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MergeState {
    pub head_name: String,
    pub orig_head: String,
    pub target: String,
    pub target_ref: String,
    pub base: String,
    pub conflicted_paths: Vec<String>,
    /// Merge message resolved at merge start (`-m` override or the generated
    /// default including the `merge.log` shortlog), replayed verbatim by
    /// `merge --continue`. `None` for states written by older binaries, which
    /// fall back to the plain `Merge <target> into <head>` form.
    #[serde(default)]
    pub message: Option<String>,
}

impl MergeState {
    fn path() -> PathBuf {
        util::storage_path().join("merge-state.json")
    }

    pub(crate) fn load_optional_sync() -> Result<Option<Self>, String> {
        let path = Self::path();
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        serde_json::from_str(&data)
            .map(Some)
            .map_err(|error| format!("failed to parse {}: {error}", path.display()))
    }

    fn load_required() -> Result<Self, PullMergeError> {
        Self::load_optional_sync()
            .map_err(PullMergeError::StateLoad)?
            .ok_or(PullMergeError::NoMergeInProgress)
    }

    fn save(&self) -> Result<(), PullMergeError> {
        let path = Self::path();
        let data = serde_json::to_vec_pretty(self)
            .map_err(|error| PullMergeError::StateSave(error.to_string()))?;
        // Atomic + fsynced write (lore.md §7.7): sequencer state is
        // recovery-critical, so a crash must leave it either fully written or
        // absent — never truncated — and it must survive a power loss.
        crate::utils::atomic_write::write_atomic(&path, &data, true)
            .map_err(|error| PullMergeError::StateSave(format!("{}: {error}", path.display())))
    }

    fn cleanup() -> Result<(), PullMergeError> {
        let path = Self::path();
        if !path.exists() {
            return Ok(());
        }
        fs::remove_file(&path)
            .map_err(|error| PullMergeError::StateCleanup(format!("{}: {error}", path.display())))
    }
}

/// The MERGE_AUTOSTASH analog (lore.md §1.8): while a merge holds an
/// autostash, its stash COMMIT OID lives in this sidecar (atomic + fsynced,
/// like MergeState) and deliberately NOT in refs/stash — `stash list` stays
/// clean until the merge concludes. The held commit is reachable only from
/// this file, so repository maintenance treats it as a fail-closed GC root.
/// OID stored as a string (sha1/sha256 both fit; never assume 40).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MergeAutostash {
    pub stash_commit: String,
}

impl MergeAutostash {
    fn path() -> PathBuf {
        util::storage_path().join("merge-autostash.json")
    }

    pub(crate) fn load_optional_sync() -> Result<Option<Self>, String> {
        let path = Self::path();
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        serde_json::from_str(&data)
            .map(Some)
            .map_err(|error| format!("failed to parse {}: {error}", path.display()))
    }

    fn save(&self) -> Result<(), PullMergeError> {
        let path = Self::path();
        let data = serde_json::to_vec_pretty(self)
            .map_err(|error| PullMergeError::Autostash(error.to_string()))?;
        crate::utils::atomic_write::write_atomic(&path, &data, true)
            .map_err(|error| PullMergeError::Autostash(format!("{}: {error}", path.display())))
    }

    fn cleanup() {
        let _ = fs::remove_file(Self::path());
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PullMergeError {
    #[error("merge requires a branch argument, --continue, or --abort")]
    MissingAction,
    #[error("merge accepts either a branch argument, --continue, or --abort")]
    ConflictingAction,
    /// The repository configures an unsupported `merge.conflictStyle` value.
    /// Surfaced only when a conflict actually needs rendering, and a hard error
    /// rather than a silent fall-back to the default style — a typo must not
    /// quietly change the conflict-marker format (`zdiff3` is not implemented).
    #[error("unsupported merge.conflictStyle '{0}' (expected 'merge' or 'diff3')")]
    InvalidConflictStyle(String),
    /// The `merge.conflictStyle` config could not be read (config-store I/O
    /// failure) — surfaced as an I/O error, never a silent default-style
    /// fall-back that would ignore a configured `diff3`.
    #[error("failed to read merge.conflictStyle config: {0}")]
    ConflictStyleRead(String),
    /// Autostash creation/apply/bookkeeping failure. The stash commit (when
    /// one exists) is referenced by merge-autostash.json — never lost.
    #[error("merge --autostash failed: {0}")]
    Autostash(String),
    /// `merge.autostash` holds a value that is not a git-bool — hard error, a
    /// typo must not silently toggle stashing (same policy as conflictStyle).
    #[error("unsupported merge.autostash '{0}' (expected a boolean)")]
    InvalidAutostashConfig(String),
    #[error("{0} - not something we can merge")]
    InvalidTarget(String),
    #[error("failed to load merge target '{commit_id}': {detail}")]
    TargetLoad { commit_id: String, detail: String },
    #[error("failed to load current commit '{commit_id}': {detail}")]
    CurrentLoad { commit_id: String, detail: String },
    #[error("failed to inspect merge history: {0}")]
    History(String),
    #[error("refusing to merge unrelated histories")]
    UnrelatedHistories,
    #[error("merge has conflicts in {paths}")]
    Conflicts { paths: String },
    #[error("no merge in progress")]
    NoMergeInProgress,
    /// `--restart` on an in-progress merge that has NO conflicts (a staged
    /// `--no-commit` merge). Restarting would silently discard the staged
    /// result and re-run with default options (possibly fast-forwarding), so
    /// it is refused — restart exists to redo a CONFLICTED merge.
    #[error("no conflicted merge to restart (the in-progress merge has no conflicts)")]
    RestartWithoutConflicts,
    #[error("merge already in progress")]
    MergeInProgress,
    #[error("you must resolve all merge conflicts before continuing")]
    UnresolvedConflicts,
    #[error("uncommitted changes, cannot merge")]
    DirtyWorktree,
    #[error("untracked working tree file would be overwritten by merge: {path}")]
    UntrackedOverwrite { path: String },
    #[error("non-fast-forward merge refused (current {current}, target {target})")]
    NonFastForward { current: String, target: String },
    #[error("failed to load merge state: {0}")]
    StateLoad(String),
    #[error("failed to save merge state: {0}")]
    StateSave(String),
    #[error("failed to clean up merge state: {0}")]
    StateCleanup(String),
    #[error("failed to load index: {0}")]
    IndexLoad(String),
    #[error("failed to save index: {0}")]
    IndexSave(String),
    #[error("failed to create merge tree: {0}")]
    TreeCreate(String),
    #[error("failed to save merge commit: {0}")]
    CommitSave(String),
    #[error("failed to reset working tree after merge: {0}")]
    WorkdirReset(String),
    #[error("failed to load tree '{tree_id}': {detail}")]
    TreeLoad { tree_id: String, detail: String },
    #[error("failed to load object '{object_id}': {detail}")]
    ObjectLoad { object_id: String, detail: String },
    #[error("failed to resolve HEAD state: {0}")]
    HeadResolve(String),
    #[error("failed to update HEAD during merge: {0}")]
    HeadUpdate(String),
    #[error("failed to restore working tree after merge: {0}")]
    Restore(String),
    #[error("commit {commit} does not have a GPG signature")]
    UnsignedMergeCommit { commit: String },
    #[error("commit {commit} has a bad GPG signature")]
    BadMergeSignature { commit: String },
    #[error("failed to verify the signature of the merged commit: {0}")]
    SignatureCheck(String),
    #[error(transparent)]
    HistoryConfig(#[from] crate::command::history_config::HistoryConfigError),
}

pub(crate) type MergeError = PullMergeError;

impl From<PullMergeError> for CliError {
    fn from(error: PullMergeError) -> Self {
        match &error {
            PullMergeError::MissingAction | PullMergeError::ConflictingAction => {
                CliError::command_usage(error.to_string())
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
            }
            PullMergeError::InvalidTarget(..) => CliError::command_usage(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidTarget),
            PullMergeError::TargetLoad { .. }
            | PullMergeError::CurrentLoad { .. }
            | PullMergeError::History(..)
            | PullMergeError::TreeLoad { .. }
            | PullMergeError::ObjectLoad { .. } => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::RepoCorrupt)
            }
            PullMergeError::UnrelatedHistories => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid),
            PullMergeError::UnsignedMergeCommit { .. }
            | PullMergeError::BadMergeSignature { .. } => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("the tip commit could not be verified against the vault PGP key")
                .with_hint("re-run without --verify-signatures to merge without verification"),
            PullMergeError::SignatureCheck(..) => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint(
                    "ensure the repository vault is initialized and unsealed for signature verification",
                )
                .with_hint("re-run without --verify-signatures to merge without verification"),
            PullMergeError::NonFastForward { .. } => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                .with_hint("run 'libra pull' without --ff-only to allow a merge commit")
                .with_hint("or run 'libra pull --rebase' to replay local commits"),
            PullMergeError::Conflicts { .. }
            | PullMergeError::DirtyWorktree
            | PullMergeError::UntrackedOverwrite { .. }
            | PullMergeError::MergeInProgress
            | PullMergeError::UnresolvedConflicts => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                .with_hint("resolve conflicts, then run 'libra merge --continue'")
                .with_hint("or run 'libra merge --abort' to restore the pre-merge state"),
            PullMergeError::NoMergeInProgress => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid),
            PullMergeError::RestartWithoutConflicts => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("finish the staged merge with 'libra merge --continue'")
                .with_hint("or discard it with 'libra merge --abort'"),
            PullMergeError::Autostash(..) => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                .with_detail("phase", "autostash"),
            PullMergeError::InvalidAutostashConfig(..) => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("set merge.autostash to true/false (or remove it)"),
            PullMergeError::InvalidConflictStyle(..) => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("set merge.conflictStyle to 'merge' (default) or 'diff3'"),
            PullMergeError::ConflictStyleRead(..) => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
            }
            PullMergeError::HistoryConfig(
                crate::command::history_config::HistoryConfigError::Read { .. },
            ) => CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed),
            PullMergeError::HistoryConfig(
                crate::command::history_config::HistoryConfigError::Invalid { .. },
            ) => CliError::command_usage(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("fix the offending value with 'libra config <key> <value>'"),
            PullMergeError::StateLoad(..) | PullMergeError::IndexLoad(..) => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
            }
            PullMergeError::StateSave(..)
            | PullMergeError::StateCleanup(..)
            | PullMergeError::IndexSave(..)
            | PullMergeError::TreeCreate(..)
            | PullMergeError::CommitSave(..)
            | PullMergeError::WorkdirReset(..) => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoWriteFailed)
            }
            PullMergeError::HeadResolve(..) => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
            }
            PullMergeError::HeadUpdate(..) | PullMergeError::Restore(..) => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoWriteFailed)
            }
        }
    }
}

pub async fn execute(args: MergeArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting.
///
/// # Side Effects
/// - Resolves and reads the current and target commits.
/// - Performs a fast-forward merge for supported cases.
/// - Updates HEAD/current branch and restores the working tree to the merged
///   tree state.
/// - Emits merge status text through [`OutputConfig`].
///
/// # Errors
/// Returns [`CliError`] when the target is invalid, histories are unrelated,
/// conflicts need resolution, objects cannot be read, or HEAD/worktree updates fail.
pub async fn execute_safe(args: MergeArgs, output: &OutputConfig) -> CliResult<()> {
    crate::command::ensure_main_worktree("merge")?;
    // Symmetric sequencer mutex (lore.md 2.6): refuse a merge while ANY other
    // sequence (cherry-pick/revert/rebase) is unresolved. Same-op (a merge
    // already in progress) is intentionally deferred to merge's OWN typed
    // guard — `run_merge_for_pull_with_options` raises `MergeInProgress` when
    // `MergeState` is present — so this stays the cross-op mutex only.
    crate::internal::sequencer::ensure_none_in_progress(
        crate::internal::sequencer::SequenceKind::Merge,
    )
    .await?;
    // `args` is moved into `run_merge`; capture the diffstat opt-in first.
    let show_stat = args.stat;
    let result = run_merge(args, output).await.map_err(merge_error_to_cli)?;
    render_merge_output(&result, output)?;
    maybe_print_merge_stat(show_stat, &result, output).await;
    // `--dry-run` that would conflict: the summary (human or JSON) has been
    // rendered; exit 1 to signal the outcome — mirroring `merge-file`'s
    // conflict-with-output exit and `diff --exit-code`. Deliberately not the
    // 128 a REAL conflicting merge exits with: the preview succeeded and wrote
    // nothing, so this is an outcome signal, not an error.
    if result.dry_run && result.would_conflict {
        return Err(CliError::silent_exit(1));
    }
    Ok(())
}

/// `--stat`: print a Git-style diffstat of what the merge changed (pre-merge
/// HEAD vs the new commit). Human output only — `--json` already exposes
/// `files_changed`. Skipped when there is no completed new commit (up-to-date,
/// aborted, conflicted, or squash/no-commit that did not move HEAD). A failure
/// to compute the stat is non-fatal: the merge already succeeded.
async fn maybe_print_merge_stat(show_stat: bool, result: &MergeOutput, output: &OutputConfig) {
    if !show_stat || output.is_json() || output.quiet || !result.conflicted_paths.is_empty() {
        return;
    }
    let (Some(old), Some(new)) = (result.old_commit.as_deref(), result.commit.as_deref()) else {
        return;
    };
    let (Ok(old_hash), Ok(new_hash)) = (ObjectHash::from_str(old), ObjectHash::from_str(new))
    else {
        return;
    };
    match crate::command::diff::diff_stat_between_commits(&old_hash, &new_hash).await {
        Ok(stat) if !stat.trim().is_empty() => print!("{stat}"),
        Ok(_) => {}
        Err(err) => tracing::warn!(error = %err, "failed to compute merge diffstat"),
    }
}

async fn run_merge(args: MergeArgs, output: &OutputConfig) -> Result<MergeOutput, MergeError> {
    // `--restart` operates on the saved merge state alone; clap guarantees no
    // branch positional or option flags accompany it (conflicts_with_all).
    if args.restart {
        return run_merge_restart(output).await;
    }
    match (args.branch.as_deref(), args.continue_merge, args.abort) {
        (Some(branch), false, false) => {
            let (ff_only, no_ff) = if args.ff_only {
                (true, false)
            } else if args.no_ff {
                (false, true)
            } else if args.ff {
                (false, false)
            } else {
                match crate::command::history_config::merge_fast_forward().await? {
                    Some(crate::command::history_config::MergeFastForward::Allow) | None => {
                        (false, false)
                    }
                    Some(crate::command::history_config::MergeFastForward::CreateMergeCommit) => {
                        (false, true)
                    }
                    Some(crate::command::history_config::MergeFastForward::Only) => (true, false),
                }
            };
            let verify_signatures = if args.verify_signatures {
                true
            } else if args.no_verify_signatures {
                false
            } else {
                crate::command::history_config::merge_verify_signatures()
                    .await?
                    .unwrap_or(false)
            };
            let merge_log = if args.message.is_some() {
                0
            } else {
                crate::command::history_config::merge_log_limit().await?
            };
            let options = PullMergeOptions {
                ff_only,
                no_ff,
                message: args.message.clone(),
                squash: args.squash,
                no_commit: args.no_commit,
                // `--verify-signatures` is enforced inside the merge on the loaded
                // tip commit, so the verified object is exactly the merged object.
                verify_signatures,
                merge_log,
                dry_run: args.dry_run,
                autostash: if args.autostash {
                    Some(true)
                } else if args.no_autostash {
                    Some(false)
                } else {
                    None
                },
                preserve_held_autostash: false,
            };
            run_merge_for_pull_with_options(branch, branch, output, options).await
        }
        (None, true, false) => run_merge_continue(output).await,
        (None, false, true) => run_merge_abort(output).await,
        (None, false, false) => Err(MergeError::MissingAction),
        _ => Err(MergeError::ConflictingAction),
    }
}

/// Verify `commit`'s PGP signature for a `--verify-signatures` merge, returning
/// a typed abort error when it is unsigned or the signature does not validate
/// against the vault PGP key. Run on the already-loaded tip commit (before any
/// state mutation) so the verified object is exactly the one being merged.
async fn verify_merge_commit_signature(commit: &Commit) -> Result<(), MergeError> {
    use crate::command::commit::{CommitSignatureStatus, verify_commit_signature};

    match verify_commit_signature(commit).await {
        Ok(CommitSignatureStatus::Good) => Ok(()),
        Ok(CommitSignatureStatus::Unsigned) => Err(MergeError::UnsignedMergeCommit {
            commit: commit.id.to_string(),
        }),
        Ok(CommitSignatureStatus::Bad) => Err(MergeError::BadMergeSignature {
            commit: commit.id.to_string(),
        }),
        Err(error) => Err(MergeError::SignatureCheck(error.to_string())),
    }
}

fn render_merge_output(result: &MergeOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("merge", result, output);
    }
    if output.quiet {
        return Ok(());
    }

    if result.dry_run {
        // `--dry-run`: preview phrasing — nothing was written, so the normal
        // messages ("Fast-forward", "fix conflicts and then commit") would be
        // misleading or outright wrong here.
        if result.up_to_date {
            info_println!(output, "Already up to date.");
        } else if result.would_conflict {
            info_println!(
                output,
                "Would conflict in: {}\n(dry run: nothing was written)",
                result.conflicted_paths.join(", ")
            );
        } else if result.strategy == "fast-forward" {
            info_println!(output, "Would fast-forward\n(dry run: nothing was written)");
        } else {
            info_println!(
                output,
                "Would merge cleanly by the 'three-way' strategy.\n(dry run: nothing was written)"
            );
        }
        return Ok(());
    }

    if result.up_to_date {
        info_println!(output, "Already up to date.");
    } else if result.aborted {
        info_println!(output, "Merge aborted.");
    } else if result.continued {
        info_println!(output, "Merge completed.");
    } else if !result.conflicted_paths.is_empty() {
        info_println!(
            output,
            "Automatic merge failed; fix conflicts and then commit the result."
        );
    } else {
        match result.strategy.as_str() {
            "three-way" => info_println!(output, "Merge made by the 'three-way' strategy."),
            "squash" => info_println!(output, "Squash commit -- not updating HEAD"),
            "no-commit" => info_println!(
                output,
                "Automatic merge went well; stopped before committing as requested\n\
                 finalize with 'libra merge --continue'"
            ),
            _ => info_println!(output, "Fast-forward"),
        }
    }
    Ok(())
}

fn merge_error_to_cli(error: MergeError) -> CliError {
    match error {
        MergeError::Conflicts { .. } => CliError::from(error)
            .with_priority_hint("resolve conflicts, then run 'libra merge --continue'")
            .with_hint("or run 'libra merge --abort' to restore the pre-merge state"),
        error => CliError::from(error),
    }
}

/// Resolve whether autostash is enabled: explicit flag wins; otherwise the
/// `merge.autostash` git-bool config (invalid value = hard error). Always off
/// under `--dry-run` (its contract is zero writes).
async fn autostash_enabled(options: &PullMergeOptions) -> Result<bool, PullMergeError> {
    if options.dry_run {
        return Ok(false);
    }
    if let Some(explicit) = options.autostash {
        return Ok(explicit);
    }
    let entry = ConfigKv::get_var_case_insensitive("merge.", "autostash")
        .await
        .map_err(|error| PullMergeError::Autostash(format!("config read failed: {error}")))?;
    match entry
        .map(|entry| entry.value.trim().to_ascii_lowercase())
        .as_deref()
    {
        None | Some("false") | Some("no") | Some("off") | Some("0") | Some("") => Ok(false),
        Some("true") | Some("yes") | Some("on") | Some("1") => Ok(true),
        Some(other) => Err(PullMergeError::InvalidAutostashConfig(other.to_string())),
    }
}

/// Finalize rule for the held autostash — runs after EVERY merge action
/// (start, --continue, --abort; success or failure): if the sidecar exists
/// and no merge is in progress, re-apply the held stash. Clean apply →
/// sidecar dropped; apply conflict → stash promoted into refs/stash with a
/// notice (never lost — the lore 1.8 headline); other apply error → sidecar
/// KEPT and a warning printed (the merge outcome itself is never changed).
/// While merge state persists the stash simply stays held.
async fn resolve_pending_autostash(output: &OutputConfig) -> Option<String> {
    let sidecar = match MergeAutostash::load_optional_sync() {
        Ok(Some(sidecar)) => sidecar,
        Ok(None) => return None,
        Err(detail) => {
            crate::utils::error::emit_warning(format!(
                "could not read merge-autostash.json ({detail}); leaving it in place"
            ));
            return None;
        }
    };
    match MergeState::load_optional_sync() {
        Ok(None) => {}
        // Merge still in progress (conflict / --no-commit): keep holding.
        Ok(Some(_)) => return Some("kept".to_string()),
        Err(detail) => {
            crate::utils::error::emit_warning(format!(
                "could not inspect merge state ({detail}); autostash left held"
            ));
            return Some("kept".to_string());
        }
    }
    let oid = match ObjectHash::from_str(&sidecar.stash_commit) {
        Ok(oid) => oid,
        Err(error) => {
            crate::utils::error::emit_warning(format!(
                "merge-autostash.json holds an invalid OID ({error}); leaving it in place"
            ));
            return None;
        }
    };
    match crate::command::stash::apply_held_stash_commit(&oid).await {
        Ok(()) => {
            MergeAutostash::cleanup();
            if !output.quiet {
                eprintln!("Applied autostash.");
            }
            Some("applied".to_string())
        }
        Err(crate::command::stash::StashError::MergeConflict(_)) => {
            // All-or-nothing apply: the merge result is intact. Promote the
            // stash into the visible list so nothing is lost.
            match crate::command::stash::store_stash_commit(&oid, "autostash").await {
                Ok(()) => {
                    MergeAutostash::cleanup();
                    if !output.quiet {
                        eprintln!(
                            "Applying autostash resulted in conflicts.\nYour changes are safe in the stash (stash@{{0}}).\nYou can run \"libra stash pop\" or \"libra stash drop\" at any time."
                        );
                    }
                    Some("stashed".to_string())
                }
                Err(error) => {
                    crate::utils::error::emit_warning(format!(
                        "failed to store the autostash into the stash list ({error}); \
                         merge-autostash.json still references stash commit {oid}"
                    ));
                    None
                }
            }
        }
        Err(error) => {
            crate::utils::error::emit_warning(format!(
                "failed to re-apply the autostash ({error}); \
                 merge-autostash.json still references stash commit {oid}"
            ));
            None
        }
    }
}

pub(crate) async fn run_merge_for_pull_with_options(
    target_ref: &str,
    upstream: &str,
    output: &OutputConfig,
    options: PullMergeOptions,
) -> Result<PullMergeSummary, PullMergeError> {
    if MergeState::load_optional_sync()
        .map_err(PullMergeError::StateLoad)?
        .is_some()
    {
        return Err(PullMergeError::MergeInProgress);
    }

    // Resolve and load the merge target up front so `--verify-signatures` /
    // `merge.verifySignatures` runs BEFORE any mutation — including autostash
    // object writes and stale-sidecar recovery below. The loaded commit is
    // passed through to the merge itself, so the verified object is exactly
    // the merged object (no time-of-check/time-of-use re-resolution gap).
    let commit_hash = resolve_merge_target(target_ref)
        .await
        .map_err(|_| PullMergeError::InvalidTarget(upstream.to_string()))?;
    let target_commit: Commit =
        load_object(&commit_hash).map_err(|error| PullMergeError::TargetLoad {
            commit_id: commit_hash.to_string(),
            detail: error.to_string(),
        })?;
    if options.verify_signatures {
        verify_merge_commit_signature(&target_commit).await?;
    }

    // ── autostash (lore.md §1.8) ──
    // Stale-sidecar recovery: a leftover sidecar with NO merge in progress
    // (crash after a finalize apply, or an interrupted start) is promoted to
    // the stash list — never overwritten or lost. Skipped on --restart
    // re-entry, where the HELD sidecar legitimately exists without state.
    if !options.preserve_held_autostash
        && let Ok(Some(sidecar)) = MergeAutostash::load_optional_sync()
    {
        if let Ok(oid) = ObjectHash::from_str(&sidecar.stash_commit) {
            match crate::command::stash::store_stash_commit(&oid, "autostash").await {
                Ok(()) => {
                    MergeAutostash::cleanup();
                    crate::utils::error::emit_warning(
                        "recovered a leftover autostash into the stash list (it may \
                         duplicate already-restored changes — inspect with 'libra stash show')",
                    );
                }
                Err(error) => {
                    return Err(PullMergeError::Autostash(format!(
                        "cannot recover the leftover autostash: {error}"
                    )));
                }
            }
        } else {
            return Err(PullMergeError::Autostash(
                "merge-autostash.json holds an invalid OID; inspect and remove it".to_string(),
            ));
        }
    }
    let autostash_on = autostash_enabled(&options).await?;
    if autostash_on && Head::current_commit().await.is_some() {
        match crate::command::stash::create_held_stash_commit("autostash").await {
            Ok(Some(stash_commit)) => {
                // ORDER IS LOAD-BEARING: objects → sidecar (durable
                // reference) → reset. A crash after the sidecar but before
                // the reset leaves a dirty tree + sidecar, which the stale
                // recovery promotes (may-duplicate warning); a crash before
                // the sidecar leaves the tree untouched. At no point are the
                // changes gone from the tree while unreferenced.
                MergeAutostash {
                    stash_commit: stash_commit.to_string(),
                }
                .save()?;
                if let Err(error) = crate::command::stash::reset_to_head_for_held_stash().await {
                    return Err(PullMergeError::Autostash(format!(
                        "created the autostash but failed to reset the tree: {error} \
                         (merge-autostash.json references stash commit {stash_commit})"
                    )));
                }
                if !output.quiet {
                    eprintln!("Created autostash: {stash_commit}");
                }
            }
            Ok(None) => {} // clean tree: strict no-op
            Err(error) => {
                return Err(PullMergeError::Autostash(error.to_string()));
            }
        }
    }

    let result = run_merge_for_pull_inner(target_commit, upstream, output, options).await;
    // Uniform finalize: applies when no merge state persists (clean success,
    // up-to-date, squash, or a start failure), holds while state exists
    // (conflict / --no-commit). The merge outcome itself is never changed.
    let autostash_outcome = resolve_pending_autostash(output).await;
    match result {
        Ok(mut summary) => {
            summary.autostash = autostash_outcome;
            Ok(summary)
        }
        Err(error) => Err(error),
    }
}

async fn run_merge_for_pull_inner(
    // Pre-resolved and (when requested) signature-verified by
    // `run_merge_for_pull_with_options` BEFORE autostash/recovery mutations;
    // reusing the same loaded object keeps verify-and-merge TOCTOU-free.
    target_commit: Commit,
    upstream: &str,
    output: &OutputConfig,
    options: PullMergeOptions,
) -> Result<PullMergeSummary, PullMergeError> {
    let Some(current_commit_id) = Head::current_commit().await else {
        let files_changed = count_changed_files(None, &target_commit)?;
        // `--dry-run`: report the fast-forward preview without applying it
        // (count_changed_files is read-only).
        if !options.dry_run {
            apply_fast_forward_merge(target_commit.clone(), upstream, output).await?;
        }
        return Ok(PullMergeSummary {
            strategy: "fast-forward".to_string(),
            old_commit: None,
            commit: Some(target_commit.id.to_string()),
            files_changed,
            up_to_date: false,
            parents: Vec::new(),
            conflicted_paths: Vec::new(),
            aborted: false,
            continued: false,
            dry_run: options.dry_run,
            would_conflict: false,
            autostash: None,
        });
    };
    let current_commit: Commit =
        load_object(&current_commit_id).map_err(|error| PullMergeError::CurrentLoad {
            commit_id: current_commit_id.to_string(),
            detail: error.to_string(),
        })?;

    let lca = lca_commit(&current_commit, &target_commit)
        .map_err(|error| PullMergeError::History(error.to_string()))?;

    let lca = lca.ok_or(PullMergeError::UnrelatedHistories)?;

    if lca.id == target_commit.id {
        return Ok(PullMergeSummary {
            strategy: "already-up-to-date".to_string(),
            old_commit: Some(current_commit_id.to_string()),
            commit: None,
            files_changed: 0,
            up_to_date: true,
            parents: Vec::new(),
            conflicted_paths: Vec::new(),
            aborted: false,
            continued: false,
            dry_run: options.dry_run,
            would_conflict: false,
            autostash: None,
        });
    }

    if lca.id == current_commit.id && !options.no_ff && !options.squash && !options.no_commit {
        let files_changed = count_changed_files(Some(&current_commit), &target_commit)?;
        // `--dry-run`: report the fast-forward preview without applying it.
        if !options.dry_run {
            apply_fast_forward_merge(target_commit.clone(), upstream, output).await?;
        }
        return Ok(PullMergeSummary {
            strategy: "fast-forward".to_string(),
            old_commit: Some(current_commit_id.to_string()),
            commit: Some(target_commit.id.to_string()),
            files_changed,
            up_to_date: false,
            parents: Vec::new(),
            conflicted_paths: Vec::new(),
            aborted: false,
            continued: false,
            dry_run: options.dry_run,
            would_conflict: false,
            autostash: None,
        });
    }

    // `--no-ff` cannot be combined with `--ff-only` (clap rejects the pair on
    // the pull surface). `ff_only` (flag or `merge.ff=only`) must reject only
    // a genuinely diverged history: a fast-forwardable `--squash`/`--no-commit`
    // merely skipped the fast-forward branch above and is allowed (Git accepts
    // `merge.ff=only` + `--squash` when the target is fast-forwardable).
    if options.ff_only && lca.id != current_commit.id {
        return Err(PullMergeError::NonFastForward {
            current: current_commit.id.to_string(),
            target: target_commit.id.to_string(),
        });
    }

    perform_three_way_merge(
        current_commit,
        target_commit,
        lca,
        upstream,
        ThreeWayMergeOptions {
            message_override: options.message.clone(),
            merge_log: options.merge_log,
            squash: options.squash,
            no_commit: options.no_commit,
            dry_run: options.dry_run,
            output,
        },
    )
    .await
}

struct ThreeWayMergeResult {
    merged_items: HashMap<PathBuf, MergeTreeEntry>,
    conflicts: Vec<(PathBuf, ConflictKind)>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct MergeTreeEntry {
    pub(crate) hash: ObjectHash,
    pub(crate) mode: TreeItemMode,
}

impl MergeTreeEntry {
    pub(crate) fn new(hash: ObjectHash, mode: TreeItemMode) -> Self {
        Self { hash, mode }
    }
}

struct ThreeWayMergeOptions<'a> {
    message_override: Option<String>,
    merge_log: usize,
    squash: bool,
    no_commit: bool,
    /// Preview only: compute the outcome, write nothing (lore.md §1.3).
    dry_run: bool,
    output: &'a OutputConfig,
}

async fn perform_three_way_merge(
    current_commit: Commit,
    target_commit: Commit,
    base_commit: Commit,
    upstream: &str,
    options: ThreeWayMergeOptions<'_>,
) -> Result<PullMergeSummary, PullMergeError> {
    // `--dry-run` never writes, so it may preview on a dirty tree (documented:
    // the preview does not validate worktree cleanliness — a real merge may
    // still refuse). Every other path must start clean.
    if !options.dry_run {
        switch::ensure_clean_status(options.output)
            .await
            .map_err(|_| PullMergeError::DirtyWorktree)?;
    }

    let head_name = current_head_name().await?;
    let base_items = commit_tree_items(&base_commit)?;
    let our_items = commit_tree_items(&current_commit)?;
    let their_items = commit_tree_items(&target_commit)?;
    // Under `--dry-run`, auto-merged blobs are computed in memory only
    // (persist=false) so the preview writes nothing to the object store —
    // under tiered storage a `save_object` would even upload to the remote.
    let merge_result = merge_tree_items(&base_items, &our_items, &their_items, !options.dry_run)?;
    let files_changed = count_item_map_changes(&our_items, &merge_result.merged_items);

    // `--dry-run`: the outcome is fully known here — report it and stop before
    // the FIRST write (no merge state, index, worktree, HEAD, or reflog
    // mutation; no conflict markers; conflict-style config not consulted).
    if options.dry_run {
        let conflicted_paths: Vec<String> = merge_result
            .conflicts
            .iter()
            .map(|(path, _)| path.display().to_string())
            .collect();
        let would_conflict = !conflicted_paths.is_empty();
        return Ok(PullMergeSummary {
            strategy: "three-way".to_string(),
            old_commit: Some(current_commit.id.to_string()),
            commit: None,
            files_changed,
            up_to_date: false,
            parents: Vec::new(),
            conflicted_paths,
            aborted: false,
            continued: false,
            dry_run: true,
            would_conflict,
            autostash: None,
        });
    }

    // Resolve the final merge message ONCE, up front — `-m` override or the
    // generated default including the `merge.log` shortlog — so the conflict
    // and `--no-commit` states persist it and `merge --continue` replays it
    // instead of regenerating a plain message (which would drop `-m` and the
    // configured shortlog).
    let resolved_message = match &options.message_override {
        Some(message) => message.clone(),
        None => crate::command::merge_message::default_message(
            current_commit.id,
            target_commit.id,
            upstream,
            &head_name,
            options.merge_log,
        )
        .map_err(PullMergeError::History)?,
    };

    if !merge_result.conflicts.is_empty() {
        // Resolved only on the conflict path: a clean merge never renders
        // markers, so an invalid style config cannot block it.
        let conflict_style = conflict_style_from_config().await.map_err(|e| match e {
            ConflictStyleError::Invalid(value) => PullMergeError::InvalidConflictStyle(value),
            ConflictStyleError::Read(detail) => PullMergeError::ConflictStyleRead(detail),
        })?;
        write_conflicted_merge_state(MergeConflictInput {
            head_name,
            message: resolved_message,
            upstream: upstream.to_string(),
            base: base_commit.id,
            ours: current_commit.id,
            theirs: target_commit.id,
            merged_items: merge_result.merged_items,
            conflicts: merge_result.conflicts,
            base_items,
            our_items,
            their_items,
            conflict_style,
        })?;
        // rerere: record the preimage of each merge conflict just written and
        // replay a recorded resolution if one matches. A no-op unless
        // `rerere.enabled`; staging of a replayed file follows `rerere.autoUpdate`
        // (merge does not expose a per-invocation `--rerere-autoupdate`).
        if let Err(error) = crate::command::rerere::auto_update(false).await {
            tracing::warn!("rerere auto-update after merge conflict failed: {error}");
        }
        let paths = MergeState::load_required()?.conflicted_paths.join(", ");
        return Err(PullMergeError::Conflicts { paths });
    }

    let current_index =
        Index::load(path::index()).map_err(|error| PullMergeError::IndexLoad(error.to_string()))?;
    let paths_to_write: Vec<PathBuf> = merge_result.merged_items.keys().cloned().collect();
    ensure_no_untracked_conflicts(&current_index, &paths_to_write)?;

    let tree_id = create_tree_from_items_map(&merge_result.merged_items)
        .map_err(PullMergeError::TreeCreate)?;

    if options.squash {
        // `--squash`: update the index/worktree to the merged tree but do not
        // create a commit or move HEAD, leaving the result staged for a normal
        // `commit`. No MERGE_HEAD/merge info is recorded (matches Git).
        reset_index_and_workdir_to_tree(&tree_id)?;
        return Ok(PullMergeSummary {
            strategy: "squash".to_string(),
            old_commit: Some(current_commit.id.to_string()),
            commit: None,
            files_changed,
            up_to_date: false,
            parents: Vec::new(),
            conflicted_paths: Vec::new(),
            aborted: false,
            continued: false,
            dry_run: false,
            would_conflict: false,
            autostash: None,
        });
    }

    if options.no_commit {
        // `--no-commit`: stage the (conflict-free) merged tree but stop before
        // committing, recording a MergeState with no conflicted paths so
        // `libra merge --continue` finalizes the two-parent commit. (Unlike Git,
        // a plain `commit` would record only one parent, so the result must be
        // finalized via `merge --continue`.)
        reset_index_and_workdir_to_tree(&tree_id)?;
        MergeState {
            head_name: head_name.clone(),
            orig_head: current_commit.id.to_string(),
            target: target_commit.id.to_string(),
            target_ref: upstream.to_string(),
            base: base_commit.id.to_string(),
            conflicted_paths: Vec::new(),
            message: Some(resolved_message.clone()),
        }
        .save()?;
        return Ok(PullMergeSummary {
            strategy: "no-commit".to_string(),
            old_commit: Some(current_commit.id.to_string()),
            commit: None,
            files_changed,
            up_to_date: false,
            parents: vec![current_commit.id.to_string(), target_commit.id.to_string()],
            conflicted_paths: Vec::new(),
            aborted: false,
            continued: false,
            dry_run: false,
            would_conflict: false,
            autostash: None,
        });
    }

    let message = resolved_message;
    let merge_commit = Commit::from_tree_id(
        tree_id,
        vec![current_commit.id, target_commit.id],
        &format_commit_msg(&message, None),
    );
    save_object(&merge_commit, &merge_commit.id)
        .map_err(|error| PullMergeError::CommitSave(error.to_string()))?;
    update_head_with_reflog(&head_name, merge_commit.id, upstream, "three-way").await?;
    reset_index_and_workdir_to_tree(&tree_id)?;

    Ok(PullMergeSummary {
        strategy: "three-way".to_string(),
        old_commit: Some(current_commit.id.to_string()),
        commit: Some(merge_commit.id.to_string()),
        files_changed,
        up_to_date: false,
        parents: vec![current_commit.id.to_string(), target_commit.id.to_string()],
        conflicted_paths: Vec::new(),
        aborted: false,
        continued: false,
        dry_run: false,
        would_conflict: false,
        autostash: None,
    })
}

/// Resolve the conflict-marker style from the Git-compatible
/// `merge.conflictStyle` config key (lore.md §1.3): unset/`merge` → the default
/// two-marker style, `diff3` → additionally emit the `||||||| base` block.
/// Matching Git, this is config-only — `git merge` has no CLI style flag. An
/// unrecognized value (including the unimplemented `zdiff3`) is a hard error so
/// a typo never silently changes the marker format. Consulted only when a
/// conflict actually needs rendering; shared by `merge`/`pull` and
/// `cherry-pick`, which use the same line-level renderer.
/// Why [`conflict_style_from_config`] could not produce a style: the configured
/// value is unsupported, or the config store itself could not be read. The two
/// are distinct on purpose — a read failure must surface as an I/O problem, not
/// silently fall back to the default style (which could ignore a configured
/// `diff3`).
pub(crate) enum ConflictStyleError {
    Invalid(String),
    Read(String),
}

pub(crate) async fn conflict_style_from_config() -> Result<diffy::ConflictStyle, ConflictStyleError>
{
    // Case-insensitive variable lookup: Git config variable names are
    // case-insensitive, and Libra stores keys verbatim, so both
    // `merge.conflictStyle` and `merge.conflictstyle` spellings must match.
    let entry = ConfigKv::get_var_case_insensitive("merge.", "conflictStyle")
        .await
        .map_err(|error| ConflictStyleError::Read(error.to_string()))?;
    match entry
        .map(|entry| entry.value.trim().to_ascii_lowercase())
        .as_deref()
    {
        None | Some("") | Some("merge") => Ok(diffy::ConflictStyle::Merge),
        Some("diff3") => Ok(diffy::ConflictStyle::Diff3),
        Some(other) => Err(ConflictStyleError::Invalid(other.to_string())),
    }
}

struct MergeConflictInput {
    head_name: String,
    /// Resolved merge message (see [`MergeState::message`]).
    message: String,
    upstream: String,
    base: ObjectHash,
    ours: ObjectHash,
    theirs: ObjectHash,
    merged_items: HashMap<PathBuf, MergeTreeEntry>,
    conflicts: Vec<(PathBuf, ConflictKind)>,
    base_items: HashMap<PathBuf, MergeTreeEntry>,
    our_items: HashMap<PathBuf, MergeTreeEntry>,
    their_items: HashMap<PathBuf, MergeTreeEntry>,
    /// Marker style for conflicted paths, resolved from `merge.conflictStyle`.
    conflict_style: diffy::ConflictStyle,
}

fn write_conflicted_merge_state(input: MergeConflictInput) -> Result<(), PullMergeError> {
    let current_index =
        Index::load(path::index()).map_err(|error| PullMergeError::IndexLoad(error.to_string()))?;

    let conflict_paths: Vec<PathBuf> = input
        .conflicts
        .iter()
        .map(|(path, _)| path.clone())
        .collect();
    let paths_to_write: Vec<PathBuf> = input
        .merged_items
        .keys()
        .cloned()
        .chain(conflict_paths.iter().cloned())
        .collect();
    ensure_no_untracked_conflicts(&current_index, &paths_to_write)?;

    let conflict_set: HashSet<PathBuf> = conflict_paths.iter().cloned().collect();
    let workdir = util::working_dir();
    let marker_eol = conflict_marker_eol();
    let theirs_abbrev = short_object_id(&input.theirs);

    let mut index = Index::new();
    for (path, entry) in &input.merged_items {
        add_blob_index_entry(&mut index, path, *entry, 0)?;
    }
    for path in &conflict_paths {
        if let Some(entry) = input.base_items.get(path) {
            add_blob_index_entry(&mut index, path, *entry, 1)?;
        }
        if let Some(entry) = input.our_items.get(path) {
            add_blob_index_entry(&mut index, path, *entry, 2)?;
        }
        if let Some(entry) = input.their_items.get(path) {
            add_blob_index_entry(&mut index, path, *entry, 3)?;
        }
    }

    let state = MergeState {
        head_name: input.head_name,
        orig_head: input.ours.to_string(),
        target: input.theirs.to_string(),
        target_ref: input.upstream,
        base: input.base.to_string(),
        conflicted_paths: conflict_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        message: Some(input.message),
    };
    state.save()?;

    if let Err(error) = index.save(path::index()) {
        let _ = MergeState::cleanup();
        return Err(PullMergeError::IndexSave(error.to_string()));
    }

    for (path, entry) in &input.merged_items {
        let blob: Blob = load_object(&entry.hash).map_err(|error| {
            PullMergeError::WorkdirReset(format!(
                "failed to load merged blob {} for '{}': {error}",
                entry.hash,
                path.display()
            ))
        })?;
        write_workdir_file(&workdir, path, &blob.data).map_err(PullMergeError::WorkdirReset)?;
    }

    let mut tracked_paths: HashSet<PathBuf> = current_index.tracked_files().into_iter().collect();
    tracked_paths.extend(input.base_items.keys().cloned());
    tracked_paths.extend(input.our_items.keys().cloned());
    tracked_paths.extend(input.their_items.keys().cloned());
    for path in tracked_paths {
        if conflict_set.contains(&path) || input.merged_items.contains_key(&path) {
            continue;
        }
        let full_path = workdir.join(&path);
        if full_path.exists() {
            fs::remove_file(&full_path).map_err(|error| {
                PullMergeError::WorkdirReset(format!(
                    "failed to remove {}: {error}",
                    path.display()
                ))
            })?;
        }
    }

    for (path, kind) in &input.conflicts {
        write_conflict_markers(
            &workdir,
            path,
            marker_eol,
            &theirs_abbrev,
            *kind,
            input.conflict_style,
        )
        .map_err(PullMergeError::WorkdirReset)?;
    }

    Ok(())
}

async fn run_merge_continue(output: &OutputConfig) -> Result<MergeOutput, MergeError> {
    let state = MergeState::load_required()?;
    ensure_no_unstaged_changes_for_continue()?;
    let index =
        Index::load(path::index()).map_err(|error| MergeError::IndexLoad(error.to_string()))?;
    if has_unmerged_entries(&index) {
        return Err(MergeError::UnresolvedConflicts);
    }

    // rerere: the merge conflict is resolved — record its postimage so an
    // identical conflict is auto-resolved next time. A no-op unless
    // `rerere.enabled`. (`libra merge --continue` finalizes the merge here
    // without going through `commit`, so it needs its own hook.)
    if let Err(error) = crate::command::rerere::auto_update(false).await {
        tracing::warn!("rerere auto-update on merge --continue failed: {error}");
    }

    let orig_head = object_hash_from_state("orig_head", &state.orig_head)?;
    let target = object_hash_from_state("target", &state.target)?;
    let original_commit: Commit =
        load_object(&orig_head).map_err(|error| MergeError::CurrentLoad {
            commit_id: orig_head.to_string(),
            detail: error.to_string(),
        })?;
    let original_items = commit_tree_items(&original_commit)?;
    let index_items = index_tree_items(&index)?;
    let files_changed = count_item_map_changes(&original_items, &index_items);
    let tree_id = create_tree_from_items_map(&index_items).map_err(MergeError::TreeCreate)?;
    // Replay the message resolved at merge start (`-m` or the generated
    // default with the `merge.log` shortlog); states written by older
    // binaries carry no message and keep the plain form.
    let message = state
        .message
        .clone()
        .unwrap_or_else(|| format!("Merge {} into {}", state.target_ref, state.head_name));
    let merge_commit = Commit::from_tree_id(
        tree_id,
        vec![orig_head, target],
        &format_commit_msg(&message, None),
    );
    save_object(&merge_commit, &merge_commit.id)
        .map_err(|error| MergeError::CommitSave(error.to_string()))?;
    update_head_with_reflog(
        &state.head_name,
        merge_commit.id,
        &state.target_ref,
        "three-way",
    )
    .await?;
    reset_index_and_workdir_to_tree(&tree_id)?;
    MergeState::cleanup()?;
    // Merge concluded: re-apply the held autostash onto the finalized tree
    // (clean → dropped; conflict → promoted to the stash list with a notice).
    let autostash = resolve_pending_autostash(output).await;

    Ok(PullMergeSummary {
        strategy: "three-way".to_string(),
        old_commit: Some(orig_head.to_string()),
        commit: Some(merge_commit.id.to_string()),
        files_changed,
        up_to_date: false,
        parents: vec![orig_head.to_string(), target.to_string()],
        conflicted_paths: Vec::new(),
        aborted: false,
        continued: true,
        dry_run: false,
        would_conflict: false,
        autostash,
    })
}

fn ensure_no_unstaged_changes_for_continue() -> Result<(), PullMergeError> {
    let unstaged = status::changes_to_be_staged()
        .map_err(|error| PullMergeError::IndexLoad(error.to_string()))?;
    if !unstaged.modified.is_empty() || !unstaged.deleted.is_empty() {
        return Err(PullMergeError::DirtyWorktree);
    }
    Ok(())
}

/// Restore the pre-merge state recorded in `state`: HEAD back to `orig_head`
/// (reflog entry labelled with `policy`), index/worktree reset to the original
/// tree, and the merge state cleaned LAST — the crash-safe ordering shared by
/// `--abort` and `--restart` (a crash mid-way leaves a resumable/abortable
/// state, never a clean-looking tree with stale merge state).
async fn restore_pre_merge_state(
    state: &MergeState,
    policy: &str,
) -> Result<ObjectHash, MergeError> {
    let orig_head = object_hash_from_state("orig_head", &state.orig_head)?;
    update_head_with_reflog(&state.head_name, orig_head, &state.target_ref, policy).await?;
    let original_commit: Commit =
        load_object(&orig_head).map_err(|error| MergeError::CurrentLoad {
            commit_id: orig_head.to_string(),
            detail: error.to_string(),
        })?;
    reset_index_and_workdir_to_tree(&original_commit.tree_id)?;
    MergeState::cleanup()?;
    Ok(orig_head)
}

/// `merge --restart` (Libra extension, porting Lore's `branch merge restart`):
/// abort the in-progress conflicted merge — restoring the pre-merge HEAD,
/// index, and working tree exactly like `--abort`, DISCARDING any conflict
/// resolution done so far — then immediately re-run the same merge against the
/// RECORDED target commit (`state.target`, not the ref name, which may have
/// moved since the original merge), regenerating fresh conflict markers and
/// merge state. The re-run uses default merge options: the original
/// `-m`/`--no-ff`/`--squash`/`--no-commit` are not persisted in [`MergeState`]
/// and are not replayed (documented limitation).
async fn run_merge_restart(output: &OutputConfig) -> Result<MergeOutput, MergeError> {
    let state = MergeState::load_required()?;
    // A `--no-commit` merge also persists MergeState — with no conflicts.
    // Restarting it would silently discard the staged result and re-run with
    // default options (possibly fast-forwarding); refuse instead.
    if state.conflicted_paths.is_empty() {
        return Err(MergeError::RestartWithoutConflicts);
    }
    let target = state.target.clone();
    let target_ref = state.target_ref.clone();
    restore_pre_merge_state(&state, "restart").await?;
    // Deterministic replay: merge the recorded commit; keep the original ref
    // name as the upstream label so the merge message/state read naturally.
    // A held autostash survives the restart cycle: no NEW stash is taken
    // (autostash off) and the stale-sidecar recovery is skipped, so the
    // uniform finalize applies it on eventual clean completion or keeps
    // holding across a re-conflict.
    let options = PullMergeOptions {
        autostash: Some(false),
        preserve_held_autostash: true,
        ..PullMergeOptions::default()
    };
    run_merge_for_pull_with_options(&target, &target_ref, output, options).await
}

async fn run_merge_abort(output: &OutputConfig) -> Result<MergeOutput, MergeError> {
    let state = MergeState::load_required()?;
    let orig_head = restore_pre_merge_state(&state, "abort").await?;
    // The held autostash re-applies onto the restored pre-merge tree (clean
    // by construction — it was taken on that very tree; the conflict fallback
    // still guards the path).
    let autostash = resolve_pending_autostash(output).await;

    Ok(PullMergeSummary {
        strategy: "abort".to_string(),
        old_commit: Some(orig_head.to_string()),
        commit: Some(orig_head.to_string()),
        files_changed: 0,
        up_to_date: false,
        parents: Vec::new(),
        conflicted_paths: Vec::new(),
        aborted: true,
        continued: false,
        dry_run: false,
        would_conflict: false,
        autostash,
    })
}

async fn resolve_merge_target(target_ref: &str) -> Result<ObjectHash, Box<dyn std::error::Error>> {
    if let Some(remote) = target_ref.strip_prefix("refs/remotes/")
        && let Some((remote_name, _)) = remote.split_once('/')
        && let Some(branch) = Branch::find_branch_result(target_ref, Some(remote_name))
            .await
            .map_err(|error: BranchStoreError| Box::new(error) as Box<dyn std::error::Error>)?
    {
        return Ok(branch.commit);
    }

    get_target_commit(target_ref).await
}

fn lca_commit(lhs: &Commit, rhs: &Commit) -> Result<Option<Commit>, CliError> {
    let Some(base_id) = merge_base::merge_base(&lhs.id, &rhs.id).map_err(|error| {
        CliError::fatal(format!("failed to compute merge base: {error}"))
            .with_stable_code(StableErrorCode::RepoCorrupt)
    })?
    else {
        return Ok(None);
    };

    let base = load_object::<Commit>(&base_id).map_err(|error| {
        CliError::fatal(format!("failed to load merge base {base_id}: {error}"))
            .with_stable_code(StableErrorCode::RepoCorrupt)
    })?;
    Ok(Some(base))
}

async fn apply_fast_forward_merge(
    target_commit: Commit,
    target_branch_name: &str,
    output: &OutputConfig,
) -> Result<(), PullMergeError> {
    switch::ensure_clean_status(output)
        .await
        .map_err(|_| PullMergeError::DirtyWorktree)?;
    let target_items = commit_tree_items(&target_commit)?;
    let current_index =
        Index::load(path::index()).map_err(|error| PullMergeError::IndexLoad(error.to_string()))?;
    let paths_to_write: Vec<PathBuf> = target_items.keys().cloned().collect();
    ensure_no_untracked_conflicts(&current_index, &paths_to_write)?;

    let db = get_db_conn_instance().await;

    let old_oid_opt = Head::current_commit_result_with_conn(&db)
        .await
        .map_err(|e| PullMergeError::HeadResolve(e.to_string()))?;
    let current_head_state = Head::current_result_with_conn(&db)
        .await
        .map_err(|e| PullMergeError::HeadResolve(e.to_string()))?;

    let action = ReflogAction::Merge {
        branch: target_branch_name.to_string(),
        policy: "fast-forward".to_string(),
    };
    let context = ReflogContext {
        // If there was no previous commit, this is an initial commit merge (e.g., on an empty branch).
        // Use the zero-hash in that case.
        old_oid: old_oid_opt.map_or(ObjectHash::zero_str(get_hash_kind()).to_string(), |id| {
            id.to_string()
        }),
        new_oid: target_commit.id.to_string(),
        action,
    };

    // Use `with_reflog`. A merge operation should log for the branch.
    if let Err(e) = with_reflog(
        context,
        move |txn: &sea_orm::DatabaseTransaction| {
            Box::pin(async move {
                match &current_head_state {
                    Head::Branch(branch_name) => {
                        Branch::update_branch_with_conn(
                            txn,
                            branch_name,
                            &target_commit.id.to_string(),
                            None,
                        )
                        .await?;
                    }
                    Head::Detached(_) => {
                        // Merging into a detached HEAD is unusual but possible. We just move HEAD.
                        Head::update_with_conn(txn, Head::Detached(target_commit.id), None).await;
                    }
                }
                Ok(())
            })
        },
        true,
    )
    .await
    {
        return Err(PullMergeError::HeadUpdate(e.to_string()));
    }

    // Only restore the working directory *after* the pointers have been updated.
    restore::execute_safe(
        RestoreArgs {
            overlay: false,
            no_overlay: false,
            ours: false,
            theirs: false,
            ignore_unmerged: false,
            merge: false,
            conflict: None,
            worktree: true,
            staged: true,
            source: None, // `restore` without source defaults to HEAD, which is now correct.
            pathspec: vec![util::working_dir_string()],
            pathspec_from_file: None,
            pathspec_file_nul: false,
            no_progress: false,
        },
        &output.child_output_config(),
    )
    .await
    .map_err(|error| PullMergeError::Restore(error.to_string()))?;
    Ok(())
}

fn count_changed_files(
    current_commit: Option<&Commit>,
    target_commit: &Commit,
) -> Result<usize, PullMergeError> {
    let target_items = commit_tree_items(target_commit)?;
    let current_items = match current_commit {
        Some(commit) => commit_tree_items(commit)?,
        None => HashMap::new(),
    };

    let mut paths: HashSet<PathBuf> = current_items.keys().cloned().collect();
    paths.extend(target_items.keys().cloned());

    Ok(paths
        .into_iter()
        .filter(|path| current_items.get(path) != target_items.get(path))
        .count())
}

fn commit_tree_items(commit: &Commit) -> Result<HashMap<PathBuf, MergeTreeEntry>, PullMergeError> {
    let tree: Tree = load_object(&commit.tree_id).map_err(|error| PullMergeError::TreeLoad {
        tree_id: commit.tree_id.to_string(),
        detail: error.to_string(),
    })?;
    Ok(tree
        .get_plain_items_with_mode()
        .into_iter()
        .filter_map(|(path, hash, mode)| {
            if mode == TreeItemMode::Commit {
                None
            } else {
                Some((path, MergeTreeEntry { hash, mode }))
            }
        })
        .collect())
}

async fn current_head_name() -> Result<String, PullMergeError> {
    Head::current_result()
        .await
        .map_err(|error| PullMergeError::HeadResolve(error.to_string()))
        .map(|head| match head {
            Head::Branch(name) => name,
            Head::Detached(_) => "HEAD".to_string(),
        })
}

async fn update_head_with_reflog(
    head_name: &str,
    new_oid: ObjectHash,
    target_branch_name: &str,
    policy: &str,
) -> Result<(), PullMergeError> {
    let db = get_db_conn_instance().await;
    let old_oid_opt = Head::current_commit_result_with_conn(&db)
        .await
        .map_err(|error| PullMergeError::HeadResolve(error.to_string()))?;
    let action = ReflogAction::Merge {
        branch: target_branch_name.to_string(),
        policy: policy.to_string(),
    };
    let context = ReflogContext {
        old_oid: old_oid_opt.map_or(ObjectHash::zero_str(get_hash_kind()).to_string(), |id| {
            id.to_string()
        }),
        new_oid: new_oid.to_string(),
        action,
    };

    let head_name = head_name.to_string();
    with_reflog(
        context,
        move |txn: &sea_orm::DatabaseTransaction| {
            let head_name = head_name.clone();
            Box::pin(async move {
                if head_name == "HEAD" {
                    Head::update_with_conn(txn, Head::Detached(new_oid), None).await;
                } else {
                    Branch::update_branch_with_conn(txn, &head_name, &new_oid.to_string(), None)
                        .await?;
                }
                Ok(())
            })
        },
        true,
    )
    .await
    .map_err(|error| PullMergeError::HeadUpdate(error.to_string()))
}

fn object_hash_from_state(field: &str, value: &str) -> Result<ObjectHash, PullMergeError> {
    ObjectHash::from_str(value)
        .map_err(|error| PullMergeError::StateLoad(format!("invalid {field} '{value}': {error}")))
}

#[derive(Debug, Copy, Clone)]
enum MergeResolution {
    Use(MergeTreeEntry),
    Delete,
    Conflict(ConflictKind),
}

#[derive(Debug, Copy, Clone)]
enum ConflictKind {
    BothChanged {
        /// Common-ancestor blob (`None` for an add/add conflict with no base),
        /// used to compute line-level conflict hunks like Git rather than
        /// wrapping the whole file in one conflict region.
        base: Option<ObjectHash>,
        ours: ObjectHash,
        theirs: ObjectHash,
    },
    OursModifiedTheirsDeleted {
        ours: ObjectHash,
    },
    TheirsModifiedOursDeleted {
        theirs: ObjectHash,
    },
}

#[derive(Debug, Copy, Clone)]
enum RelativeState {
    Same(MergeTreeEntry),
    Modified(MergeTreeEntry),
    Deleted,
    Added(MergeTreeEntry),
    Missing,
}

fn classify_relative_to_base(
    base: Option<&MergeTreeEntry>,
    side: Option<&MergeTreeEntry>,
) -> RelativeState {
    match (base, side) {
        (Some(base), Some(side)) if base == side => RelativeState::Same(*side),
        (Some(_), Some(side)) => RelativeState::Modified(*side),
        (Some(_), None) => RelativeState::Deleted,
        (None, Some(side)) => RelativeState::Added(*side),
        (None, None) => RelativeState::Missing,
    }
}

fn resolve_three_way(
    base: Option<&MergeTreeEntry>,
    ours: Option<&MergeTreeEntry>,
    theirs: Option<&MergeTreeEntry>,
    persist_merged_blobs: bool,
) -> Result<MergeResolution, PullMergeError> {
    let base_present = base.is_some();
    let ours_state = classify_relative_to_base(base, ours);
    let theirs_state = classify_relative_to_base(base, theirs);

    Ok(match (base_present, ours_state, theirs_state) {
        (false, RelativeState::Missing, RelativeState::Missing) => MergeResolution::Delete,
        (false, RelativeState::Added(ours), RelativeState::Missing) => MergeResolution::Use(ours),
        (false, RelativeState::Missing, RelativeState::Added(theirs)) => {
            MergeResolution::Use(theirs)
        }
        (false, RelativeState::Added(ours), RelativeState::Added(theirs)) => {
            if ours == theirs {
                MergeResolution::Use(theirs)
            } else {
                MergeResolution::Conflict(ConflictKind::BothChanged {
                    base: None,
                    ours: ours.hash,
                    theirs: theirs.hash,
                })
            }
        }
        (true, RelativeState::Same(ours), RelativeState::Same(_)) => MergeResolution::Use(ours),
        (true, RelativeState::Same(_), RelativeState::Modified(theirs)) => {
            MergeResolution::Use(theirs)
        }
        (true, RelativeState::Modified(ours), RelativeState::Same(_)) => MergeResolution::Use(ours),
        (true, RelativeState::Modified(ours), RelativeState::Modified(theirs)) => {
            if ours == theirs {
                MergeResolution::Use(theirs)
            } else if let Some(base) = base
                && let Some(merged) =
                    try_merge_blob_contents(base, ours, theirs, persist_merged_blobs)?
            {
                MergeResolution::Use(merged)
            } else {
                MergeResolution::Conflict(ConflictKind::BothChanged {
                    base: base.map(|b| b.hash),
                    ours: ours.hash,
                    theirs: theirs.hash,
                })
            }
        }
        (true, RelativeState::Deleted, RelativeState::Same(_)) => MergeResolution::Delete,
        (true, RelativeState::Same(_), RelativeState::Deleted) => MergeResolution::Delete,
        (true, RelativeState::Deleted, RelativeState::Deleted) => MergeResolution::Delete,
        (true, RelativeState::Deleted, RelativeState::Modified(theirs)) => {
            MergeResolution::Conflict(ConflictKind::TheirsModifiedOursDeleted {
                theirs: theirs.hash,
            })
        }
        (true, RelativeState::Modified(ours), RelativeState::Deleted) => {
            MergeResolution::Conflict(ConflictKind::OursModifiedTheirsDeleted { ours: ours.hash })
        }
        _ => MergeResolution::Delete,
    })
}

fn try_merge_blob_contents(
    base: &MergeTreeEntry,
    ours: MergeTreeEntry,
    theirs: MergeTreeEntry,
    persist: bool,
) -> Result<Option<MergeTreeEntry>, PullMergeError> {
    if base.mode != ours.mode
        || base.mode != theirs.mode
        || !matches!(base.mode, TreeItemMode::Blob | TreeItemMode::BlobExecutable)
    {
        return Ok(None);
    }

    let base_blob = load_merge_blob(base.hash)?;
    let ours_blob = load_merge_blob(ours.hash)?;
    let theirs_blob = load_merge_blob(theirs.hash)?;

    let Ok(merged_bytes) = diffy::merge_bytes(&base_blob.data, &ours_blob.data, &theirs_blob.data)
    else {
        return Ok(None);
    };

    let merged_blob = Blob::from_content_bytes(merged_bytes);
    // `--dry-run` (persist=false): the merged OID is computed in memory only —
    // persisting here would write the object store (and, under tiered storage,
    // upload to the durable tier) from a preview.
    if persist {
        save_object(&merged_blob, &merged_blob.id).map_err(|error| {
            PullMergeError::TreeCreate(format!(
                "failed to save auto-merged blob {}: {error}",
                merged_blob.id
            ))
        })?;
    }

    Ok(Some(MergeTreeEntry {
        hash: merged_blob.id,
        mode: ours.mode,
    }))
}

fn load_merge_blob(hash: ObjectHash) -> Result<Blob, PullMergeError> {
    load_object(&hash).map_err(|error| PullMergeError::ObjectLoad {
        object_id: hash.to_string(),
        detail: error.to_string(),
    })
}

fn merge_tree_items(
    base_items: &HashMap<PathBuf, MergeTreeEntry>,
    our_items: &HashMap<PathBuf, MergeTreeEntry>,
    their_items: &HashMap<PathBuf, MergeTreeEntry>,
    persist_merged_blobs: bool,
) -> Result<ThreeWayMergeResult, PullMergeError> {
    let mut all_paths: HashSet<PathBuf> = base_items.keys().cloned().collect();
    all_paths.extend(our_items.keys().cloned());
    all_paths.extend(their_items.keys().cloned());

    let mut merged_items = HashMap::new();
    let mut conflicts = Vec::new();
    for path in all_paths {
        match resolve_three_way(
            base_items.get(&path),
            our_items.get(&path),
            their_items.get(&path),
            persist_merged_blobs,
        )? {
            MergeResolution::Use(hash) => {
                merged_items.insert(path, hash);
            }
            MergeResolution::Delete => {}
            MergeResolution::Conflict(kind) => conflicts.push((path, kind)),
        }
    }

    Ok(ThreeWayMergeResult {
        merged_items,
        conflicts,
    })
}

fn count_item_map_changes(
    before: &HashMap<PathBuf, MergeTreeEntry>,
    after: &HashMap<PathBuf, MergeTreeEntry>,
) -> usize {
    let mut paths: HashSet<PathBuf> = before.keys().cloned().collect();
    paths.extend(after.keys().cloned());
    paths
        .into_iter()
        .filter(|path| before.get(path) != after.get(path))
        .count()
}

fn add_blob_index_entry(
    index: &mut Index,
    path: &Path,
    item: MergeTreeEntry,
    stage: u8,
) -> Result<(), PullMergeError> {
    let blob: Blob = load_object(&item.hash).map_err(|error| {
        PullMergeError::IndexSave(format!(
            "failed to load blob {} for index entry '{}': {error}",
            item.hash,
            path.display()
        ))
    })?;
    let mut entry = IndexEntry::new_from_blob(
        path_to_index_key(path)?.to_string(),
        item.hash,
        blob.data.len() as u32,
    );
    entry.mode = tree_item_mode_to_index_mode(item.mode)?;
    entry.flags.stage = stage;
    index.add(entry);
    Ok(())
}

fn ensure_no_untracked_conflicts(
    current_index: &Index,
    paths: &[PathBuf],
) -> Result<(), PullMergeError> {
    let untracked_paths =
        worktree::untracked_workdir_paths(current_index).map_err(PullMergeError::IndexLoad)?;
    for untracked in &untracked_paths {
        for path in paths {
            if worktree::paths_conflict(untracked, path) {
                return Err(PullMergeError::UntrackedOverwrite {
                    path: untracked.display().to_string(),
                });
            }
        }
    }
    Ok(())
}

fn write_workdir_file(workdir: &Path, relative: &Path, content: &[u8]) -> Result<(), String> {
    let file_path = workdir.join(relative);
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    fs::write(&file_path, content)
        .map_err(|error| format!("failed to write {}: {error}", file_path.display()))
}

fn conflict_marker_eol() -> &'static str {
    if cfg!(windows) { "\r\n" } else { "\n" }
}

fn conflict_payload(content: &[u8]) -> Cow<'_, str> {
    match std::str::from_utf8(content) {
        Ok(text) => Cow::Borrowed(text),
        Err(_) => Cow::Owned(format!("[binary content, {} bytes]", content.len())),
    }
}

fn write_conflict_markers(
    workdir: &Path,
    path: &Path,
    marker_eol: &str,
    commit_abbrev: &str,
    kind: ConflictKind,
    conflict_style: diffy::ConflictStyle,
) -> Result<(), String> {
    let content: Vec<u8> = match kind {
        ConflictKind::BothChanged { base, ours, theirs } => {
            let ours_blob: Blob = load_object(&ours).map_err(|error| error.to_string())?;
            let theirs_blob: Blob = load_object(&theirs).map_err(|error| error.to_string())?;
            both_changed_conflict_content(
                base,
                &ours_blob.data,
                &theirs_blob.data,
                marker_eol,
                commit_abbrev,
                conflict_style,
            )?
        }
        ConflictKind::OursModifiedTheirsDeleted { ours } => {
            let ours_blob: Blob = load_object(&ours).map_err(|error| error.to_string())?;
            format!(
                "<<<<<<< HEAD{marker_eol}{}{marker_eol}======={marker_eol}>>>>>>> {} (deleted){marker_eol}",
                conflict_payload(&ours_blob.data),
                commit_abbrev
            )
            .into_bytes()
        }
        ConflictKind::TheirsModifiedOursDeleted { theirs } => {
            let theirs_blob: Blob = load_object(&theirs).map_err(|error| error.to_string())?;
            format!(
                "<<<<<<< HEAD (deleted){marker_eol}======={marker_eol}{}{marker_eol}>>>>>>> {}{marker_eol}",
                conflict_payload(&theirs_blob.data),
                commit_abbrev
            )
            .into_bytes()
        }
    };
    write_workdir_file(workdir, path, &content)
}

/// Build the worktree content for a both-modified conflict.
///
/// When all three sides are UTF-8 text, this runs a line-level three-way merge
/// (`diffy` with Git's two-marker `merge` conflict style) so the conflict
/// markers enclose only the diverging hunks — matching Git — instead of wrapping
/// each whole file in a single conflict region. A missing base (an add/add
/// conflict) is treated as an empty common ancestor and still merges line-level.
/// Binary content falls back to whole-file markers, where a line-level merge
/// would be meaningless; an unreadable base blob is a hard error (propagated),
/// not a silent fallback.
fn both_changed_conflict_content(
    base: Option<ObjectHash>,
    ours: &[u8],
    theirs: &[u8],
    marker_eol: &str,
    commit_abbrev: &str,
    conflict_style: diffy::ConflictStyle,
) -> Result<Vec<u8>, String> {
    let whole_file = || {
        format!(
            "<<<<<<< HEAD{marker_eol}{}{marker_eol}======={marker_eol}{}{marker_eol}>>>>>>> {}{marker_eol}",
            conflict_payload(ours),
            conflict_payload(theirs),
            commit_abbrev
        )
        .into_bytes()
    };

    // Load the common-ancestor content (if any) and defer to the shared
    // line-level renderer; fall back to whole-file markers for binary sides.
    let base_data: Option<Vec<u8>> = match base {
        Some(base) => {
            let base_blob: Blob = load_object(&base).map_err(|error| error.to_string())?;
            Some(base_blob.data)
        }
        None => None,
    };
    Ok(render_line_level_conflict(
        base_data.as_deref(),
        ours,
        theirs,
        commit_abbrev,
        conflict_style,
    )
    .unwrap_or_else(whole_file))
}

/// Render a both-modified conflict as a line-level three-way merge, matching
/// Git: the conflict markers enclose only the diverging hunks (lines shared by
/// both sides stay outside the markers) instead of wrapping each whole file in a
/// single conflict region. Shared by `merge`/`pull` (here) and `cherry-pick`.
///
/// Returns `None` when a line-level merge is not applicable — any side is not
/// UTF-8 text (binary), or the content merged with no real text conflict — so
/// the caller can fall back to its whole-file presentation. `base` is the
/// common-ancestor content (`None` for an add/add conflict with no base).
/// `commit_label` is the `>>>>>>>` side label (e.g. the other commit's
/// abbreviation).
pub(crate) fn render_line_level_conflict(
    base: Option<&[u8]>,
    ours: &[u8],
    theirs: &[u8],
    commit_label: &str,
    conflict_style: diffy::ConflictStyle,
) -> Option<Vec<u8>> {
    if std::str::from_utf8(ours).is_err()
        || std::str::from_utf8(theirs).is_err()
        || base.is_some_and(|b| std::str::from_utf8(b).is_err())
    {
        return None;
    }

    // Choose a marker length long enough that no line in the inputs can be
    // mistaken for (and then wrongly relabelled as) a generated marker — Git's
    // conflict-marker-size bumping. With this length the relabel below matches
    // only `diffy`'s emitted markers.
    let marker_len = conflict_marker_length(&[base.unwrap_or(&[]), ours, theirs]);
    let mut options = diffy::MergeOptions::new();
    options.set_conflict_style(conflict_style);
    options.set_conflict_marker_length(marker_len);
    match options.merge_bytes(base.unwrap_or(&[]), ours, theirs) {
        // A genuine conflict: `diffy` returns the file with line-level markers
        // labelled `ours`/`theirs`; relabel them to Git's `HEAD`/<commit>.
        Err(conflicted) => Some(relabel_conflict_markers(
            conflicted,
            marker_len,
            commit_label,
        )),
        // Content merged cleanly with no markers (no real text conflict — e.g. a
        // mode-only divergence): let the caller surface it as a whole-file
        // conflict rather than writing the silently-merged text.
        Ok(_) => None,
    }
}

/// The conflict-marker length to use, mirroring Git: the default of 7, bumped to
/// one longer than the longest run of leading conflict-marker characters
/// (`<` `>` `=` `|`) on any line of the inputs, so a content line that itself
/// looks like a marker is never confused with a generated one.
fn conflict_marker_length(sides: &[&[u8]]) -> usize {
    const DEFAULT_MARKER_LENGTH: usize = 7;
    let mut longest = 0usize;
    for side in sides {
        for line in side.split(|&b| b == b'\n') {
            let Some(&first) = line.first() else { continue };
            if matches!(first, b'<' | b'>' | b'=' | b'|') {
                let run = line.iter().take_while(|&&b| b == first).count();
                if run >= DEFAULT_MARKER_LENGTH {
                    longest = longest.max(run);
                }
            }
        }
    }
    if longest >= DEFAULT_MARKER_LENGTH {
        longest + 1
    } else {
        DEFAULT_MARKER_LENGTH
    }
}

/// Rewrite `diffy`'s conflict-marker labels (`ours` / `theirs`) to Git's
/// (`HEAD` / the other side's abbreviation).
///
/// Matches WHOLE LINES only: a line is relabelled exactly when it equals the
/// generated marker (`{marker} ours` / `{marker} theirs`). Combined with the
/// [`conflict_marker_length`] bump (which guarantees no input line *starts* with
/// that many markers), this leaves any content that merely *contains* a
/// marker-like substring — e.g. `prefix <<<<<<< ours` — untouched.
fn relabel_conflict_markers(conflicted: Vec<u8>, marker_len: usize, commit_label: &str) -> Vec<u8> {
    let open = "<".repeat(marker_len);
    let close = ">".repeat(marker_len);
    let bars = "|".repeat(marker_len);
    let ours_marker = format!("{open} ours");
    let theirs_marker = format!("{close} theirs");
    // `diffy`'s diff3 base marker; only emitted under ConflictStyle::Diff3.
    let original_marker = format!("{bars} original");
    let head_marker = format!("{open} HEAD");
    let label_marker = format!("{close} {commit_label}");
    // Match the `||||||| base` label convention `restore --conflict=diff3` uses.
    let base_marker = format!("{bars} base");

    let text = String::from_utf8_lossy(&conflicted);
    // `split('\n')` + `join('\n')` round-trips exactly (including a trailing
    // newline, which yields a final empty segment that re-joins cleanly).
    let relabelled = text
        .split('\n')
        .map(|line| {
            if line == ours_marker {
                head_marker.as_str()
            } else if line == theirs_marker {
                label_marker.as_str()
            } else if line == original_marker {
                base_marker.as_str()
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    relabelled.into_bytes()
}

fn index_tree_items(index: &Index) -> Result<HashMap<PathBuf, MergeTreeEntry>, PullMergeError> {
    let mut items = HashMap::new();
    for path in index.tracked_files() {
        if let Some(entry) = index.get(path_to_index_key(&path)?, 0) {
            items.insert(
                path,
                MergeTreeEntry {
                    hash: entry.hash,
                    mode: index_mode_to_tree_item_mode(entry.mode)?,
                },
            );
        }
    }
    Ok(items)
}

pub(crate) fn create_tree_from_items_map(
    items: &HashMap<PathBuf, MergeTreeEntry>,
) -> Result<ObjectHash, String> {
    // Delegate to the shared nested-tree builder so merge, cherry-pick, and
    // `write-tree` share one tree-construction rule (and one bug-fix surface).
    // Merge entries already carry a `TreeItemMode`, so they map straight onto
    // the builder's leaf tuples.
    let leaves = items
        .iter()
        .map(|(path, entry)| (path.clone(), entry.mode, entry.hash));
    tree_plumbing::write_tree_from_leaves(leaves).map_err(|error| error.to_string())
}

fn reset_index_and_workdir_to_tree(tree_id: &ObjectHash) -> Result<(), PullMergeError> {
    let tree: Tree = load_object(tree_id).map_err(|error| PullMergeError::TreeLoad {
        tree_id: tree_id.to_string(),
        detail: error.to_string(),
    })?;
    let current_index =
        Index::load(path::index()).map_err(|error| PullMergeError::IndexLoad(error.to_string()))?;
    let mut new_index = Index::new();
    reset::rebuild_index_from_tree(&tree, &mut new_index, "")
        .map_err(PullMergeError::TreeCreate)?;
    reset_workdir_tracked_only(&current_index, &new_index)?;
    new_index
        .save(path::index())
        .map_err(|error| PullMergeError::IndexSave(error.to_string()))
}

fn reset_workdir_tracked_only(
    current_index: &Index,
    new_index: &Index,
) -> Result<(), PullMergeError> {
    let workdir = util::working_dir();
    let untracked_paths =
        worktree::untracked_workdir_paths(current_index).map_err(PullMergeError::IndexLoad)?;
    if let Some(conflict) = worktree::untracked_overwrite_path(&untracked_paths, new_index) {
        return Err(PullMergeError::UntrackedOverwrite {
            path: conflict.display().to_string(),
        });
    }

    let new_tracked_paths: HashSet<_> = new_index.tracked_files().into_iter().collect();
    for path_buf in current_index.tracked_files() {
        if !new_tracked_paths.contains(&path_buf) {
            let full_path = workdir.join(path_buf);
            if full_path.exists() {
                fs::remove_file(&full_path).map_err(|error| {
                    PullMergeError::WorkdirReset(format!("failed to remove file: {error}"))
                })?;
            }
        }
    }

    for path_buf in new_index.tracked_files() {
        if let Some(entry) = new_index.get(path_to_index_key(&path_buf)?, 0) {
            let blob: Blob = load_object(&entry.hash).map_err(|error| {
                PullMergeError::WorkdirReset(format!(
                    "failed to load blob {} for '{}': {error}",
                    entry.hash,
                    path_buf.display()
                ))
            })?;
            write_workdir_file(&workdir, &path_buf, &blob.data)
                .map_err(PullMergeError::WorkdirReset)?;
        }
    }
    Ok(())
}

fn has_unmerged_entries(index: &Index) -> bool {
    !unresolved_conflicted_paths(index, &[]).is_empty()
}

pub(crate) fn unresolved_conflicted_paths(
    index: &Index,
    conflicted_paths: &[String],
) -> Vec<String> {
    let resolved: HashSet<String> = index
        .tracked_entries(0)
        .into_iter()
        .map(|entry| entry.name.clone())
        .collect();
    let staged_conflicts = staged_conflict_paths(index);
    let mut paths: Vec<String> = if conflicted_paths.is_empty() {
        staged_conflicts.into_iter().collect()
    } else {
        conflicted_paths
            .iter()
            .filter(|path| staged_conflicts.contains(path.as_str()))
            .cloned()
            .collect()
    };
    paths.retain(|path| !resolved.contains(path.as_str()));
    paths.sort();
    paths
}

fn staged_conflict_paths(index: &Index) -> HashSet<String> {
    (1..=3)
        .flat_map(|stage| index.tracked_entries(stage))
        .map(|entry| entry.name.clone())
        .collect()
}

fn path_to_index_key(path: &Path) -> Result<&str, PullMergeError> {
    path.to_str().ok_or_else(|| {
        PullMergeError::IndexSave(format!("path is not valid UTF-8: {}", path.display()))
    })
}

fn tree_item_mode_to_index_mode(mode: TreeItemMode) -> Result<u32, PullMergeError> {
    match mode {
        TreeItemMode::Blob => Ok(0o100644),
        TreeItemMode::BlobExecutable => Ok(0o100755),
        TreeItemMode::Link => Ok(0o120000),
        TreeItemMode::Tree => Err(PullMergeError::IndexSave(
            "tree entry cannot be represented as a file index entry".to_string(),
        )),
        TreeItemMode::Commit => Err(PullMergeError::IndexSave(
            "gitlink entries are not supported by merge".to_string(),
        )),
    }
}

fn index_mode_to_tree_item_mode(mode: u32) -> Result<TreeItemMode, PullMergeError> {
    match mode {
        0o100644 => Ok(TreeItemMode::Blob),
        0o100755 => Ok(TreeItemMode::BlobExecutable),
        0o120000 => Ok(TreeItemMode::Link),
        other => Err(PullMergeError::TreeCreate(format!(
            "unsupported index mode {other:o} while creating merge tree"
        ))),
    }
}

fn short_object_id(object_id: &ObjectHash) -> String {
    let object_id = object_id.to_string();
    object_id.chars().take(7).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merge_entry(byte: u8, mode: TreeItemMode) -> MergeTreeEntry {
        MergeTreeEntry {
            hash: ObjectHash::new(&[byte; 20]),
            mode,
        }
    }

    #[test]
    fn render_line_level_conflict_isolates_diverging_hunk() {
        let base = b"top\nl1\nl2\nl3\nbottom\n";
        let ours = b"top\nl1\nMAIN\nl3\nbottom\n";
        let theirs = b"top\nl1\nOTHER\nl3\nbottom\n";
        let out = render_line_level_conflict(
            Some(base),
            ours,
            theirs,
            "abc1234",
            diffy::ConflictStyle::Merge,
        )
        .expect("a real text conflict renders line-level markers");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "top\nl1\n<<<<<<< HEAD\nMAIN\n=======\nOTHER\n>>>>>>> abc1234\nl3\nbottom\n",
            "only the diverging line is enclosed; shared context stays outside"
        );
    }

    #[test]
    fn render_line_level_conflict_does_not_corrupt_marker_like_content() {
        // A shared line that itself looks like a conflict marker must survive
        // verbatim: the generated markers are bumped to 8 chars, so the 7-char
        // content line is neither treated as a marker nor relabelled.
        let base = b"<<<<<<< ours\nl2\n";
        let ours = b"<<<<<<< ours\nMAIN\n";
        let theirs = b"<<<<<<< ours\nOTHER\n";
        let out = render_line_level_conflict(
            Some(base),
            ours,
            theirs,
            "abc1234",
            diffy::ConflictStyle::Merge,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.starts_with("<<<<<<< ours\n"),
            "the literal marker-like content line is preserved verbatim: {text:?}"
        );
        assert!(
            text.contains("<<<<<<<< HEAD\n") && text.contains(">>>>>>>> abc1234\n"),
            "generated markers are bumped to 8 chars so they cannot collide: {text:?}"
        );
        // The marker-like content line keeps its original ` ours` label — a naive
        // 7-char relabel would have rewritten it to `<<<<<<< HEAD`.
        assert!(
            text.contains("<<<<<<< ours\n"),
            "the 7-char content line was preserved, not relabelled: {text:?}"
        );
    }

    #[test]
    fn render_line_level_conflict_preserves_non_leading_marker_substring() {
        // A shared line that merely CONTAINS a marker-like substring (not at the
        // start of the line, so it does not bump the marker length) must survive
        // verbatim — only complete generated marker lines are relabelled.
        let base = b"prefix <<<<<<< ours\nl2\n";
        let ours = b"prefix <<<<<<< ours\nMAIN\n";
        let theirs = b"prefix <<<<<<< ours\nOTHER\n";
        let out = render_line_level_conflict(
            Some(base),
            ours,
            theirs,
            "abc1234",
            diffy::ConflictStyle::Merge,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.starts_with("prefix <<<<<<< ours\n"),
            "the mid-line marker-like content is preserved, not relabelled: {text:?}"
        );
        assert!(
            text.contains("<<<<<<< HEAD\n") && text.contains(">>>>>>> abc1234\n"),
            "the generated 7-char markers are relabelled normally: {text:?}"
        );
        assert!(
            !text.contains("prefix <<<<<<< HEAD"),
            "the marker-like substring was NOT rewritten to HEAD: {text:?}"
        );
    }

    #[test]
    fn render_line_level_conflict_skips_binary_and_clean_merges() {
        // Binary side -> None (caller falls back to whole-file markers).
        assert!(
            render_line_level_conflict(
                None,
                b"a\n",
                &[0xff, 0xfe],
                "x",
                diffy::ConflictStyle::Merge
            )
            .is_none()
        );
        // No real text conflict (only one side changed) -> None.
        assert!(
            render_line_level_conflict(
                Some(b"a\n"),
                b"a\n",
                b"b\n",
                "x",
                diffy::ConflictStyle::Merge
            )
            .is_none()
        );
    }

    #[test]
    fn render_line_level_conflict_diff3_emits_base_block() {
        // `merge.conflictStyle = diff3`: the common-ancestor content appears
        // between a `||||||| base` marker and the `=======` separator.
        let base = b"top\nl1\nORIG\nl3\nbottom\n";
        let ours = b"top\nl1\nMAIN\nl3\nbottom\n";
        let theirs = b"top\nl1\nOTHER\nl3\nbottom\n";
        let out = render_line_level_conflict(
            Some(base),
            ours,
            theirs,
            "abc1234",
            diffy::ConflictStyle::Diff3,
        )
        .expect("a real text conflict renders line-level markers");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "top\nl1\n<<<<<<< HEAD\nMAIN\n||||||| base\nORIG\n=======\nOTHER\n>>>>>>> abc1234\nl3\nbottom\n",
            "diff3 adds the base block, relabelled from diffy's `original` to `base`"
        );
    }

    #[test]
    fn render_line_level_conflict_diff3_does_not_corrupt_base_marker_like_content() {
        // A shared content line that looks like the diff3 base marker must
        // survive verbatim: markers are bumped past it, and only the generated
        // (bumped) `|||||||| original` line is relabelled.
        let base = b"||||||| original\nORIG\n";
        let ours = b"||||||| original\nMAIN\n";
        let theirs = b"||||||| original\nOTHER\n";
        let out = render_line_level_conflict(
            Some(base),
            ours,
            theirs,
            "abc1234",
            diffy::ConflictStyle::Diff3,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.starts_with("||||||| original\n"),
            "the literal base-marker-like content line is preserved verbatim: {text:?}"
        );
        assert!(
            text.contains("|||||||| base\n"),
            "the generated (8-char, bumped) base marker is relabelled to `base`: {text:?}"
        );
    }

    #[test]
    fn merge_args_parse_ff_flags() {
        let no_ff = MergeArgs::try_parse_from(["merge", "--no-ff", "feature"]).unwrap();
        assert!(no_ff.no_ff);
        assert!(!no_ff.ff_only);
        assert_eq!(no_ff.branch.as_deref(), Some("feature"));

        let ff_only = MergeArgs::try_parse_from(["merge", "--ff-only", "feature"]).unwrap();
        assert!(ff_only.ff_only);
        assert!(!ff_only.no_ff);

        let with_msg = MergeArgs::try_parse_from(["merge", "-m", "custom", "feature"]).unwrap();
        assert_eq!(with_msg.message.as_deref(), Some("custom"));

        let squash = MergeArgs::try_parse_from(["merge", "--squash", "feature"]).unwrap();
        assert!(squash.squash);
        let no_commit = MergeArgs::try_parse_from(["merge", "--no-commit", "feature"]).unwrap();
        assert!(no_commit.no_commit);
        // --squash and --no-commit are mutually exclusive.
        assert!(
            MergeArgs::try_parse_from(["merge", "--squash", "--no-commit", "feature"]).is_err()
        );
    }

    #[test]
    fn merge_args_ff_only_conflicts_with_no_ff() {
        let err = MergeArgs::try_parse_from(["merge", "--ff-only", "--no-ff", "feature"])
            .expect_err("--ff-only and --no-ff are mutually exclusive");
        assert!(err.to_string().contains("cannot be used with"));
    }

    /// Pin the `Display` format for every variant of [`PullMergeError`]
    /// (also exposed as `MergeError`). These strings are used as the
    /// CliError message via `From<PullMergeError> for CliError` and
    /// surface in both human and `--json` envelopes for `merge` and
    /// the merge phase of `pull`.
    #[test]
    fn pull_merge_error_display_pins_each_variant() {
        assert_eq!(
            PullMergeError::InvalidTarget("a/b".to_string()).to_string(),
            "a/b - not something we can merge",
        );
        assert_eq!(
            PullMergeError::InvalidConflictStyle("zdiff3".to_string()).to_string(),
            "unsupported merge.conflictStyle 'zdiff3' (expected 'merge' or 'diff3')",
        );
        assert_eq!(
            PullMergeError::ConflictStyleRead("db locked".to_string()).to_string(),
            "failed to read merge.conflictStyle config: db locked",
        );
        assert_eq!(
            PullMergeError::RestartWithoutConflicts.to_string(),
            "no conflicted merge to restart (the in-progress merge has no conflicts)",
        );
        assert_eq!(
            PullMergeError::TargetLoad {
                commit_id: "deadbeef".to_string(),
                detail: "object not found".to_string(),
            }
            .to_string(),
            "failed to load merge target 'deadbeef': object not found",
        );
        assert_eq!(
            PullMergeError::CurrentLoad {
                commit_id: "feedface".to_string(),
                detail: "io error".to_string(),
            }
            .to_string(),
            "failed to load current commit 'feedface': io error",
        );
        assert_eq!(
            PullMergeError::History("walk failed".to_string()).to_string(),
            "failed to inspect merge history: walk failed",
        );
        assert_eq!(
            PullMergeError::UnrelatedHistories.to_string(),
            "refusing to merge unrelated histories",
        );
        assert_eq!(
            PullMergeError::UnsignedMergeCommit {
                commit: "abc1234".to_string(),
            }
            .to_string(),
            "commit abc1234 does not have a GPG signature",
        );
        assert_eq!(
            PullMergeError::BadMergeSignature {
                commit: "def5678".to_string(),
            }
            .to_string(),
            "commit def5678 has a bad GPG signature",
        );
        assert_eq!(
            PullMergeError::SignatureCheck("vault sealed".to_string()).to_string(),
            "failed to verify the signature of the merged commit: vault sealed",
        );
        assert_eq!(
            PullMergeError::NonFastForward {
                current: "1111111".to_string(),
                target: "2222222".to_string(),
            }
            .to_string(),
            "non-fast-forward merge refused (current 1111111, target 2222222)",
        );
        assert_eq!(
            PullMergeError::TreeLoad {
                tree_id: "abc123".to_string(),
                detail: "decode failed".to_string(),
            }
            .to_string(),
            "failed to load tree 'abc123': decode failed",
        );
        assert_eq!(
            PullMergeError::ObjectLoad {
                object_id: "def456".to_string(),
                detail: "blob missing".to_string(),
            }
            .to_string(),
            "failed to load object 'def456': blob missing",
        );
        assert_eq!(
            PullMergeError::HeadResolve("db locked".to_string()).to_string(),
            "failed to resolve HEAD state: db locked",
        );
        assert_eq!(
            PullMergeError::HeadUpdate("write failed".to_string()).to_string(),
            "failed to update HEAD during merge: write failed",
        );
        assert_eq!(
            PullMergeError::Restore("checkout failed".to_string()).to_string(),
            "failed to restore working tree after merge: checkout failed",
        );
    }

    #[test]
    fn merge_tree_items_preserves_mode_from_changed_side() {
        let path = PathBuf::from("script.sh");
        let base = merge_entry(1, TreeItemMode::Blob);
        let theirs = merge_entry(2, TreeItemMode::BlobExecutable);
        let mut base_items = HashMap::new();
        base_items.insert(path.clone(), base);
        let mut our_items = HashMap::new();
        our_items.insert(path.clone(), base);
        let mut their_items = HashMap::new();
        their_items.insert(path.clone(), theirs);

        let result = merge_tree_items(&base_items, &our_items, &their_items, true)
            .expect("merge tree items");

        assert!(result.conflicts.is_empty());
        assert_eq!(result.merged_items.get(&path), Some(&theirs));
    }
}
