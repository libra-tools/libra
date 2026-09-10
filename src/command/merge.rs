//! Merge command orchestration that resolves base/target commits, performs recursive merge, stages results, and updates refs or surfaces conflicts.

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    str::FromStr,
    sync::{Arc, Mutex},
};

use clap::{Parser, ValueEnum};
use git_internal::{
    hash::{ObjectHash, get_hash_kind},
    internal::{
        index::{Index, IndexEntry},
        object::{
            blob::Blob,
            commit::Commit,
            signature::{Signature, SignatureType},
            tree::{Tree, TreeItemMode},
        },
    },
};
use serde::{Deserialize, Serialize};

use super::{
    get_target_commit, load_object, load_object_raw, rename_detect, reset,
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
        repo_hooks::{
            RepoHook, replay_repo_hook_output, run_advisory_repo_hook, run_repo_hook_with_io,
        },
        tree_plumbing,
    },
    utils::{
        attributes::{self, AttributeState},
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
    libra merge -X ours feature-x  Favor HEAD only where content conflicts
    libra merge -s ours archive   Record archive as merged while retaining the HEAD tree
    libra merge --allow-unrelated-histories imported-root
                                     Merge a root with no common ancestor
    libra merge --log=10 feature-x  Include target subjects in the merge message
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

/// Single-head merge strategies currently implemented by Libra.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeStrategy {
    /// Record the merge relationship while retaining the current HEAD tree.
    Ours,
}

/// Conflict-side preference accepted by the default three-way strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeFavor {
    /// Resolve only conflicting paths/hunks in favor of the current HEAD side.
    Ours,
    /// Resolve only conflicting paths/hunks in favor of the merged target side.
    Theirs,
}

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
    /// conflict markers. The re-run preserves the recovery-critical
    /// `--allow-unrelated-histories` permission but otherwise uses default
    /// merge options (an original `-m`/`--no-ff`/`--squash`/`--no-commit` is
    /// not replayed).
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

    /// Select the merge strategy. Libra currently supports only `ours`, which
    /// records both parents while retaining the current HEAD tree.
    #[arg(short = 's', long = "strategy", value_enum, conflicts_with_all = ["continue_merge", "abort", "restart", "strategy_option"])]
    pub strategy: Option<MergeStrategy>,

    /// Pass a strategy option to the default three-way merge. `ours` and
    /// `theirs` resolve conflicting paths/hunks in favor of that side while
    /// retaining all non-conflicting changes. May be repeated; the last value
    /// wins.
    #[arg(short = 'X', long = "strategy-option", value_enum, action = clap::ArgAction::Append, conflicts_with_all = ["continue_merge", "abort", "restart", "strategy"])]
    pub strategy_option: Vec<MergeFavor>,

    /// Permit a two-parent merge when the histories have no common ancestor.
    #[arg(long = "allow-unrelated-histories", conflicts_with_all = ["continue_merge", "abort", "restart"])]
    pub allow_unrelated_histories: bool,

    /// Append up to N target-side commit subjects to the generated merge
    /// message. Bare `--log` uses 20. With `-m`, an explicit `--log` still
    /// appends the shortlog; config-only `merge.log` remains suppressed.
    #[arg(long = "log", value_name = "N", num_args = 0..=1, require_equals = true, default_missing_value = "20", overrides_with = "no_log", conflicts_with_all = ["continue_merge", "abort", "restart"])]
    pub log: Option<usize>,

    /// Do not append target-side subjects to the merge message. Last one wins
    /// with `--log[=<N>]` and overrides `merge.log`.
    #[arg(long = "no-log", overrides_with = "log", conflicts_with_all = ["continue_merge", "abort", "restart"])]
    pub no_log: bool,

    /// Use the given message for the merge commit instead of the default. May
    /// also be given with `--continue` (a Libra extension — Git's
    /// `git merge --continue` takes no arguments and only lets you edit the
    /// stored message in an editor): it overrides the message recorded when the
    /// conflicted merge started, which is otherwise unreachable because Libra
    /// finalizes `--continue` without opening an editor.
    #[arg(
        short = 'm',
        long = "message",
        value_name = "MSG",
        conflicts_with = "abort"
    )]
    pub message: Option<String>,

    /// Merge changes but stage the result without committing or moving HEAD
    /// (no merge info recorded); finalize with a normal `commit`.
    #[arg(long, conflicts_with_all = ["continue_merge", "abort"])]
    pub squash: bool,

    /// Perform the merge and stage the result but stop before committing,
    /// recording merge state; finalize with `libra merge --continue`.
    #[arg(long = "no-commit", conflicts_with_all = ["squash", "continue_merge", "abort"])]
    pub no_commit: bool,

    /// Skip every `.libra/hooks` lifecycle hook for this merge. With
    /// `--continue`, this bypasses the pending commit/message/post hooks.
    #[arg(long = "no-verify", conflicts_with_all = ["abort", "restart"])]
    pub no_verify: bool,

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

    /// Do not auto-stage rerere-replayed resolutions for this merge. Rerere IS
    /// integrated: with `rerere.enabled`, a conflicted merge records each
    /// conflict's preimage and replays a recorded resolution when one matches;
    /// whether a replayed file is auto-STAGED follows the `rerere.autoUpdate`
    /// config. This flag is accepted for Git parity but the per-invocation
    /// override is not implemented — staging follows the config either way.
    /// (Git's positive `--rerere-autoupdate` is not exposed.)
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
    /// `--dry-run` only: the category of every would-be conflict (MG-04), so a
    /// caller can tell a `file-directory` collision — with the path the file
    /// would be moved to — from a `content` or `modify-delete` conflict. Absent
    /// whenever empty (schema-additive; every real merge omits it).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflict_kinds: Vec<ConflictReport>,
    /// Autostash outcome (lore.md §1.8): `applied` (re-applied cleanly),
    /// `stashed` (re-apply conflicted; entry promoted to the stash list), or
    /// `kept` (held while merge state persists, e.g. `--no-commit`). Absent
    /// whenever autostash was off or the tree was clean (schema-additive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autostash: Option<String>,
}

/// One would-be conflict in a `--dry-run` summary (MG-04).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ConflictReport {
    /// The path that will be unmerged — for a D/F collision, the path the file
    /// is moved to.
    pub path: String,
    /// `content` | `modify-delete` | `file-directory`.
    pub kind: String,
    /// D/F only: the colliding path the directory keeps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_path: Option<String>,
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
    /// Explicit merge strategy. `None` uses the default three-way strategy.
    pub strategy: Option<MergeStrategy>,
    /// Conflict-side preference for the default three-way strategy.
    pub favor: Option<MergeFavor>,
    /// Permit an empty merge base when histories are unrelated.
    pub allow_unrelated_histories: bool,
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
    /// Suppress repository hooks for this merge. Persisted into merge state so
    /// a conflict/no-commit continuation keeps the original trust decision.
    pub skip_hooks: bool,
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

/// This worktree's merge sidecar, for the §C.5 pseudo-ref projection
/// (`MERGE_HEAD` = `target`, `ORIG_HEAD` = `orig_head`). Read-only, and it
/// resolves through the same request-bound path as every other consumer, so
/// the projection can never answer from a different worktree's sidecar.
/// The SEMANTIC OID fields of this gitdir's `merge-state.json`, for GC root
/// collection (plan-20260714 §C.4.3): `(field name, oid)` pairs, `None` when
/// no merge is in progress, `Err` on a file that exists but cannot be
/// parsed. Text fields (branch names, messages, conflict paths) are NOT
/// returned — a path or branch name that happens to be 40 hex characters
/// must never be treated as an object reference.
pub(crate) fn merge_state_gc_oids(
    gitdir: &std::path::Path,
) -> Result<Option<Vec<(&'static str, String)>>, String> {
    let Some(state) = merge_state_for_pseudo_refs(gitdir)? else {
        return Ok(None);
    };
    let mut oids = vec![("orig_head", state.orig_head), ("target", state.target)];
    if let Some(base) = state.base {
        oids.push(("base", base));
    }
    Ok(Some(oids))
}

pub(crate) fn merge_state_for_pseudo_refs(
    gitdir: &std::path::Path,
) -> Result<Option<MergeState>, String> {
    let path = gitdir.join("merge-state.json");
    if !path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str(&data)
        .map(Some)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MergeState {
    pub head_name: String,
    pub orig_head: String,
    pub target: String,
    pub target_ref: String,
    /// Common ancestor used by the three-way merge, when it is a real commit.
    /// `None` represents the virtual empty base used by
    /// `--allow-unrelated-histories` AND the recursive virtual ancestor of a
    /// criss-cross merge, which is a one-shot object deliberately left out of
    /// this file so it is not a GC root (ADR-MG-04, see [`recorded_merge_base`]).
    /// Deserializing older state files remains compatible because a JSON string
    /// maps to `Some` and missing fields use the default.
    #[serde(default)]
    pub base: Option<String>,
    /// Strategy needed to preserve `--no-commit -s ours` through `--continue`.
    /// `None` is the default three-way strategy and keeps old state compatible.
    #[serde(default)]
    pub strategy: Option<MergeStrategy>,
    /// Replayed by `--restart` so a conflicted merge with a virtual empty base
    /// does not turn into an unrelated-history rejection after restoration.
    #[serde(default)]
    pub allow_unrelated_histories: bool,
    /// Whether the starting invocation used `--no-verify`.
    #[serde(default)]
    pub skip_hooks: bool,
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
        // Part C W1 (§C.4.2/§C.4.3): an in-progress merge belongs to the
        // worktree whose index holds the conflict, so its state lives in THIS
        // worktree's gitdir. Identical path for the main worktree (local ==
        // common storage), so a merge started by an older binary is still found.
        util::request_worktree_gitdir_strict().join("merge-state.json")
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
        // Record the writer's scope (W2, ADR-0714-08): the field is what lets
        // a later control action PROVE this common-storage file is main's
        // instead of guessing. Injected at the JSON layer so every
        // constructor stays untouched; deserialization ignores unknown keys.
        let mut value = serde_json::to_value(self)
            .map_err(|error| PullMergeError::StateSave(error.to_string()))?;
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "owner_scope".to_string(),
                serde_json::Value::String(
                    crate::internal::worktree_scope::WorktreeScope::for_request()
                        .storage_key()
                        .to_string(),
                ),
            );
        }
        let data = serde_json::to_vec_pretty(&value)
            .map_err(|error| PullMergeError::StateSave(error.to_string()))?;
        // Atomic + fsynced write (lore.md §7.7): sequencer state is
        // recovery-critical, so a crash must leave it either fully written or
        // absent — never truncated — and it must survive a power loss.
        crate::utils::atomic_write::write_atomic(&path, &data, true)
            .map_err(|error| PullMergeError::StateSave(format!("{}: {error}", path.display())))
    }

    fn cleanup() -> Result<(), PullMergeError> {
        let path = Self::path();
        // Durable (§C.10): a resurrected merge-state replays a merge the user
        // already concluded — same stakes as the stash log.
        crate::utils::atomic_write::remove_durably(&path)
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
        // Part C W1 (§C.4.3): the held autostash belongs to this worktree's
        // in-progress merge, so it lives in this worktree's gitdir alongside
        // `merge-state.json`. It remains a fail-closed GC root; the held commit
        // is protected in a multi-worktree repo by GC's per-repo prune skip.
        util::request_worktree_gitdir_strict().join("merge-autostash.json")
    }

    /// Read a SPECIFIC worktree's held-autostash sidecar. GC enumerates every
    /// worktree's gitdir (Part C §C.9) — a held autostash is a first-class
    /// reachability root regardless of which worktree holds it.
    pub(crate) fn load_optional_sync_in_gitdir(
        gitdir: &std::path::Path,
    ) -> Result<Option<Self>, String> {
        Self::load_optional_sync_at(&gitdir.join("merge-autostash.json"))
    }

    fn load_optional_sync_at(path: &std::path::Path) -> Result<Option<Self>, String> {
        Ok(Self::load_snapshot_at(path)?.map(|snapshot| snapshot.sidecar))
    }

    /// ONE read that yields everything a consumer needs: the parsed sidecar
    /// AND the recorded owner, from the same bytes. Verifying ownership by
    /// re-reading the file (as the first cut did) let a concurrent
    /// replacement validate sidecar B while sidecar A was applied — and then
    /// delete B, the only durable reference to a newer stash.
    fn load_snapshot_at(path: &std::path::Path) -> Result<Option<AutostashSnapshot>, String> {
        let data = match fs::read_to_string(path) {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
        };
        let value: serde_json::Value = serde_json::from_str(&data)
            .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
        let sidecar: MergeAutostash = serde_json::from_value(value.clone())
            .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
        let recorded_owner = value
            .get("owner_scope")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        Ok(Some(AutostashSnapshot {
            sidecar,
            recorded_owner,
        }))
    }

    fn load_snapshot() -> Result<Option<AutostashSnapshot>, String> {
        Self::load_snapshot_at(&Self::path())
    }

    fn save(&self) -> Result<(), PullMergeError> {
        // Serialize with every consumer (W2 r5 #2): a save landing between a
        // consumer's verify and its cleanup would be deleted unapplied.
        let _lock = acquire_autostash_lock().map_err(PullMergeError::Autostash)?;
        let path = Self::path();
        // Record the writer's scope (W2, ADR-0714-08) — like MergeState: the
        // held autostash is promotable into the SHARED stash list, so an
        // unowned common-storage file must stay refusable.
        let mut value = serde_json::to_value(self)
            .map_err(|error| PullMergeError::Autostash(error.to_string()))?;
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "owner_scope".to_string(),
                serde_json::Value::String(
                    crate::internal::worktree_scope::WorktreeScope::for_request()
                        .storage_key()
                        .to_string(),
                ),
            );
        }
        let data = serde_json::to_vec_pretty(&value)
            .map_err(|error| PullMergeError::Autostash(error.to_string()))?;
        crate::utils::atomic_write::write_atomic(&path, &data, true)
            .map_err(|error| PullMergeError::Autostash(format!("{}: {error}", path.display())))
    }

    fn cleanup() -> Result<(), PullMergeError> {
        let path = Self::path();
        // Durable and SURFACED: a swallowed failure here leaves a sidecar
        // that a later merge would re-promote — duplicating changes the user
        // already restored.
        crate::utils::atomic_write::remove_durably(&path)
            .map_err(|error| PullMergeError::Autostash(format!("{}: {error}", path.display())))
    }
}

/// One consistent read of the held-autostash sidecar: the parsed document and
/// the owner it records, from the same bytes.
struct AutostashSnapshot {
    sidecar: MergeAutostash,
    recorded_owner: Option<String>,
}

/// RAII guard serializing every held-autostash consumer and writer in ONE
/// worktree (W2 r5 #2): load→verify→consume→cleanup must be atomic against a
/// concurrent save, or a replacement between the verify and the cleanup
/// deletes a sidecar that was never the one applied. Per-gitdir flock,
/// blocking, released on drop.
struct AutostashLockGuard {
    file: fs::File,
}

impl Drop for AutostashLockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn acquire_autostash_lock() -> Result<AutostashLockGuard, String> {
    let lock_path = util::request_worktree_gitdir_strict().join("merge-autostash.lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| format!("{}: {error}", lock_path.display()))?;
    file.lock()
        .map_err(|error| format!("{}: {error}", lock_path.display()))?;
    Ok(AutostashLockGuard { file })
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
    /// Low-level merge-driver configuration could not be read. Unknown names
    /// are valid and fall back to text; only an actual config-store failure is
    /// an error.
    #[error("failed to read merge driver config: {0}")]
    MergeDriverConfigRead(String),
    /// A rename-detection config value Libra rejects (`merge.renames`,
    /// `merge.renameLimit`, `merge.directoryRenames`, and the applicable
    /// `diff.*` fall-backs). A typo must not silently change merge behavior.
    #[error("bad config value '{value}' for '{key}' (expected {expected})")]
    InvalidRenameConfig {
        key: String,
        value: String,
        expected: &'static str,
    },
    /// The rename-detection config could not be read (config-store I/O).
    #[error("failed to read rename config '{key}': {detail}")]
    RenameConfigRead { key: String, detail: String },
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
    /// A three-way merge input carries a gitlink the merge would have to
    /// arbitrate. Refused before any index/worktree write (ADR-MG-01) rather
    /// than silently dropped from the merge result the way it used to be.
    #[error("{0}")]
    GitlinkUnsupported(GitlinkNotSupported),
    /// The recursive virtual ancestor (MG-02) would have to nest deeper than
    /// [`MAX_VIRTUAL_ANCESTOR_DEPTH`]. Git recurses without a ceiling; Libra
    /// folds the bases with real recursion, so it stops with a message instead
    /// of risking a stack overflow on a pathological history.
    #[error(
        "merging these branches needs a virtual common ancestor nested more than \
         {MAX_VIRTUAL_ANCESTOR_DEPTH} levels deep, which Libra does not build"
    )]
    VirtualAncestorTooDeep,
    /// More merge bases than [`MAX_VIRTUAL_ANCESTOR_BASES`] at one level of
    /// the fold. Every base folded in costs another merge-base walk against
    /// every base already folded, so the fold's work grows with the SQUARE of
    /// the width; the ceiling keeps that bounded instead of letting a
    /// pathological history run for hours.
    #[error(
        "merging these branches needs a virtual common ancestor folded from {bases} merge \
         bases, more than the {MAX_VIRTUAL_ANCESTOR_BASES} Libra folds"
    )]
    VirtualAncestorTooWide { bases: usize },
    #[error("merge has conflicts in {paths}")]
    Conflicts { paths: String, squash: bool },
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
    #[error("failed to resolve the identity for the merge commit: {0}")]
    IdentityMissing(String),
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
    #[error("{hook} hook failed: {detail}")]
    RepositoryHook { hook: &'static str, detail: String },
    #[error("failed to write merge commit message file '{path}': {detail}")]
    MessageFileWrite { path: String, detail: String },
    #[error("failed to read merge commit message file '{path}': {detail}")]
    MessageFileRead { path: String, detail: String },
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
            PullMergeError::VirtualAncestorTooDeep | PullMergeError::VirtualAncestorTooWide { .. } => {
                CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::Unsupported)
                .with_hint(
                    "merge the branches' common ancestors together first, so the history has a single merge base",
                )
                .with_hint("or record the merge with 'libra merge -s ours' and reconcile the tree by hand")
            }
            PullMergeError::GitlinkUnsupported(..) => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::Unsupported)
                .with_hint(
                    "submodule merging is a permanent non-goal; resolve the submodule pointer outside Libra",
                )
                .with_hint(
                    "or drop the gitlink entry from the branches being merged so no submodule decision is needed",
                ),
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
            PullMergeError::RepositoryHook { .. } => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("use --no-verify to bypass repository hooks"),
            PullMergeError::MessageFileWrite { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::IoWriteFailed),
            PullMergeError::MessageFileRead { .. } => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::IoReadFailed),
            PullMergeError::NonFastForward { .. } => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                .with_hint("run 'libra pull' without --ff-only to allow a merge commit")
                .with_hint("or run 'libra pull --rebase' to replay local commits"),
            PullMergeError::Conflicts { squash: true, .. } => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                .with_hint("resolve conflicts, stage the resolved paths with 'libra add', then run 'libra commit'"),
            PullMergeError::Conflicts { squash: false, .. }
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
            PullMergeError::MergeDriverConfigRead(..) => {
                CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
            }
            PullMergeError::InvalidRenameConfig { .. } => CliError::failure(error.to_string())
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("set merge.renames to true/false and merge.renameLimit to an integer"),
            PullMergeError::RenameConfigRead { .. } => {
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
            // Mirrors `CommitError::IdentityMissing`: a merge commit needs the same
            // identity as any other commit, so it fails the same way and offers the
            // same fix.
            PullMergeError::IdentityMissing(..) => CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::AuthMissingCredentials)
                .with_hint("run 'libra config --global user.name \"Your Name\"' and 'libra config --global user.email \"you@example.com\"'")
                .with_hint("omit '--global' to set the identity only in this repository."),
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
    // Part C W1 (§C.4.2/§C.4.3): merge is now safe in a LINKED worktree — its
    // in-progress state (`merge-state.json`) and held autostash
    // (`merge-autostash.json`, still a fail-closed GC root, protected by GC's
    // multi-worktree prune skip) live in this worktree's own gitdir, the mutex
    // resolves the merge per-worktree, and it merges into THIS worktree's index
    // and current branch. So the `ensure_main_worktree` guard is lifted, matching
    // cherry-pick/am/revert. (`pull` remains guarded — it drives merge through an
    // internal path that has not yet been routed through a scoped API.)
    //
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
            let merge_log = if let Some(limit) = args.log {
                limit
            } else if args.no_log || args.message.is_some() {
                0
            } else {
                crate::command::history_config::merge_log_limit().await?
            };
            let options = PullMergeOptions {
                ff_only,
                no_ff,
                strategy: args.strategy,
                favor: args.strategy_option.last().copied(),
                allow_unrelated_histories: args.allow_unrelated_histories,
                message: args.message.clone(),
                squash: args.squash,
                no_commit: args.no_commit,
                skip_hooks: args.no_verify,
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
        (None, true, false) => {
            run_merge_continue(output, args.no_verify, args.message.clone()).await
        }
        (None, false, true) => run_merge_abort(output).await,
        (None, false, false) => Err(MergeError::MissingAction),
        _ => Err(MergeError::ConflictingAction),
    }
}

/// Build a merge commit that carries the repository's configured identity.
///
/// `Commit::from_tree_id` hardcodes `mega <admin@mega.org>` as both author and
/// committer, so every merge commit built through it silently discards
/// `user.name` / `user.email` (and the `GIT_AUTHOR_*` / `GIT_COMMITTER_*`
/// overrides). A merge commit is an ordinary commit as far as authorship goes,
/// so it resolves its identity through the same path as `libra commit`.
async fn build_merge_commit(
    tree_id: ObjectHash,
    parent_commit_ids: Vec<ObjectHash>,
    message: &str,
) -> Result<Commit, PullMergeError> {
    let (author, committer, _) = crate::command::commit::create_commit_signatures(None, None)
        .await
        .map_err(|error| PullMergeError::IdentityMissing(error.to_string()))?;
    Ok(Commit::new(
        author,
        committer,
        tree_id,
        parent_commit_ids,
        message,
    ))
}

async fn run_pre_merge_commit_hook(output: &OutputConfig) -> Result<(), PullMergeError> {
    run_blocking_merge_hook(RepoHook::PreMergeCommit, &[], None, output).await
}

async fn run_blocking_merge_hook(
    hook: RepoHook,
    args: &[String],
    writable_message_file: Option<&Path>,
    output: &OutputConfig,
) -> Result<(), PullMergeError> {
    let Some(hook_output) = run_repo_hook_with_io(hook, args, None, writable_message_file)
        .await
        .map_err(|error| PullMergeError::RepositoryHook {
            hook: hook.as_str(),
            detail: error.to_string(),
        })?
    else {
        return Ok(());
    };
    replay_repo_hook_output(&hook_output, output).map_err(|detail| {
        PullMergeError::RepositoryHook {
            hook: hook.as_str(),
            detail,
        }
    })?;
    if hook_output.timed_out {
        return Err(PullMergeError::RepositoryHook {
            hook: hook.as_str(),
            detail: format!(
                "hook '{}' exceeded the 15 minute timeout",
                hook_output.path.display()
            ),
        });
    }
    if hook_output.exit_code != 0 {
        return Err(PullMergeError::RepositoryHook {
            hook: hook.as_str(),
            detail: format!(
                "hook '{}' failed with exit code {}",
                hook_output.path.display(),
                hook_output.exit_code
            ),
        });
    }
    Ok(())
}

fn merge_message_path() -> Result<PathBuf, PullMergeError> {
    util::try_get_worktree_gitdir(None)
        .map(|gitdir| gitdir.join("COMMIT_EDITMSG"))
        .map_err(|error| PullMergeError::MessageFileWrite {
            path: ".libra/COMMIT_EDITMSG".to_string(),
            detail: format!("failed to locate the current worktree metadata directory: {error}"),
        })
}

fn write_merge_message(path: &Path, message: &str) -> Result<(), PullMergeError> {
    crate::utils::atomic_write::write_atomic(path, message.as_bytes(), false).map_err(|error| {
        PullMergeError::MessageFileWrite {
            path: path.display().to_string(),
            detail: error.to_string(),
        }
    })
}

fn read_merge_message(path: &Path) -> Result<String, PullMergeError> {
    fs::read_to_string(path).map_err(|error| PullMergeError::MessageFileRead {
        path: path.display().to_string(),
        detail: error.to_string(),
    })
}

async fn run_merge_message_hooks(
    message: &str,
    output: &OutputConfig,
) -> Result<String, PullMergeError> {
    let message_path = merge_message_path()?;
    write_merge_message(&message_path, message)?;
    let message_path_arg = message_path
        .to_str()
        .ok_or_else(|| PullMergeError::RepositoryHook {
            hook: RepoHook::PrepareCommitMsg.as_str(),
            detail: format!(
                "merge commit message path '{}' is not valid UTF-8",
                message_path.display()
            ),
        })?
        .to_string();
    run_blocking_merge_hook(
        RepoHook::PrepareCommitMsg,
        &[message_path_arg.clone(), "merge".to_string()],
        Some(&message_path),
        output,
    )
    .await?;
    run_blocking_merge_hook(
        RepoHook::CommitMsg,
        &[message_path_arg],
        Some(&message_path),
        output,
    )
    .await?;
    let message = read_merge_message(&message_path)?;
    if message.trim().is_empty() {
        return Err(PullMergeError::RepositoryHook {
            hook: RepoHook::CommitMsg.as_str(),
            detail: "hook left the merge commit message empty".to_string(),
        });
    }
    Ok(message)
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
                "Would merge cleanly by the '{}' strategy.\n(dry run: nothing was written)",
                result.strategy
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
            "ours" => info_println!(output, "Merge made by the 'ours' strategy."),
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
    CliError::from(error)
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
/// The ownership matrix every held-autostash CONSUMER must pass (W2,
/// ADR-0714-08) before applying or promoting the sidecar — both adopt its
/// commit into user-visible state and then delete the evidence.
///
/// * recorded owner == this scope → operable (proven ours);
/// * recorded owner == some OTHER scope → refused (a copied/moved file);
/// * no record, MAIN scope, linked-worktree history → refused (an old
///   binary's common-storage file could be a removed linked worktree's);
/// * no record otherwise → operable (a W1-era file in an unambiguous gitdir).
fn verify_autostash_ownership(recorded: Option<&str>) -> Result<(), String> {
    let scope = crate::internal::worktree_scope::WorktreeScope::for_request();
    let path = util::request_worktree_gitdir_strict().join("merge-autostash.json");
    let recorded = recorded.map(str::to_string);
    match recorded {
        Some(owner) if owner == scope.storage_key() => Ok(()),
        Some(owner) => Err(format!(
            "the held-autostash sidecar at '{}' records owner scope '{owner}', not this \
             worktree's — applying or promoting it would adopt another worktree's stashed \
             changes and delete the evidence. Conclude the merge in the worktree that owns \
             it, or remove the file after inspecting `libra stash show` against its commit",
            path.display()
        )),
        None if !scope.is_linked()
            && crate::command::maintenance::repository_had_linked_worktrees() =>
        {
            Err(format!(
                "the held-autostash sidecar at '{}' carries no owner record, and this \
                 repository has linked-worktree history, so it cannot be proven to be the \
                 main worktree's. Inspect `libra stash show` against its commit, then \
                 remove the file manually if it is stale",
                path.display()
            ))
        }
        None => Ok(()),
    }
}

/// notice (never lost — the lore 1.8 headline); other apply error → sidecar
/// KEPT and a warning printed (the merge outcome itself is never changed).
/// While merge state persists the stash simply stays held.
/// Remove the sidecar ONLY if it is still the document `snapshot` was read
/// from (W2 r6 #3): the caller drops the autostash lock before taking the
/// stash-stack lock (§C.10's order — repository lock never inside a local
/// one), so a writer may replace the file in between. Deleting a replacement
/// would destroy the only reference to a NEWER held stash; leaving it is
/// always safe (stale-file recovery re-promotes it with a warning).
fn cleanup_autostash_if_matches(snapshot: &AutostashSnapshot) -> Result<bool, String> {
    let _lock = acquire_autostash_lock()?;
    match MergeAutostash::load_snapshot()? {
        Some(current)
            if current.sidecar.stash_commit == snapshot.sidecar.stash_commit
                && current.recorded_owner == snapshot.recorded_owner =>
        {
            MergeAutostash::cleanup().map_err(|error| error.to_string())?;
            Ok(true)
        }
        Some(_) => Ok(false),
        None => Ok(true),
    }
}

/// Load one consistent snapshot of the held autostash under the lock, then
/// RELEASE the lock (§C.10: the stash-stack — repository — lock taken by the
/// consumers below must never nest inside this local one). `Err` = the file
/// exists but cannot be read; the caller must not mutate past it.
fn snapshot_held_autostash() -> Result<Option<AutostashSnapshot>, String> {
    let _lock = acquire_autostash_lock()?;
    MergeAutostash::load_snapshot()
}

async fn resolve_pending_autostash(
    output: &OutputConfig,
    preserve_conflicts: bool,
) -> Option<String> {
    let snapshot = match snapshot_held_autostash() {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return None,
        Err(detail) => {
            crate::utils::error::emit_warning(format!(
                "could not read merge-autostash.json ({detail}); leaving it in place"
            ));
            return None;
        }
    };
    resolve_pending_autostash_with(output, snapshot, preserve_conflicts).await
}

/// The consumer half, taking a snapshot the CALLER loaded — the merge
/// controls load theirs before mutating anything (W2 r6 #4), so the document
/// they preflighted is the one consumed.
async fn resolve_pending_autostash_with(
    output: &OutputConfig,
    snapshot: AutostashSnapshot,
    preserve_conflicts: bool,
) -> Option<String> {
    let sidecar = &snapshot.sidecar;
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
    if let Err(reason) = verify_autostash_ownership(snapshot.recorded_owner.as_deref()) {
        crate::utils::error::emit_warning(format!("{reason}; leaving it in place"));
        return Some("kept".to_string());
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
    // A conflicted squash has no merge sidecar, but its unmerged stages must
    // survive. Git leaves its autostash for a later stash pop in this case.
    if preserve_conflicts {
        return store_pending_autostash(output, &snapshot, &oid).await;
    }
    match crate::command::stash::apply_held_stash_commit(&oid).await {
        Ok(()) => {
            match cleanup_autostash_if_matches(&snapshot) {
                Ok(true) => {}
                Ok(false) => crate::utils::error::emit_warning(
                    "the autostash sidecar changed while it was being applied; the newer \
                     file was left in place",
                ),
                Err(error) => crate::utils::error::emit_warning(format!(
                    "the applied autostash's sidecar could not be removed ({error}); a later \
                     merge would re-promote it — remove it manually"
                )),
            }
            if !output.quiet {
                eprintln!("Applied autostash.");
            }
            Some("applied".to_string())
        }
        Err(crate::command::stash::StashError::MergeConflict(_)) => {
            // All-or-nothing apply: the merge result is intact. Promote the
            // stash into the visible list so nothing is lost.
            if !output.quiet {
                eprintln!("Applying autostash resulted in conflicts.");
            }
            store_pending_autostash(output, &snapshot, &oid).await
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

async fn store_pending_autostash(
    output: &OutputConfig,
    snapshot: &AutostashSnapshot,
    oid: &ObjectHash,
) -> Option<String> {
    match crate::command::stash::store_stash_commit(oid, "autostash").await {
        Ok(()) => {
            match cleanup_autostash_if_matches(snapshot) {
                Ok(true) => {}
                Ok(false) => crate::utils::error::emit_warning(
                    "the autostash sidecar changed while it was being promoted; \
                             the newer file was left in place",
                ),
                Err(error) => crate::utils::error::emit_warning(format!(
                    "the promoted autostash's sidecar could not be removed \
                             ({error}); a later merge would re-promote it — remove it \
                             manually"
                )),
            }
            if !output.quiet {
                eprintln!(
                    "Your changes are safe in the stash (stash@{{0}}).\nAfter resolving any conflicts, run \"libra stash pop\" to restore your local changes."
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

pub(crate) async fn run_merge_for_pull_with_options(
    target_ref: &str,
    upstream: &str,
    output: &OutputConfig,
    options: PullMergeOptions,
) -> Result<PullMergeSummary, PullMergeError> {
    let skip_hooks = options.skip_hooks;
    if MergeState::load_optional_sync()
        .map_err(PullMergeError::StateLoad)?
        .is_some()
    {
        return Err(PullMergeError::MergeInProgress);
    }

    // A squash leaves conflict stages but deliberately no merge sidecar.
    // Git checks the index even for an otherwise up-to-date merge, before
    // autostash or any other operation can replace the unresolved entries.
    let index =
        Index::load(path::index()).map_err(|error| PullMergeError::IndexLoad(error.to_string()))?;
    let unresolved = unresolved_conflicted_paths(&index, &[]);
    if !unresolved.is_empty() {
        return Err(PullMergeError::Conflicts {
            paths: unresolved.join(", "),
            squash: true,
        });
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
    // ADR-MG-01: refuse a submodule-arbitrating merge here, ahead of the
    // autostash below — its stash commit, sidecar and worktree reset are
    // writes, and the card requires a refused merge to make none.
    preflight_merge_gitlinks(&target_commit, &options).await?;

    // MG-05 (Codex R7 and R8): the rename config is STRICT, so an unusable
    // `merge.renames` must be refused before ANY repository mutation — before
    // the stale-sidecar recovery below, which promotes a leftover stash into
    // the stash list and deletes its sidecar, and before the autostash that
    // follows it, which writes a stash commit, a durable sidecar and resets the
    // worktree. Only the three-way path is preflighted, matching exactly where
    // Git parses the value at all.
    let reaches_three_way = merge_reaches_three_way_engine(&target_commit, &options).await;
    let preflighted_rename_config = if reaches_three_way {
        Some(merge_rename_config().await?)
    } else {
        None
    };
    let preflighted_default_driver = if reaches_three_way {
        read_merge_default_driver()
            .await
            .map_err(PullMergeError::MergeDriverConfigRead)?
    } else {
        None
    };
    let preflighted_external_drivers = if reaches_three_way {
        read_external_merge_runtime()
            .await
            .map_err(PullMergeError::MergeDriverConfigRead)?
    } else {
        Arc::new(ExternalMergeRuntime::default())
    };

    // ── autostash (lore.md §1.8) ──
    // Stale-sidecar recovery: a leftover sidecar with NO merge in progress
    // (crash after a finalize apply, or an interrupted start) is promoted to
    // the stash list — never overwritten or lost. Skipped on --restart
    // re-entry, where the HELD sidecar legitimately exists without state.
    // A sidecar that EXISTS but cannot be read is a hard stop, not a skip:
    // proceeding would let the later `--autostash` save OVERWRITE the corrupt
    // file — destroying the only durable reference to a held commit, which GC
    // may then collect.
    // §C.10 lock order: the snapshot is taken under the LOCAL autostash lock
    // and the lock is RELEASED before `store_stash_commit` takes the
    // repository-wide stash-stack lock — a repository lock never nests inside
    // a local one. The cleanup afterwards is identity-checked, so a sidecar
    // replaced in the unlocked window is preserved, never deleted.
    // `--dry-run` writes nothing, and promoting a stale sidecar into the stash
    // list (then deleting it) is a write — so a preview leaves a leftover
    // sidecar exactly where it found it, for the next REAL merge to recover.
    let held_snapshot = if options.preserve_held_autostash || options.dry_run {
        None
    } else {
        snapshot_held_autostash().map_err(PullMergeError::Autostash)?
    };
    if let Some(snapshot) = held_snapshot {
        let sidecar = &snapshot.sidecar;
        // ADR-0714-08: promoting adopts the file into the SHARED stash list
        // and deletes the evidence — only a file this scope can PROVE its own
        // may be adopted, in any worktree (a foreign-marked file inside a
        // linked gitdir is a manual copy, not that worktree's autostash).
        verify_autostash_ownership(snapshot.recorded_owner.as_deref())
            .map_err(PullMergeError::Autostash)?;
        if let Ok(oid) = ObjectHash::from_str(&sidecar.stash_commit) {
            match crate::command::stash::store_stash_commit(&oid, "autostash").await {
                Ok(()) => {
                    match cleanup_autostash_if_matches(&snapshot) {
                        Ok(true) => {}
                        Ok(false) => crate::utils::error::emit_warning(
                            "the autostash sidecar changed while it was being recovered; \
                             the newer file was left in place",
                        ),
                        Err(error) => {
                            return Err(PullMergeError::Autostash(format!(
                                "the recovered autostash's sidecar could not be removed: \
                                 {error}"
                            )));
                        }
                    }
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

    let dry_run = options.dry_run;
    let result = run_merge_for_pull_inner(
        target_commit,
        upstream,
        output,
        options,
        preflighted_rename_config,
        preflighted_default_driver,
        preflighted_external_drivers,
    )
    .await;
    // Uniform finalize: applies when no merge state persists (clean success,
    // up-to-date, squash, or a start failure), holds while state exists
    // (conflict / --no-commit). The merge outcome itself is never changed.
    // Skipped for `--dry-run`, which took no autostash and must not apply or
    // promote a held one either.
    let autostash_outcome = if dry_run {
        None
    } else {
        resolve_pending_autostash(
            output,
            matches!(&result, Err(PullMergeError::Conflicts { squash: true, .. })),
        )
        .await
    };
    match result {
        Ok(mut summary) => {
            summary.autostash = autostash_outcome;
            if !skip_hooks && merge_completed_for_post_hook(&summary) {
                let squash = if summary.strategy == "squash" {
                    "1"
                } else {
                    "0"
                };
                run_advisory_repo_hook(RepoHook::PostMerge, &[squash.to_string()], None, output)
                    .await;
            }
            Ok(summary)
        }
        Err(error) => Err(error),
    }
}

/// The rename configuration for a three-way merge — read strictly, EXCEPT on a
/// merge that is only here because `--squash` or `--no-commit` skipped the
/// fast-forward branch. There the result is the target tree whatever rename
/// detection decides, and Git never parses the value; reading it strictly
/// would refuse a merge Git performs.
async fn three_way_rename_config(
    options: &ThreeWayMergeOptions<'_>,
) -> Result<MergeRenameConfig, PullMergeError> {
    if options.fast_forwardable {
        return Ok(MergeRenameConfig {
            enabled: false,
            ..MergeRenameConfig::default()
        });
    }
    if let Some(config) = &options.rename_config {
        return Ok(config.clone());
    }
    merge_rename_config().await
}

/// Will this merge reach the three-way engine, the only path that reads the
/// rename configuration?
///
/// The rename config is STRICT (MG-05): an unusable value is a hard error. It
/// must therefore be refused before the repository is mutated, and autostash
/// mutates more than the virtual-ancestor fold Codex R5 closed — it writes a
/// stash commit, a durable sidecar and resets the worktree. But the refusal
/// must land exactly where Git's does and nowhere else: measured on git 2.50.1
/// with `merge.renames = not-a-bool`, a fast-forward merge and an
/// already-up-to-date merge both succeed (the value is never parsed), `-s ours`
/// succeeds, and `--ff-only` on a diverged history reports the non-fast-forward
/// error rather than the config error; only a real three-way merge fails with
/// `fatal: bad boolean config value`. This predicate answers "yes" for that
/// last case alone, and answers "no" whenever it cannot tell — the merge itself
/// then reports its own error, unchanged.
async fn merge_reaches_three_way_engine(
    target_commit: &Commit,
    options: &PullMergeOptions,
) -> bool {
    if options.strategy.is_some() {
        return false;
    }
    let Some(current_commit_id) = Head::current_commit().await else {
        return false;
    };
    let Ok(current_commit) = load_object::<Commit>(&current_commit_id) else {
        return false;
    };
    let Ok(bases) = merge_base_commits(
        &current_commit,
        target_commit,
        merge_options_will_fold(options),
    ) else {
        return false;
    };
    if bases.is_empty() && !options.allow_unrelated_histories {
        return false;
    }
    if merge_is_up_to_date(bases.as_slice(), target_commit) {
        return false;
    }
    // Fast-forwardable is not a three-way merge, and stays that way under
    // `--squash` and `--no-commit`: measured on git 2.50.1 with
    // `merge.renames=not-a-bool`, `git merge --squash` and `git merge
    // --no-commit` both print "Updating .. Fast-forward" and succeed, while
    // `git merge --no-ff` on the SAME history fails with `fatal: bad boolean
    // config value`. `--ff-only` on a diverged history is the non-fast-forward
    // error, which Git reports in preference to the config error.
    if merge_head_is_sole_base(bases.as_slice(), &current_commit) {
        return options.no_ff;
    }
    if options.ff_only {
        return false;
    }
    true
}

/// Whether the target is already reachable from HEAD — nothing to merge.
/// Shared by the merge itself and by [`preflight_merge_gitlinks`] so the two can
/// never disagree about which merges arbitrate anything (GC-02).
///
/// Phrased over the merge-base SET (MG-02): a commit that is an ancestor of the
/// other dominates every other common ancestor, so "already merged" is exactly
/// the shape where the target is the ONE merge base. A criss-cross history with
/// several bases is never up to date and never fast-forwardable.
fn merge_is_up_to_date(bases: &[Commit], target_commit: &Commit) -> bool {
    matches!(bases, [base] if base.id == target_commit.id)
}

/// Whether a merge run with `options` would fold several merge bases into a
/// virtual ancestor at all. Shared by the preflight and the engine so the width
/// ceiling can never fire for a merge that decides the shape some other way.
fn merge_options_will_fold(options: &PullMergeOptions) -> bool {
    options.strategy.is_none() && !options.ff_only
}

/// Whether HEAD is the sole merge base — the shape a fast-forward (and
/// `--ff-only`) requires.
fn merge_head_is_sole_base(bases: &[Commit], current_commit: &Commit) -> bool {
    matches!(bases, [base] if base.id == current_commit.id)
}

/// The merge base recorded in `merge-state.json`.
///
/// `None` for an unrelated-history merge (virtual empty base) AND for a
/// criss-cross merge, whose base is the recursive virtual ancestor: a one-shot
/// object that is deliberately not a GC root (ADR-MG-04). Recording it would
/// pin the very object `maintenance gc` is meant to be free to reclaim, and
/// nothing needs it — `--continue` finishes from the index, and `--restart`
/// recomputes the ancestor from the real bases.
fn recorded_merge_base(bases: &[Commit]) -> Option<ObjectHash> {
    match bases {
        [base] => Some(base.id),
        _ => None,
    }
}

/// Whether the merge will fast-forward: HEAD is the merge base and no option
/// forces a merge commit. A fast-forward adopts the target tree wholesale, so
/// it decides nothing — gitlinks included. Shared with
/// [`preflight_merge_gitlinks`] (GC-02).
fn merge_is_fast_forward(
    bases: &[Commit],
    current_commit: &Commit,
    options: &PullMergeOptions,
) -> bool {
    merge_head_is_sole_base(bases, current_commit)
        && options.strategy.is_none()
        && !options.no_ff
        && !options.squash
        && !options.no_commit
}

/// ADR-MG-01 gate for the merge WRAPPER, ahead of every mutation it performs.
///
/// `perform_three_way_merge` has its own gate, but by the time it runs the
/// autostash has already written a stash commit, an fsynced sidecar, and reset
/// the working tree — writes the card requires a refused merge not to make.
/// This mirrors the placement `--verify-signatures` already uses for the same
/// reason. Only a merge that will actually ARBITRATE is checked: an
/// up-to-date, fast-forward, unborn-HEAD or `-s ours` merge adopts a tree
/// wholesale and never decides anything about a submodule.
async fn preflight_merge_gitlinks(
    target_commit: &Commit,
    options: &PullMergeOptions,
) -> Result<(), PullMergeError> {
    if options.strategy.is_some() {
        return Ok(());
    }
    let Some(current_commit_id) = Head::current_commit().await else {
        return Ok(());
    };
    let current_commit: Commit =
        load_object(&current_commit_id).map_err(|error| PullMergeError::CurrentLoad {
            commit_id: current_commit_id.to_string(),
            detail: error.to_string(),
        })?;
    let bases = merge_base_commits(
        &current_commit,
        target_commit,
        merge_options_will_fold(options),
    )?;
    // Every shape the engine settles WITHOUT arbitrating is skipped here, so a
    // gitlink refusal can never pre-empt the engine's own verdict:
    //   * unrelated histories the user did not opt into are rejected outright;
    //   * `--ff-only` on a genuinely diverged history is rejected outright;
    //   * an up-to-date or fast-forward merge adopts a tree wholesale.
    if bases.is_empty() && !options.allow_unrelated_histories {
        return Ok(());
    }
    if options.ff_only && !merge_head_is_sole_base(&bases, &current_commit) {
        return Ok(());
    }
    if merge_is_up_to_date(&bases, target_commit)
        || merge_is_fast_forward(&bases, &current_commit, options)
    {
        return Ok(());
    }
    if bases.len() <= 1 && incremental_tree_walk_enabled() {
        // MG-03: the same read-only gate the engine runs, on the trees — no
        // flattening, so the preflight's reads are bounded like the merge's.
        let root = |id: ObjectHash| WalkEntry {
            id,
            mode: TreeItemMode::Tree,
        };
        let mut source = ObjectStoreTrees::new();
        // Root trees through `refs/replace`, as the flattening path's root
        // `load_object` does (see `perform_incremental_three_way_merge`).
        return incremental_gitlink_gate(
            &mut source,
            &[
                bases
                    .first()
                    .map(|base| root(super::replace::resolve(base.tree_id))),
                Some(root(super::replace::resolve(current_commit.tree_id))),
                Some(root(super::replace::resolve(target_commit.tree_id))),
            ],
        )
        .map(|_| ());
    }
    ensure_merge_gitlinks_uniform(
        &bases,
        &commit_gitlink_entries(&current_commit)?,
        &commit_gitlink_entries(target_commit)?,
    )
    .map(|_| ())
}

/// ADR-MG-01 for a merge with any number of merge bases (MG-00 × MG-02).
///
/// EVERY merge base has to agree with both sides about every gitlink, not just
/// the one that happens to sort first: with a criss-cross history the fold
/// merges the bases against each other first, so a gitlink two bases disagree
/// about would be arbitrated inside the virtual ancestor — before the outer
/// merge ever looked at it. Asking the question once per base is the same
/// fail-closed rule applied to every input the merge actually reads, and it
/// runs BEFORE the fold writes anything.
///
/// Returns the pass-through set (identical for every base, since passing
/// requires all three sides to carry the same object id).
fn ensure_merge_gitlinks_uniform(
    bases: &[Commit],
    our_gitlinks: &GitlinkEntries,
    their_gitlinks: &GitlinkEntries,
) -> Result<GitlinkEntries, PullMergeError> {
    if bases.is_empty() {
        return ensure_gitlinks_not_arbitrated(
            "merge",
            &GitlinkEntries::new(),
            our_gitlinks,
            their_gitlinks,
        )
        .map_err(PullMergeError::GitlinkUnsupported);
    }
    let mut passthrough = GitlinkEntries::new();
    for base in bases {
        passthrough = ensure_gitlinks_not_arbitrated(
            "merge",
            &commit_gitlink_entries(base)?,
            our_gitlinks,
            their_gitlinks,
        )
        .map_err(PullMergeError::GitlinkUnsupported)?;
    }
    Ok(passthrough)
}

fn merge_completed_for_post_hook(summary: &PullMergeSummary) -> bool {
    !summary.dry_run
        && !summary.aborted
        && !summary.up_to_date
        && summary.conflicted_paths.is_empty()
        && (summary.commit.is_some() || summary.strategy == "squash")
}

async fn run_merge_for_pull_inner(
    // Pre-resolved and (when requested) signature-verified by
    // `run_merge_for_pull_with_options` BEFORE autostash/recovery mutations;
    // reusing the same loaded object keeps verify-and-merge TOCTOU-free.
    target_commit: Commit,
    upstream: &str,
    output: &OutputConfig,
    options: PullMergeOptions,
    // Validated by the caller BEFORE it mutated anything (MG-05, Codex R7/R8);
    // carried here so the engines never read the key a second time, where a
    // transient failure or a concurrent edit could refuse the merge after the
    // autostash had already saved and reset the tree (Codex R9).
    preflighted_rename_config: Option<MergeRenameConfig>,
    preflighted_default_driver: Option<String>,
    preflighted_external_drivers: SharedExternalMergeRuntime,
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
            conflict_kinds: Vec::new(),
            autostash: None,
        });
    };
    let current_commit: Commit =
        load_object(&current_commit_id).map_err(|error| PullMergeError::CurrentLoad {
            commit_id: current_commit_id.to_string(),
            detail: error.to_string(),
        })?;

    let bases = merge_base_commits(
        &current_commit,
        &target_commit,
        merge_options_will_fold(&options),
    )?;

    if bases.is_empty() && !options.allow_unrelated_histories {
        return Err(PullMergeError::UnrelatedHistories);
    }

    if merge_is_up_to_date(&bases, &target_commit) {
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
            conflict_kinds: Vec::new(),
            autostash: None,
        });
    }

    if merge_is_fast_forward(&bases, &current_commit, &options) {
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
            conflict_kinds: Vec::new(),
            autostash: None,
        });
    }

    // `--no-ff` cannot be combined with `--ff-only` (clap rejects the pair on
    // the pull surface). `ff_only` (flag or `merge.ff=only`) must reject only
    // a genuinely diverged history: a fast-forwardable `--squash`/`--no-commit`
    // merely skipped the fast-forward branch above and is allowed (Git accepts
    // `merge.ff=only` + `--squash` when the target is fast-forwardable).
    if options.ff_only && !merge_head_is_sole_base(&bases, &current_commit) {
        return Err(PullMergeError::NonFastForward {
            current: current_commit.id.to_string(),
            target: target_commit.id.to_string(),
        });
    }

    let merge_options = ThreeWayMergeOptions {
        message_override: options.message.clone(),
        merge_log: options.merge_log,
        squash: options.squash,
        no_commit: options.no_commit,
        skip_hooks: options.skip_hooks,
        dry_run: options.dry_run,
        favor: options.favor,
        allow_unrelated_histories: options.allow_unrelated_histories,
        fast_forwardable: merge_head_is_sole_base(&bases, &current_commit) && !options.no_ff,
        rename_config: preflighted_rename_config,
        merge_default_driver: preflighted_default_driver,
        external_merge_runtime: preflighted_external_drivers,
        output,
    };
    match options.strategy {
        Some(MergeStrategy::Ours) => {
            perform_ours_merge(current_commit, target_commit, upstream, merge_options).await
        }
        None => {
            perform_three_way_merge(
                current_commit,
                target_commit,
                bases,
                upstream,
                merge_options,
            )
            .await
        }
    }
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

#[derive(Clone)]
struct ThreeWayMergeOptions<'a> {
    message_override: Option<String>,
    merge_log: usize,
    squash: bool,
    no_commit: bool,
    skip_hooks: bool,
    /// Preview only: compute the outcome, write nothing (lore.md §1.3).
    dry_run: bool,
    /// Resolve otherwise-conflicting paths in favor of one side.
    favor: Option<MergeFavor>,
    /// Persisted in recovery state for unrelated-history restart.
    allow_unrelated_histories: bool,
    /// HEAD is an ancestor of the target and `--no-ff` was not asked for, so
    /// this merge only reaches the three-way engine because `--squash` or
    /// `--no-commit` skipped the fast-forward branch. The result is the target
    /// tree either way, so rename arbitration cannot change it — and Git,
    /// which takes its own fast-forward path here, never parses the rename
    /// config (measured: `git merge --squash` and `--no-commit` succeed with
    /// `merge.renames=not-a-bool`, while `--no-ff` on the same history fails).
    fast_forwardable: bool,
    /// The rename configuration the wrapper already validated, before the
    /// stale-sidecar recovery and the autostash it guards. Reading it again in
    /// the engine would reopen the window those refusals exist to close.
    rename_config: Option<MergeRenameConfig>,
    merge_default_driver: Option<String>,
    external_merge_runtime: SharedExternalMergeRuntime,
    output: &'a OutputConfig,
}

fn resolve_merge_message(
    current: ObjectHash,
    target: ObjectHash,
    upstream: &str,
    head_name: &str,
    message_override: Option<&String>,
    merge_log: usize,
) -> Result<String, PullMergeError> {
    match message_override {
        Some(message) => crate::command::merge_message::append_shortlog(
            message.clone(),
            current,
            target,
            upstream,
            merge_log,
        )
        .map_err(PullMergeError::History),
        None => crate::command::merge_message::default_message(
            current, target, upstream, head_name, merge_log,
        )
        .map_err(PullMergeError::History),
    }
}

/// Git's `ours` merge strategy records the target as a second parent while
/// keeping the current HEAD tree byte-for-byte. It is distinct from `-X ours`:
/// the latter keeps all non-conflicting target changes and favors ours only
/// where the default three-way merge would conflict.
async fn perform_ours_merge(
    current_commit: Commit,
    target_commit: Commit,
    upstream: &str,
    options: ThreeWayMergeOptions<'_>,
) -> Result<PullMergeSummary, PullMergeError> {
    if !options.dry_run {
        switch::ensure_clean_status(options.output)
            .await
            .map_err(|_| PullMergeError::DirtyWorktree)?;
    }

    let head_name = current_head_name().await?;
    let resolved_message = resolve_merge_message(
        current_commit.id,
        target_commit.id,
        upstream,
        &head_name,
        options.message_override.as_ref(),
        options.merge_log,
    )?;

    if options.dry_run {
        return Ok(PullMergeSummary {
            strategy: "ours".to_string(),
            old_commit: Some(current_commit.id.to_string()),
            commit: None,
            files_changed: 0,
            up_to_date: false,
            parents: Vec::new(),
            conflicted_paths: Vec::new(),
            aborted: false,
            continued: false,
            dry_run: true,
            would_conflict: false,
            conflict_kinds: Vec::new(),
            autostash: None,
        });
    }

    if options.squash {
        return Ok(PullMergeSummary {
            strategy: "squash".to_string(),
            old_commit: Some(current_commit.id.to_string()),
            commit: None,
            files_changed: 0,
            up_to_date: false,
            parents: Vec::new(),
            conflicted_paths: Vec::new(),
            aborted: false,
            continued: false,
            dry_run: false,
            would_conflict: false,
            conflict_kinds: Vec::new(),
            autostash: None,
        });
    }

    if options.no_commit {
        MergeState {
            head_name: head_name.clone(),
            orig_head: current_commit.id.to_string(),
            target: target_commit.id.to_string(),
            target_ref: upstream.to_string(),
            base: None,
            strategy: Some(MergeStrategy::Ours),
            allow_unrelated_histories: options.allow_unrelated_histories,
            skip_hooks: options.skip_hooks,
            conflicted_paths: Vec::new(),
            message: Some(resolved_message),
        }
        .save()?;
        return Ok(PullMergeSummary {
            strategy: "no-commit".to_string(),
            old_commit: Some(current_commit.id.to_string()),
            commit: None,
            files_changed: 0,
            up_to_date: false,
            parents: vec![current_commit.id.to_string(), target_commit.id.to_string()],
            conflicted_paths: Vec::new(),
            aborted: false,
            continued: false,
            dry_run: false,
            would_conflict: false,
            conflict_kinds: Vec::new(),
            autostash: None,
        });
    }

    let message = if !options.skip_hooks {
        run_pre_merge_commit_hook(options.output).await?;
        switch::ensure_clean_status(options.output)
            .await
            .map_err(|_| PullMergeError::DirtyWorktree)?;
        let message = run_merge_message_hooks(&resolved_message, options.output).await?;
        switch::ensure_clean_status(options.output)
            .await
            .map_err(|_| PullMergeError::DirtyWorktree)?;
        message
    } else {
        resolved_message
    };
    let merge_commit = build_merge_commit(
        current_commit.tree_id,
        vec![current_commit.id, target_commit.id],
        &format_commit_msg(&message, None),
    )
    .await?;
    save_object(&merge_commit, &merge_commit.id)
        .map_err(|error| PullMergeError::CommitSave(error.to_string()))?;
    update_head_with_reflog(&head_name, merge_commit.id, upstream, "ours").await?;
    reset_index_and_workdir_to_tree(&current_commit.tree_id)?;
    if !options.skip_hooks {
        run_advisory_repo_hook(RepoHook::PostCommit, &[], None, options.output).await;
    }

    Ok(PullMergeSummary {
        strategy: "ours".to_string(),
        old_commit: Some(current_commit.id.to_string()),
        commit: Some(merge_commit.id.to_string()),
        files_changed: 0,
        up_to_date: false,
        parents: vec![current_commit.id.to_string(), target_commit.id.to_string()],
        conflicted_paths: Vec::new(),
        aborted: false,
        continued: false,
        dry_run: false,
        would_conflict: false,
        conflict_kinds: Vec::new(),
        autostash: None,
    })
}

async fn perform_three_way_merge(
    current_commit: Commit,
    target_commit: Commit,
    base_commits: Vec<Commit>,
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
    // MG-03: a merge against ONE real base (or none) walks the three trees
    // incrementally, opening only the directories the sides disagree about.
    // The recursive fold of several bases (MG-02) has already read every input
    // to build its virtual ancestor, so that shape keeps the flattening path.
    if base_commits.len() <= 1
        && incremental_tree_walk_enabled()
        && let Some(summary) = perform_incremental_three_way_merge(
            current_commit.clone(),
            target_commit.clone(),
            base_commits.first(),
            head_name.clone(),
            upstream,
            options.clone(),
        )
        .await?
    {
        return Ok(summary);
    }
    // MG-05: the rename config is STRICT, so it is read before anything is
    // persisted — a multi-base fold materializes its virtual ancestor, and an
    // unparseable `merge.renames` must not leave those objects behind
    // (Codex R5 P1).
    let rename_config = three_way_rename_config(&options).await?;
    let (mut our_items, our_gitlinks) = commit_tree_split_for_merge(&current_commit)?;
    let (mut their_items, their_gitlinks) = commit_tree_split_for_merge(&target_commit)?;
    report_tree_walk_stats("flat", None);
    // ADR-MG-01 fail-closed gate: refuse before the first write (this runs
    // ahead of the `--dry-run` report as well, so the preview is honest) if any
    // submodule pointer diverged — for EVERY merge base, so the recursive fold
    // below can never end up arbitrating one either. Gitlinks all the inputs
    // agree on are carried into the result tree untouched instead of vanishing
    // from it.
    let passthrough_gitlinks =
        ensure_merge_gitlinks_uniform(&base_commits, &our_gitlinks, &their_gitlinks)?;
    // MG-02: a criss-cross history leaves several merge bases, none of them
    // better than the others. Fold them into one virtual ancestor (Git's
    // recursive strategy, `merge-ort.c:5313`) instead of arbitrarily picking
    // one, which reports conflicts the recursion resolves.
    // A conflict inside the virtual ancestor, and MG-06's rename-driven
    // content merges, are both rendered with the configured style — exactly as
    // Git renders one at any call depth. Resolved once, ahead of both, so a
    // merge that renames pays a single config read: an invalid
    // `merge.conflictStyle` stops the merge before anything is written.
    let conflict_style = conflict_style_from_config().await.map_err(|e| match e {
        ConflictStyleError::Invalid(value) => PullMergeError::InvalidConflictStyle(value),
        ConflictStyleError::Read(detail) => PullMergeError::ConflictStyleRead(detail),
    })?;
    let (base_items, mut virtual_blobs) = match base_commits.as_slice() {
        [] => (HashMap::new(), VirtualBlobs::new()),
        [base] => (commit_tree_split_for_merge(base)?.0, VirtualBlobs::new()),
        bases => {
            let base_ids: Vec<ObjectHash> = bases.iter().map(|base| base.id).collect();
            let ancestor = virtual_merge_base(
                &base_ids,
                &passthrough_gitlinks,
                !options.dry_run,
                conflict_style,
                &rename_config,
                options.merge_default_driver.as_deref(),
                options.external_merge_runtime.clone(),
            )?;
            (ancestor.items, ancestor.blobs)
        }
    };
    // MG-05: detect renames once per side and rewrite the base and the other
    // side onto the new path, so the ordinary three-way match below sees one
    // triple there (Git's `detect_regular_renames` + `process_renames`).
    let mut base_items = base_items;
    let rename_report = detect_and_apply_renames(
        &mut base_items,
        &mut our_items,
        &mut their_items,
        &rename_config,
        conflict_style,
        (
            df_branch_label(MergeSide::Ours, upstream).as_str(),
            upstream,
        ),
        &mut TreeMergeContext::top_level_with_external(
            !options.dry_run,
            options.favor,
            options.merge_default_driver.as_deref(),
            upstream,
            options.external_merge_runtime.clone(),
            &mut virtual_blobs,
        ),
    )?;

    // Under `--dry-run`, auto-merged blobs are computed in memory only
    // (persist=false) so the preview writes nothing to the object store —
    // under tiered storage a `save_object` would even upload to the remote.
    // The virtual ancestor obeys the same rule: its blobs stay in
    // `virtual_blobs` and its one-shot tree/commit are not materialized.
    let mut merge_result = merge_tree_items(
        &base_items,
        &our_items,
        &their_items,
        &mut TreeMergeContext::top_level_with_external(
            !options.dry_run,
            options.favor,
            options.merge_default_driver.as_deref(),
            upstream,
            options.external_merge_runtime.clone(),
            &mut virtual_blobs,
        ),
    )?;
    // MG-06: conflicts the ordinary match must not decide for itself. A
    // rename/rename(1to2)'s destinations look like one-sided adds to it, its
    // source like a clean delete, and a rename/delete whose content never
    // changed like a clean delete too — all three are PATH-level conflicts for
    // Git. Applied after the match so the paths carry the rename pass's
    // verdict, exactly as the pruned walk's `settle` does.
    for (path, kind) in &rename_report.forced {
        merge_result.merged_items.remove(path);
        merge_result.conflicts.retain(|(other, _)| other != path);
        merge_result.conflicts.push((path.clone(), *kind));
    }
    let merge_result = merge_result;
    // A rename THEIRS made moves a file ours still had at the old path: the
    // remapped comparison sees the same entry on both sides, so the move
    // itself has to be counted here (Git's diffstat shows it as one changed
    // file, `old => new`). A rename OURS made is already where ours has it.
    let files_changed = count_item_map_changes(&our_items, &merge_result.merged_items)
        + unseen_renames_by(
            &rename_report.decisions,
            MergeSide::Theirs,
            &our_items,
            &merge_result.merged_items,
            &rename_config,
            &virtual_blobs,
        );

    // Carry the agreed-on gitlinks into the merge result. Injected AFTER
    // `files_changed` so an untouched submodule is never reported as a changed
    // file, and never routed through `resolve_three_way` — the merge only
    // copies the object id all three sides already had.
    let mut merge_result = merge_result;
    for (path, gitlink) in &passthrough_gitlinks {
        merge_result.merged_items.insert(
            path.clone(),
            MergeTreeEntry {
                hash: *gitlink,
                mode: TreeItemMode::Commit,
            },
        );
    }

    // `--dry-run`: the outcome is fully known here — report it and stop before
    // the FIRST write (no merge state, index, worktree, HEAD, or reflog
    // mutation; no conflict markers). The conflict-style config is consulted
    // only for the multi-base fold above, whose ancestor content depends on it.
    // Git reports conflicts in path order (`process_entries` walks sorted
    // paths), and the unique-name suffixes follow that order too.
    merge_result
        .conflicts
        .sort_by(|(left, _), (right, _)| left.cmp(right));
    let placements = conflict_placements(
        &merge_result.conflicts,
        &df_occupied_names_if_needed(
            &[&base_items, &our_items, &their_items],
            &merge_result.merged_items,
            &merge_result.conflicts,
        ),
        upstream,
    );
    if options.dry_run {
        // A preview writes nothing, so there is no write preflight to wait for
        // — and it must still report the rename decisions the real merge would
        // make, or a declined rename would be invisible until the merge itself
        // (Codex R13 P2). `--json`/`--machine` stay silent, as always.
        announce_rename_notices(
            &rename_report.notes,
            &rename_report.limited,
            upstream,
            options.output,
        );
        let conflicted_paths: Vec<String> = placements
            .iter()
            .map(|(path, _, _)| path.display().to_string())
            .collect();
        let conflict_kinds = conflict_reports(&placements);
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
            conflict_kinds,
            autostash: None,
        });
    }

    // Resolve the final merge message ONCE, up front — `-m` override or the
    // generated default including the `merge.log` shortlog — so the conflict
    // and `--no-commit` states persist it and `merge --continue` replays it
    // instead of regenerating a plain message (which would drop `-m` and the
    // configured shortlog).
    let resolved_message = resolve_merge_message(
        current_commit.id,
        target_commit.id,
        upstream,
        &head_name,
        options.message_override.as_ref(),
        options.merge_log,
    )?;

    if !merge_result.conflicts.is_empty() {
        // For a single-base merge the style is resolved only here, on the
        // conflict path, so an invalid value cannot block a clean merge. A
        // multi-base merge already resolved it above (the fold's content
        // depends on it) — this second read then simply agrees with the first.
        let conflict_style = conflict_style_from_config().await.map_err(|e| match e {
            ConflictStyleError::Invalid(value) => PullMergeError::InvalidConflictStyle(value),
            ConflictStyleError::Read(detail) => PullMergeError::ConflictStyleRead(detail),
        })?;
        write_conflicted_merge_state(MergeConflictInput {
            head_name,
            message: resolved_message,
            squash: options.squash,
            upstream: upstream.to_string(),
            base: recorded_merge_base(&base_commits),
            allow_unrelated_histories: options.allow_unrelated_histories,
            skip_hooks: options.skip_hooks,
            ours: current_commit.id,
            theirs: target_commit.id,
            merged_items: merge_result.merged_items,
            placements: placements.clone(),
            base_items,
            our_items,
            their_items,
            conflict_style,
        })?;
        // Announced only now: the writer's preflight (untracked collisions,
        // symlink traversal, directory takeover) may still refuse the merge,
        // and Git prints nothing when it does. The rename notices wait for the
        // same moment and print first, so the decision that shaped the conflict
        // is read before the conflict itself.
        announce_rename_notices(
            &rename_report.notes,
            &rename_report.limited,
            upstream,
            options.output,
        );
        announce_df_conflicts(&placements, upstream, options.output);
        // rerere: record the preimage of each merge conflict just written and
        // replay a recorded resolution if one matches. A no-op unless
        // `rerere.enabled`; staging of a replayed file follows `rerere.autoUpdate`
        // (merge does not expose a per-invocation `--rerere-autoupdate`).
        if let Err(error) = crate::command::rerere::auto_update(false).await {
            tracing::warn!("rerere auto-update after merge conflict failed: {error}");
        }
        let paths = placements
            .iter()
            .map(|(path, _, _)| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(PullMergeError::Conflicts {
            paths,
            squash: options.squash,
        });
    }

    let current_index =
        Index::load(path::index()).map_err(|error| PullMergeError::IndexLoad(error.to_string()))?;
    let paths_to_write = worktree_paths_to_write(&merge_result.merged_items);
    let gitlink_paths: Vec<PathBuf> = passthrough_gitlinks.keys().cloned().collect();
    ensure_no_untracked_conflicts(&current_index, &paths_to_write, &gitlink_paths)?;
    // The flattening path commits and moves HEAD before it checks out, so a
    // write (or removal) it would refuse — through an ignored symlinked
    // directory — must be refused HERE, before its first write, as the
    // incremental path's checkout-first order does inherently. A tracked
    // symlink the result removes is allowed on the way to the paths written
    // after its removal (a symlink `foo` replaced by the directory `foo/`).
    let flat_removals: Vec<PathBuf> = current_index
        .tracked_files()
        .into_iter()
        .filter(|path| !merge_result.merged_items.contains_key(path))
        .filter(|path| !is_gitlink_index_path(&current_index, path).unwrap_or(false))
        .collect();
    refuse_symlink_traversal(&util::working_dir(), &paths_to_write, &flat_removals)?;
    // The preflight passed, so the merge will happen: the rename notices can be
    // printed now (Codex R12 P2 — a refused merge prints no rename decision).
    announce_rename_notices(
        &rename_report.notes,
        &rename_report.limited,
        upstream,
        options.output,
    );

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
            conflict_kinds: Vec::new(),
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
            base: recorded_merge_base(&base_commits).map(|base| base.to_string()),
            strategy: None,
            allow_unrelated_histories: options.allow_unrelated_histories,
            skip_hooks: options.skip_hooks,
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
            conflict_kinds: Vec::new(),
            autostash: None,
        });
    }

    let message = if !options.skip_hooks {
        run_pre_merge_commit_hook(options.output).await?;
        switch::ensure_clean_status(options.output)
            .await
            .map_err(|_| PullMergeError::DirtyWorktree)?;
        // The hook may create an untracked path after the merge result was
        // computed. Recheck before saving the commit or moving HEAD; relying
        // on the reset-time guard would detect the collision too late and
        // leave a partially completed merge.
        ensure_no_untracked_conflicts(&current_index, &paths_to_write, &gitlink_paths)?;
        // A hook may also plant an IGNORED symlink (invisible to the untracked
        // scan) where the merge writes: recheck the traversal too, before HEAD.
        refuse_symlink_traversal(&util::working_dir(), &paths_to_write, &flat_removals)?;
        let message = run_merge_message_hooks(&resolved_message, options.output).await?;
        switch::ensure_clean_status(options.output)
            .await
            .map_err(|_| PullMergeError::DirtyWorktree)?;
        ensure_no_untracked_conflicts(&current_index, &paths_to_write, &gitlink_paths)?;
        refuse_symlink_traversal(&util::working_dir(), &paths_to_write, &flat_removals)?;
        message
    } else {
        resolved_message
    };
    let merge_commit = build_merge_commit(
        tree_id,
        vec![current_commit.id, target_commit.id],
        &format_commit_msg(&message, None),
    )
    .await?;
    save_object(&merge_commit, &merge_commit.id)
        .map_err(|error| PullMergeError::CommitSave(error.to_string()))?;
    update_head_with_reflog(&head_name, merge_commit.id, upstream, "three-way").await?;
    reset_index_and_workdir_to_tree(&tree_id)?;
    if !options.skip_hooks {
        run_advisory_repo_hook(RepoHook::PostCommit, &[], None, options.output).await;
    }

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
        conflict_kinds: Vec::new(),
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
    /// Git skips MERGE_HEAD even when a squash has unresolved conflicts.
    squash: bool,
    /// Resolved merge message (see [`MergeState::message`]).
    message: String,
    upstream: String,
    /// Real common ancestor, or `None` for the virtual empty base used by an
    /// unrelated-history merge.
    base: Option<ObjectHash>,
    allow_unrelated_histories: bool,
    skip_hooks: bool,
    ours: ObjectHash,
    theirs: ObjectHash,
    merged_items: HashMap<PathBuf, MergeTreeEntry>,
    /// Where each conflict is unmerged (MG-04: a D/F file at its unique name)
    /// — computed ONCE by the engine, while it still saw every directory
    /// entry and every input path, and used for the announcement, the
    /// unmerged-path list, the stages and the working-tree writes alike.
    placements: Vec<(PathBuf, ConflictKind, Option<PathBuf>)>,
    base_items: HashMap<PathBuf, MergeTreeEntry>,
    our_items: HashMap<PathBuf, MergeTreeEntry>,
    their_items: HashMap<PathBuf, MergeTreeEntry>,
    /// Marker style for conflicted paths, resolved from `merge.conflictStyle`.
    conflict_style: diffy::ConflictStyle,
}

fn write_conflicted_merge_state(input: MergeConflictInput) -> Result<(), PullMergeError> {
    let current_index =
        Index::load(path::index()).map_err(|error| PullMergeError::IndexLoad(error.to_string()))?;

    // MG-04: a D/F conflict's file is placed at its `unique_path`; every other
    // conflict stays at its own path. `placements` is the single source for
    // the unmerged path list, the untracked-collision check, the index stages
    // and the working-tree writes below.
    let placements = input.placements;
    let conflict_paths: Vec<PathBuf> = placements.iter().map(|(path, _, _)| path.clone()).collect();
    let paths_to_write: Vec<PathBuf> = worktree_paths_to_write(&input.merged_items)
        .into_iter()
        .chain(conflict_paths.iter().cloned())
        .collect();
    let gitlink_paths: Vec<PathBuf> = input
        .merged_items
        .iter()
        .filter(|(_, entry)| entry.mode == TreeItemMode::Commit)
        .map(|(path, _)| path.clone())
        .collect();
    ensure_no_untracked_conflicts(&current_index, &paths_to_write, &gitlink_paths)?;
    let conflict_set: HashSet<PathBuf> = conflict_paths.iter().cloned().collect();
    // What this merge REMOVES: tracked now, absent from the result. Nothing
    // else is ever unlinked — a path only the history knew (untracked or
    // recreated since) is not the merge's to delete.
    let removals: Vec<PathBuf> = current_index
        .tracked_files()
        .into_iter()
        .filter(|path| !conflict_set.contains(path) && !input.merged_items.contains_key(path))
        .filter(|path| !is_gitlink_index_path(&current_index, path).unwrap_or(false))
        .collect();
    refuse_symlink_traversal(&util::working_dir(), &paths_to_write, &removals)?;

    let workdir = util::working_dir();
    let marker_eol = conflict_marker_eol();
    let theirs_abbrev = short_object_id(&input.theirs);

    let mut index = Index::new();
    for (path, entry) in &input.merged_items {
        add_blob_index_entry(&mut index, path, *entry, 0)?;
    }
    for (path, kind, original) in &placements {
        // A moved modify/delete conflict is still looked up where the sides
        // HAD the file; its stages are written at the moved path.
        let source = original.as_ref().unwrap_or(path);
        if let ConflictKind::DirectorySplit { content } = kind {
            // Git records the addition as fully resolved (stage 0) while the
            // directory-level decision keeps the merge itself unclean.
            add_blob_index_entry(&mut index, path, *content, 0)?;
            continue;
        }
        if let ConflictKind::FileDirectory {
            file,
            file_side,
            base_file,
            ..
        } = kind
        {
            // Git's stage layout for the moved file (`merge-ort.c:4100-4198`):
            // the file's own side carries it, the base's FILE (if any) is
            // stage 1, and every directory-side stage is zeroed — absent here.
            if let Some(base_file) = base_file {
                add_blob_index_entry(&mut index, path, *base_file, 1)?;
            }
            let stage = match file_side {
                MergeSide::Ours => 2,
                MergeSide::Theirs => 3,
            };
            add_blob_index_entry(&mut index, path, *file, stage)?;
            continue;
        }
        // An empty-directory marker (the flat view's `Tree` entry) is not a
        // stage: a base that held an empty `foo/` next to two added files
        // `foo` is an add/add conflict with no stage 1.
        let stage_entry = |items: &HashMap<PathBuf, MergeTreeEntry>| {
            items
                .get(source)
                .copied()
                .filter(|entry| entry.mode != TreeItemMode::Tree)
        };
        if let Some(entry) = stage_entry(&input.base_items) {
            add_blob_index_entry(&mut index, path, entry, 1)?;
        }
        if let Some(entry) = stage_entry(&input.our_items) {
            add_blob_index_entry(&mut index, path, entry, 2)?;
        }
        if let Some(entry) = stage_entry(&input.their_items) {
            add_blob_index_entry(&mut index, path, entry, 3)?;
        }
    }

    let state = MergeState {
        head_name: input.head_name,
        orig_head: input.ours.to_string(),
        target: input.theirs.to_string(),
        target_ref: input.upstream,
        base: input.base.map(|base| base.to_string()),
        strategy: None,
        allow_unrelated_histories: input.allow_unrelated_histories,
        skip_hooks: input.skip_hooks,
        conflicted_paths: conflict_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        message: Some(input.message),
    };
    // Git builtin/merge.c writes merge state only for a non-squash result.
    // A squash's staged resolution must be committed with one parent.
    if !input.squash {
        state.save()?;
    }

    if let Err(error) = index.save(path::index()) {
        if !input.squash {
            let _ = MergeState::cleanup();
        }
        return Err(PullMergeError::IndexSave(error.to_string()));
    }

    // Remove what the result no longer has BEFORE writing what it has: a D/F
    // collision turns our file `foo` into the directory `foo/` (MG-04), and
    // `foo/bar.txt` cannot be created while the file is still there. (The
    // same order `reset_workdir_tracked_only` uses.)
    for path in &removals {
        let full_path = workdir.join(path);
        // `symlink_metadata` so a DANGLING tracked link is removed too.
        if fs::symlink_metadata(&full_path).is_ok_and(|meta| !meta.is_dir()) {
            fs::remove_file(&full_path).map_err(|error| {
                PullMergeError::WorkdirReset(format!(
                    "failed to remove {}: {error}",
                    path.display()
                ))
            })?;
            prune_empty_parents(&workdir, path);
        }
    }

    for (path, entry) in &input.merged_items {
        // Submodule working trees are not materialized by Libra: a pass-through
        // gitlink is an index/tree fact only (ADR-MG-01).
        if entry.mode == TreeItemMode::Commit {
            continue;
        }
        let blob: Blob = load_object(&entry.hash).map_err(|error| {
            PullMergeError::WorkdirReset(format!(
                "failed to load merged blob {} for '{}': {error}",
                entry.hash,
                path.display()
            ))
        })?;
        write_workdir_entry(&workdir, path, entry.mode, &blob.data)
            .map_err(PullMergeError::WorkdirReset)?;
    }

    for (path, kind, original) in &placements {
        // A moved file is left in the tree verbatim — Git's "Version HEAD of
        // foo~HEAD left in tree" — whether it is a one-sided add or the
        // surviving side of a modify/delete conflict.
        if original.is_some()
            && let Some(entry) = moved_file_content(kind)
        {
            let blob: Blob = load_object(&entry.hash).map_err(|error| {
                PullMergeError::WorkdirReset(format!(
                    "failed to load moved blob {} for '{}': {error}",
                    entry.hash,
                    path.display()
                ))
            })?;
            write_workdir_entry(&workdir, path, entry.mode, &blob.data)
                .map_err(PullMergeError::WorkdirReset)?;
            continue;
        }
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

/// The entry a D/F-moved conflict leaves at its unique path: the file itself,
/// with its mode (a moved symbolic link stays a symbolic link, as Git's
/// checkout of `foo~HEAD` does).
fn moved_file_content(kind: &ConflictKind) -> Option<MergeTreeEntry> {
    match kind {
        ConflictKind::FileDirectory { file, .. } => Some(*file),
        ConflictKind::OursModifiedTheirsDeleted { ours } => Some(MergeTreeEntry {
            hash: *ours,
            mode: TreeItemMode::Blob,
        }),
        ConflictKind::TheirsModifiedOursDeleted { theirs } => Some(MergeTreeEntry {
            hash: *theirs,
            mode: TreeItemMode::Blob,
        }),
        ConflictKind::BothChanged { .. } => None,
        // MG-06: not a D/F relocation, so never consulted here (`original` is
        // `None` for every rename-driven conflict) — the 1to2 destinations get
        // their already-merged content from `write_conflict_markers`.
        ConflictKind::RenameMerged { .. } => None,
        // A split-directory conflict is not relocated either; its optional
        // resolved stage-0 entry is handled directly by the writer.
        ConflictKind::DirectorySplit { .. } => None,
    }
}

/// Write a tracked entry into the working tree with its TYPE and MODE: a
/// symbolic link as a link (Unix), an executable as `0755`, anything else as
/// `0644`. Every merge write goes through here — the conflict path's merged
/// files, the D/F-moved entries and the checkout in
/// [`reset_workdir_tracked_only`] — because the write REPLACES the directory
/// entry (a hard-linked alias keeps the old content, as Git leaves it), which
/// also means the new file's mode has to be set explicitly rather than
/// inherited from whatever was there before.
fn write_workdir_entry(
    workdir: &Path,
    relative: &Path,
    mode: TreeItemMode,
    content: &[u8],
) -> Result<(), String> {
    if mode == TreeItemMode::Link {
        return write_workdir_symlink(workdir, relative, content);
    }
    write_workdir_file(workdir, relative, content)?;
    apply_file_mode(&workdir.join(relative), mode)
}

/// A symbolic link, target bytes verbatim (never UTF-8 validated). Windows has
/// no symlinks here: the target text is left in a file, as `checkout` does.
fn write_workdir_symlink(workdir: &Path, relative: &Path, content: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        refuse_symlink_components(workdir, relative)?;
        let full = workdir.join(relative);
        if let Some(parent) = full.parent() {
            // Same ancestor rule as the file writer: an ignored file standing
            // where a directory must go is replaced (Codex R10).
            clear_ancestor_files(workdir, relative)?;
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
        }
        clear_write_target(&full)?;
        std::os::unix::fs::symlink(OsStr::from_bytes(content), &full).map_err(|error| {
            format!(
                "failed to create the symbolic link {}: {error}",
                full.display()
            )
        })
    }
    #[cfg(not(unix))]
    {
        write_workdir_file(workdir, relative, content)
    }
}

/// `0755` for an executable entry, `0644` otherwise (Unix; a no-op elsewhere).
/// Explicit because every write creates the file anew.
fn apply_file_mode(path: &Path, mode: TreeItemMode) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let bits = if mode == TreeItemMode::BlobExecutable {
            0o755
        } else {
            0o644
        };
        fs::set_permissions(path, fs::Permissions::from_mode(bits))
            .map_err(|error| format!("failed to chmod {}: {error}", path.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// Whether THIS worktree has a merge in progress — the request-scoped sidecar
/// probe `pull` runs BEFORE fetching (W2 r8 #2): the operation-log slot only
/// excludes CONCURRENT control actions, while a persisted conflicted merge is
/// state, and a pull that fetched first would mutate FETCH_HEAD and remote
/// refs before discovering it has nowhere to integrate.
pub(crate) fn merge_in_progress() -> Result<bool, String> {
    let gitdir = util::request_worktree_gitdir()
        .map_err(|error| format!("cannot resolve this worktree's gitdir: {error}"))?;
    Ok(gitdir.join("merge-state.json").exists())
}

/// Preflight for the merge control actions (W2 r5 #1, r6 #4): the
/// held-autostash sidecar must be READABLE before HEAD/index/worktree are
/// restored — an unreadable one would otherwise surface after `abort` had
/// already deleted the merge state, leaving the only stash reference
/// unparseable. The SNAPSHOT is returned and later CONSUMED, so the document
/// the control preflighted is the one applied: a sidecar replaced between
/// the preflight and the consumption is preserved by the identity-checked
/// cleanup, never adopted or deleted.
fn preflight_held_autostash() -> Result<Option<AutostashSnapshot>, MergeError> {
    snapshot_held_autostash().map_err(MergeError::StateLoad)
}

/// `files_changed` for a finalized merge, counting a renamed file ONCE.
///
/// `--continue` compares the pre-merge tree with the index, where a rename
/// looks like a delete plus an add — so it reported 2 for the same merge the
/// preview and `--no-commit` reported as 1 (Codex R18). Git's diffstat renders
/// that pair as one `old => new` line, and detection here uses the same engine
/// and the same `merge.renames` / `merge.renameLimit` configuration the merge
/// itself used, so the two numbers agree by construction. Detection off, or a
/// merge with no renames, leaves the count exactly as it was.
async fn count_changes_following_renames(
    before: &HashMap<PathBuf, MergeTreeEntry>,
    after: &HashMap<PathBuf, MergeTreeEntry>,
) -> Result<usize, MergeError> {
    let raw = count_item_map_changes(before, after);
    // A rename can only collapse a pair, so unless the comparison shows BOTH a
    // removal and an addition there is nothing to detect.
    let removed = before.keys().any(|path| !after.contains_key(path));
    let added = after.keys().any(|path| !before.contains_key(path));
    if !removed || !added {
        return Ok(raw);
    }
    // This number is a REPORT. A merge that already started must never fail to
    // finalize because of it, so an unusable rename configuration falls back to
    // the plain count instead of failing the command — Git finalizes with
    // `git commit`, which never parses `merge.renames` at all. The earlier
    // guard covered `-s ours` only while the staged result equalled HEAD; a
    // user who moves a tracked file before `--continue` reintroduced the
    // failure (Codex R19 then R20).
    let Ok(config) = merge_rename_config().await else {
        return Ok(raw);
    };
    if !config.enabled {
        return Ok(raw);
    }
    let no_virtual_blobs = VirtualBlobs::new();
    let mut reader = MergeRenameReader::new();
    let detected =
        detect_side_renames_for_report(before, after, &config, &no_virtual_blobs, &mut reader);
    // Each pair was counted as a delete AND an add; Git counts it once.
    Ok(raw.saturating_sub(detected.matches.len()))
}

async fn run_merge_continue(
    output: &OutputConfig,
    skip_hooks_for_continue: bool,
    message_override: Option<String>,
) -> Result<MergeOutput, MergeError> {
    refuse_ambiguous_common_merge_state()?;
    let held_autostash = preflight_held_autostash()?;
    let state = MergeState::load_required()?;
    ensure_no_unstaged_changes_for_continue()?;
    let skip_hooks = state.skip_hooks || skip_hooks_for_continue;
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
    // Pass-through gitlinks were staged at stage 0 when the conflict was
    // written (ADR-MG-01), so the index already carries them into the result
    // tree; the pre-merge snapshot has to include them too, or an untouched
    // submodule would be counted as a changed file.
    let (mut original_items, original_gitlinks) = commit_tree_split(&original_commit)?;
    for (path, gitlink) in original_gitlinks {
        original_items.insert(
            path,
            MergeTreeEntry {
                hash: gitlink,
                mode: TreeItemMode::Commit,
            },
        );
    }
    let index_items = index_tree_items(&index)?;
    let files_changed = count_changes_following_renames(&original_items, &index_items).await?;
    let tree_id = create_tree_from_items_map(&index_items).map_err(MergeError::TreeCreate)?;
    // A `-m` given to `--continue` wins: it is the only way to set the message
    // of a conflicted merge, since Libra finalizes without opening an editor.
    // Otherwise replay the message resolved at merge start (`-m` or the
    // generated default with the `merge.log` shortlog); states written by older
    // binaries carry no message and keep the plain form.
    let message = message_override
        .or_else(|| state.message.clone())
        .unwrap_or_else(|| format!("Merge {} into {}", state.target_ref, state.head_name));
    let message = if !skip_hooks {
        run_pre_merge_commit_hook(output).await?;
        ensure_no_unstaged_changes_for_continue()?;
        let message = run_merge_message_hooks(&message, output).await?;
        ensure_no_unstaged_changes_for_continue()?;
        message
    } else {
        message
    };
    let merge_commit = build_merge_commit(
        tree_id,
        vec![orig_head, target],
        &format_commit_msg(&message, None),
    )
    .await?;
    save_object(&merge_commit, &merge_commit.id)
        .map_err(|error| MergeError::CommitSave(error.to_string()))?;
    let strategy = match state.strategy {
        Some(MergeStrategy::Ours) => "ours",
        None => "three-way",
    };
    update_head_with_reflog(
        &state.head_name,
        merge_commit.id,
        &state.target_ref,
        strategy,
    )
    .await?;
    reset_index_and_workdir_to_tree(&tree_id)?;
    MergeState::cleanup()?;
    // Merge concluded: re-apply the held autostash onto the finalized tree
    // (clean → dropped; conflict → promoted to the stash list with a notice).
    // The control's pre-mutation snapshot is what is consumed (W2 r6 #4).
    let autostash = match held_autostash {
        Some(snapshot) => resolve_pending_autostash_with(output, snapshot, false).await,
        None => None,
    };
    if !skip_hooks {
        run_advisory_repo_hook(RepoHook::PostCommit, &[], None, output).await;
    }

    let summary = PullMergeSummary {
        strategy: strategy.to_string(),
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
        conflict_kinds: Vec::new(),
        autostash,
    };
    if !skip_hooks {
        run_advisory_repo_hook(RepoHook::PostMerge, &["0".to_string()], None, output).await;
    }
    Ok(summary)
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
    // MG-04: a D/F conflict placed a file at `<path>~<branch>` — a path the
    // pre-merge tree never tracked, so the reset above leaves it behind. Every
    // recorded conflict path the restored index does not track is such a
    // moved file (a content conflict's path is tracked and was just restored).
    let restored =
        Index::load(path::index()).map_err(|error| MergeError::IndexLoad(error.to_string()))?;
    let workdir = util::working_dir();
    for conflicted in &state.conflicted_paths {
        let relative = PathBuf::from(conflicted);
        if restored.get(path_to_index_key(&relative)?, 0).is_some() {
            continue;
        }
        let full = workdir.join(&relative);
        // `is_file()` FOLLOWS symlinks: a moved DANGLING link (or one pointing
        // at a directory) would be left behind, and the restored index does
        // not track it, so nothing else would ever remove it.
        if fs::symlink_metadata(&full).is_ok_and(|meta| !meta.is_dir()) {
            fs::remove_file(&full).map_err(|error| {
                MergeError::WorkdirReset(format!(
                    "failed to remove the moved conflict file {}: {error}",
                    relative.display()
                ))
            })?;
            prune_empty_parents(&workdir, &relative);
        }
    }
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
/// and are not replayed (documented limitation). The recovery-critical
/// unrelated-history permission is persisted and replayed below.
async fn run_merge_restart(output: &OutputConfig) -> Result<MergeOutput, MergeError> {
    refuse_ambiguous_common_merge_state()?;
    let _held_autostash = preflight_held_autostash()?;
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
        allow_unrelated_histories: state.allow_unrelated_histories,
        skip_hooks: state.skip_hooks,
        ..PullMergeOptions::default()
    };
    run_merge_for_pull_with_options(&target, &target_ref, output, options).await
}

/// Refuse a control action on a COMMON-storage merge sidecar whose owner cannot
/// be established (§C.4.3) — same rule and same reasoning as revert's.
fn refuse_ambiguous_common_merge_state() -> Result<(), MergeError> {
    let scope = crate::internal::worktree_scope::WorktreeScope::for_request();
    if scope.is_linked() {
        return Ok(());
    }
    if !crate::command::maintenance::repository_had_linked_worktrees() {
        return Ok(());
    }
    let gitdir = util::request_worktree_gitdir().map_err(|error| {
        MergeError::StateLoad(format!(
            "cannot resolve this worktree's gitdir to check for ambiguous shared state: {error}"
        ))
    })?;
    let sidecar = gitdir.join("merge-state.json");
    if !sidecar.exists() {
        return Ok(());
    }
    // W2: a sidecar whose writer recorded main's scope is PROVEN main's and
    // stays operable; only an unmarked (old-binary) file keeps W1's guess.
    match crate::internal::sequencer::sidecar_recorded_owner(&sidecar) {
        Ok(Some(owner)) if owner.is_empty() => return Ok(()),
        Ok(_) => {}
        Err(error) => return Err(MergeError::StateLoad(error)),
    }
    Err(MergeError::StateLoad(format!(
        "a merge state file exists at '{}' in COMMON storage, and this repository has \
         linked-worktree history, so it cannot be proven to be the main worktree's — \
         continuing or aborting it would reset this worktree from another worktree's state. \
         Inspect it with `libra worktree doctor`; remove it manually once you have confirmed \
         it is stale.",
        sidecar.display()
    )))
}

async fn run_merge_abort(output: &OutputConfig) -> Result<MergeOutput, MergeError> {
    refuse_ambiguous_common_merge_state()?;
    let held_autostash = preflight_held_autostash()?;
    let state = MergeState::load_required()?;
    let orig_head = restore_pre_merge_state(&state, "abort").await?;
    // The held autostash re-applies onto the restored pre-merge tree (clean
    // by construction — it was taken on that very tree; the conflict fallback
    // still guards the path). The SNAPSHOT taken before the restore is what
    // is consumed (W2 r6 #4): a sidecar replaced in between is preserved by
    // the identity-checked cleanup, never adopted by this control.
    let autostash = match held_autostash {
        Some(snapshot) => resolve_pending_autostash_with(output, snapshot, false).await,
        None => None,
    };

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
        conflict_kinds: Vec::new(),
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

/// EVERY merge base of `lhs` and `rhs`, in the ascending-hex order
/// `merge_base::merge_bases` guarantees — which is also the order the recursive
/// virtual ancestor folds them in ([`virtual_base_fold_order`]).
///
/// Empty when the two share no history. More than one means a criss-cross
/// history, which MG-02 resolves by folding them rather than by picking one.
///
/// `will_fold` is whether several bases would actually be FOLDED; when it is
/// set the width ceiling is enforced here, on the bare ids, before a single base
/// commit or tree is loaded — the ceiling exists to bound work, so it has to
/// fire before the work starts. Callers pass [`merge_options_will_fold`]:
/// `-s ours` never folds, and a diverged `--ff-only` merge is refused as
/// non-fast-forward before any fold could start — neither may be refused for
/// width instead.
fn merge_base_commits(
    lhs: &Commit,
    rhs: &Commit,
    will_fold: bool,
) -> Result<Vec<Commit>, PullMergeError> {
    let base_ids = merge_base::merge_bases(&lhs.id, &rhs.id).map_err(|error| {
        PullMergeError::History(format!("failed to compute merge base: {error}"))
    })?;
    if will_fold {
        ensure_virtual_ancestor_width(base_ids.len())?;
    }
    base_ids
        .into_iter()
        .map(|base_id| {
            load_object::<Commit>(&base_id).map_err(|error| PullMergeError::ObjectLoad {
                object_id: base_id.to_string(),
                detail: format!("failed to load merge base: {error}"),
            })
        })
        .collect()
}

async fn apply_fast_forward_merge(
    target_commit: Commit,
    target_branch_name: &str,
    output: &OutputConfig,
) -> Result<(), PullMergeError> {
    switch::ensure_clean_status(output)
        .await
        .map_err(|_| PullMergeError::DirtyWorktree)?;
    let (target_items, target_gitlinks) = commit_tree_split(&target_commit)?;
    let current_index =
        Index::load(path::index()).map_err(|error| PullMergeError::IndexLoad(error.to_string()))?;
    let paths_to_write: Vec<PathBuf> = target_items.keys().cloned().collect();
    // A fast-forward materializes the target tree, gitlinks included: a plain
    // file sitting exactly at a submodule path would be replaced by `restore`'s
    // directory placeholder, so it has to be refused here, before HEAD moves.
    let gitlink_paths: Vec<PathBuf> = target_gitlinks.keys().cloned().collect();
    ensure_no_untracked_conflicts(&current_index, &paths_to_write, &gitlink_paths)?;

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

    // The restore below deliberately runs AFTER the pointers move, so anything
    // it would refuse has to be caught here — otherwise the branch ends up
    // ahead of the index and working tree. Most visibly: a materialized
    // submodule directory the target tree no longer declares (ADR-MG-01).
    restore::preflight_worktree_restore_to_commit(&target_commit.id)
        .await
        .map_err(|error| PullMergeError::Restore(error.to_string()))?;

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
                        Head::update_result_with_conn(txn, Head::Detached(target_commit.id), None)
                            .await
                            .map_err(|error| sea_orm::DbErr::Custom(error.to_string()))?;
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

/// The gitlink (`160000`) entries of one three-way merge input, keyed by
/// worktree-relative path.
///
/// Submodule content is never merged (ADR-MG-01), so gitlinks are held here
/// instead of in the mergeable entry maps. Keeping them addressable — rather
/// than dropping them during tree flattening, as `merge` and `rebase` used to —
/// is what lets [`ensure_gitlinks_not_arbitrated`] tell "the merge has to make
/// a decision about this submodule" (refused) from "all three sides already
/// agree" (carried through untouched).
pub(crate) type GitlinkEntries = BTreeMap<PathBuf, ObjectHash>;

/// A three-way merge was refused because it would have had to arbitrate a
/// gitlink. Produced by [`ensure_gitlinks_not_arbitrated`] and rendered into
/// each consumer's own error type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitlinkNotSupported {
    /// The operation the user asked for: `merge`, `rebase`, `cherry-pick`.
    pub(crate) operation: &'static str,
    /// The gitlink path that would need a merge decision.
    pub(crate) path: PathBuf,
}

impl std::fmt::Display for GitlinkNotSupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} would have to merge the submodule (gitlink) entry '{}': Libra does not support submodules",
            self.operation,
            self.path.display()
        )
    }
}

/// Split a flattened tree listing into its mergeable entries and its gitlinks.
///
/// The mergeable half keeps exactly the entries the three-way engine used to
/// see (blobs, executables, symlinks); the gitlink half replaces the silent
/// `filter_map` drop that made a submodule vanish from the merge result.
pub(crate) fn split_gitlink_entries(
    items: Vec<(PathBuf, ObjectHash, TreeItemMode)>,
) -> (HashMap<PathBuf, MergeTreeEntry>, GitlinkEntries) {
    let mut mergeable = HashMap::new();
    let mut gitlinks = GitlinkEntries::new();
    for (path, hash, mode) in items {
        if mode == TreeItemMode::Commit {
            gitlinks.insert(path, hash);
        } else {
            mergeable.insert(path, MergeTreeEntry { hash, mode });
        }
    }
    (mergeable, gitlinks)
}

/// Fail-closed gitlink guard shared by every three-way consumer — `merge`,
/// `rebase` and `cherry-pick` (ADR-MG-01, single source of truth per GC-02).
///
/// Libra is a monorepo client and never merges submodule content. The two
/// tiers are:
///
/// 1. a gitlink the merge would have to *arbitrate* — any side whose object id
///    differs from the base, including a side that added or removed it — is
///    refused, and the caller must surface the refusal before touching the
///    index or the working tree; and
/// 2. a gitlink all three sides already agree on is returned as pass-through,
///    so the caller can carry the entry into the merge result byte-for-byte
///    without ever making a decision about it.
///
/// Returns the pass-through set on success, or the first arbitrated path
/// (scanned in sorted order, so the reported path is deterministic).
pub(crate) fn ensure_gitlinks_not_arbitrated(
    operation: &'static str,
    base: &GitlinkEntries,
    ours: &GitlinkEntries,
    theirs: &GitlinkEntries,
) -> Result<GitlinkEntries, GitlinkNotSupported> {
    let mut paths: BTreeSet<&PathBuf> = base.keys().collect();
    paths.extend(ours.keys());
    paths.extend(theirs.keys());

    let mut passthrough = GitlinkEntries::new();
    for path in paths {
        match (base.get(path), ours.get(path), theirs.get(path)) {
            (Some(base_oid), Some(our_oid), Some(their_oid))
                if base_oid == our_oid && base_oid == their_oid =>
            {
                passthrough.insert(path.clone(), *base_oid);
            }
            _ => {
                return Err(GitlinkNotSupported {
                    operation,
                    path: path.clone(),
                });
            }
        }
    }
    Ok(passthrough)
}

/// Conservative pre-mutation gitlink gate for MULTI-STEP replays (`rebase`,
/// `cherry-pick`).
///
/// A per-step [`ensure_gitlinks_not_arbitrated`] cannot run before the sequence
/// starts: each step's "ours" side only exists once the previous step has been
/// applied, so by the time step N would be refused the sequence has already
/// moved HEAD, written its state sidecar, and possibly created commits. This
/// gate asks a stronger question up front instead — every input tree of the
/// WHOLE sequence must record the same object id for a given gitlink path —
/// which is the only shape in which no individual step can end up arbitrating
/// one (ADR-MG-01).
///
/// Deliberately conservative: a sequence whose inputs disagree is refused
/// before the first write even in the rare shape where every individual step
/// would have turned out fine. Refusing up front beats stopping half-applied.
pub(crate) fn ensure_gitlinks_uniform_across_inputs(
    operation: &'static str,
    inputs: &[GitlinkEntries],
) -> Result<(), GitlinkNotSupported> {
    let mut paths: BTreeSet<&PathBuf> = BTreeSet::new();
    for input in inputs {
        paths.extend(input.keys());
    }
    for path in paths {
        let mut pointers = inputs.iter().map(|input| input.get(path));
        let first = pointers.next().unwrap_or(None);
        if first.is_none() || !pointers.all(|pointer| pointer == first) {
            return Err(GitlinkNotSupported {
                operation,
                path: path.clone(),
            });
        }
    }
    Ok(())
}

/// The gitlink entries of `commit`'s tree.
pub(crate) fn commit_gitlink_entries(commit: &Commit) -> Result<GitlinkEntries, PullMergeError> {
    // Through the FALLIBLE flattener: a missing or corrupt nested tree must
    // come back as an error, not as `TreeExt::load`'s panic (MG-04 R6).
    let tree: Tree = load_object(&commit.tree_id).map_err(|error| PullMergeError::TreeLoad {
        tree_id: commit.tree_id.to_string(),
        detail: error.to_string(),
    })?;
    Ok(split_gitlink_entries(flat_items_with_empty_dirs(&tree)?).1)
}

/// Collect the gitlink entries of a commit's tree without flattening the
/// mergeable half — used by consumers that take one side from the index rather
/// than from a tree (`cherry-pick`).
pub(crate) fn tree_gitlink_entries(tree: &Tree) -> GitlinkEntries {
    tree.get_plain_items_with_mode()
        .into_iter()
        .filter(|(_, _, mode)| *mode == TreeItemMode::Commit)
        .map(|(path, hash, _)| (path, hash))
        .collect()
}

/// Gitlink entries recorded at stage 0 of an index — the "ours" side for
/// consumers that apply onto the index (`cherry-pick`).
pub(crate) fn index_gitlink_entries(index: &Index) -> GitlinkEntries {
    let mut gitlinks = GitlinkEntries::new();
    for path in index.tracked_files() {
        let Some(key) = path.to_str() else { continue };
        if let Some(entry) = index.get(key, 0)
            && entry.mode & 0o170000 == 0o160000
        {
            gitlinks.insert(path.clone(), entry.hash);
        }
    }
    gitlinks
}

/// Flatten a commit tree into the mergeable entries plus the gitlinks it
/// carries (see [`split_gitlink_entries`]).
fn commit_tree_split(
    commit: &Commit,
) -> Result<(HashMap<PathBuf, MergeTreeEntry>, GitlinkEntries), PullMergeError> {
    let tree: Tree = load_object(&commit.tree_id).map_err(|error| PullMergeError::TreeLoad {
        tree_id: commit.tree_id.to_string(),
        detail: error.to_string(),
    })?;
    Ok(split_gitlink_entries(tree.get_plain_items_with_mode()))
}

fn commit_tree_items(commit: &Commit) -> Result<HashMap<PathBuf, MergeTreeEntry>, PullMergeError> {
    commit_tree_split(commit).map(|(items, _)| items)
}

/// The flat merge engine's view of a commit's tree: every leaf, plus every
/// EMPTY subtree as a `TreeItemMode::Tree` entry (MG-04). Git's traversal sees
/// such a directory (`dirmask`), and it counts as "in the way" of a file at the
/// same path — verified against `git merge` with a crafted empty `foo/bar`
/// tree: `foo` still moves to `foo~HEAD`. The shared flattener drops empty
/// subtrees, which is why the two engines disagreed. The entries exist for the
/// D/F decision only: [`merge_tree_items`] strips them from its result (the
/// flat path never rebuilds empty trees — registered in MG-03). Reads mirror
/// the shared flattener (`Tree::load` for nested trees).
fn commit_tree_split_for_merge(
    commit: &Commit,
) -> Result<(HashMap<PathBuf, MergeTreeEntry>, GitlinkEntries), PullMergeError> {
    let tree: Tree = load_object(&commit.tree_id).map_err(|error| PullMergeError::TreeLoad {
        tree_id: commit.tree_id.to_string(),
        detail: error.to_string(),
    })?;
    Ok(split_gitlink_entries(flat_items_with_empty_dirs(&tree)?))
}

/// Fallible on purpose: `TreeExt::load` PANICS on a missing or corrupt object,
/// and this runs on the production merge path, so a damaged repository must
/// come back as [`PullMergeError::TreeLoad`] instead of aborting the process.
fn flat_items_with_empty_dirs(
    tree: &Tree,
) -> Result<Vec<(PathBuf, ObjectHash, TreeItemMode)>, PullMergeError> {
    let mut items = Vec::new();
    for item in &tree.tree_items {
        if item.mode != TreeItemMode::Tree {
            items.push((PathBuf::from(&item.name), item.id, item.mode));
            continue;
        }
        let sub_tree = Tree::try_load(&item.id).ok_or_else(|| PullMergeError::TreeLoad {
            tree_id: item.id.to_string(),
            detail: format!("failed to read the tree for '{}'", item.name),
        })?;
        if sub_tree.tree_items.is_empty() {
            items.push((PathBuf::from(&item.name), item.id, TreeItemMode::Tree));
            continue;
        }
        items.extend(
            flat_items_with_empty_dirs(&sub_tree)?
                .into_iter()
                .map(|(path, hash, mode)| (PathBuf::from(&item.name).join(path), hash, mode)),
        );
    }
    Ok(items)
}

/// An EMPTY directory entry at the very path another side holds a file is not
/// "beneath" that file: it contributes no leaf, so — like Git's "directory
/// merges to nothing" — the file sides alone decide. Only entries strictly
/// beneath a file make a directory "in the way" ([`directory_is_in_the_way`]).
fn sides_without_empty_dir_beside_file<'a>(
    base: Option<&'a MergeTreeEntry>,
    ours: Option<&'a MergeTreeEntry>,
    theirs: Option<&'a MergeTreeEntry>,
) -> [Option<&'a MergeTreeEntry>; 3] {
    let sides = [base, ours, theirs];
    let has_file = sides
        .iter()
        .any(|entry| entry.is_some_and(|entry| entry.mode != TreeItemMode::Tree));
    if !has_file {
        return sides;
    }
    sides.map(|entry| entry.filter(|entry| entry.mode != TreeItemMode::Tree))
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
                    Head::update_result_with_conn(txn, Head::Detached(new_oid), None)
                        .await
                        .map_err(|error| sea_orm::DbErr::Custom(error.to_string()))?;
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
        driver: BuiltinMergeDriver,
        /// External drivers own their conflict presentation and write it to
        /// `%A`; built-in drivers render later with the final branch labels.
        rendered: Option<MergeTreeEntry>,
    },
    OursModifiedTheirsDeleted {
        ours: ObjectHash,
    },
    TheirsModifiedOursDeleted {
        theirs: ObjectHash,
    },
    /// A directory/file (D/F) collision (MG-04, Git `merge-ort.c:4100-4198`):
    /// one side has a FILE at this path, the other a DIRECTORY whose contents
    /// survive the merge. The directory keeps the path; the file is written
    /// to `<path>~<branch>` (`unique_path`) and recorded there with its own
    /// stages, exactly as Git does.
    FileDirectory {
        /// The file the merge moves (the file side's entry).
        file: MergeTreeEntry,
        /// Which side holds the file (the other side holds the directory).
        file_side: MergeSide,
        /// The merge base's FILE at this path, if it had one — stage 1 at the
        /// moved name. Never a directory.
        base_file: Option<MergeTreeEntry>,
        /// Git's modify/delete shape: the base had the file, the file side
        /// changed it and the directory side deleted it — reported as
        /// `modify-delete` and announced with Git's second line. Kept a
        /// conflict even under `-X ours/theirs`, as Git does (verified).
        modify_delete: bool,
    },
    /// MG-06: a rename path conflict with its final worktree content already
    /// rendered. Stage entries remain in the remapped input maps: a colliding
    /// add must stay independently addressable even when -X settles the text.
    RenameMerged {
        content: MergeTreeEntry,
        kind: RenameConflictKind,
    },
    /// Git marks a split directory rename unclean even though affected paths
    /// stay resolved at stage 0. A split with no affected addition is clean.
    DirectorySplit {
        content: MergeTreeEntry,
    },
}

/// A path conflict retains its stages even when its content is already settled.
#[derive(Debug, Copy, Clone)]
enum RenameConflictKind {
    RenameRename,
    Content,
    DirectoryRename,
}

/// Which merge input a D/F file came from.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum MergeSide {
    Ours,
    Theirs,
}

#[derive(Debug, Copy, Clone)]
enum RelativeState {
    Same(MergeTreeEntry),
    Modified(MergeTreeEntry),
    Deleted,
    Added(MergeTreeEntry),
    Missing,
}

enum BlobMergeAttempt {
    NotApplicable,
    Clean(MergeTreeEntry),
    Conflict {
        driver: BuiltinMergeDriver,
        rendered: Option<MergeTreeEntry>,
    },
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
    path: &Path,
    base: Option<&MergeTreeEntry>,
    ours: Option<&MergeTreeEntry>,
    theirs: Option<&MergeTreeEntry>,
    context: &mut TreeMergeContext<'_>,
) -> Result<MergeResolution, PullMergeError> {
    let favor = context.favor;
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
                let driver = context.driver_for_path(path);
                if driver.is_builtin(BuiltinMergeDriver::Text)
                    && let Some(favor) = favor
                {
                    // Preserve the long-standing add/add `-X` shortcut for
                    // the built-in text driver: it chooses one complete side
                    // without loading either blob. External drivers still run
                    // and own `%A`, regardless of the strategy option.
                    favored_resolution(favor, Some(ours), Some(theirs))
                } else {
                    match try_merge_blob_contents(path, None, ours, theirs, driver, context)? {
                        BlobMergeAttempt::Clean(merged) => MergeResolution::Use(merged),
                        BlobMergeAttempt::Conflict { driver, rendered } => {
                            MergeResolution::Conflict(ConflictKind::BothChanged {
                                base: None,
                                ours: ours.hash,
                                theirs: theirs.hash,
                                driver,
                                rendered,
                            })
                        }
                        BlobMergeAttempt::NotApplicable if favor.is_some() => favored_resolution(
                            favor.unwrap_or(MergeFavor::Ours),
                            Some(ours),
                            Some(theirs),
                        ),
                        BlobMergeAttempt::NotApplicable => {
                            MergeResolution::Conflict(ConflictKind::BothChanged {
                                base: None,
                                ours: ours.hash,
                                theirs: theirs.hash,
                                driver: BuiltinMergeDriver::Text,
                                rendered: None,
                            })
                        }
                    }
                }
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
            } else {
                let driver = context.driver_for_path(path);
                match try_merge_blob_contents(path, base, ours, theirs, driver, context)? {
                    BlobMergeAttempt::Clean(merged) => MergeResolution::Use(merged),
                    BlobMergeAttempt::Conflict { driver, rendered } => {
                        MergeResolution::Conflict(ConflictKind::BothChanged {
                            base: base.map(|b| b.hash),
                            ours: ours.hash,
                            theirs: theirs.hash,
                            driver,
                            rendered,
                        })
                    }
                    BlobMergeAttempt::NotApplicable if favor.is_some() => favored_resolution(
                        favor.unwrap_or(MergeFavor::Ours),
                        Some(ours),
                        Some(theirs),
                    ),
                    BlobMergeAttempt::NotApplicable => {
                        MergeResolution::Conflict(ConflictKind::BothChanged {
                            base: base.map(|b| b.hash),
                            ours: ours.hash,
                            theirs: theirs.hash,
                            driver: BuiltinMergeDriver::Text,
                            rendered: None,
                        })
                    }
                }
            }
        }
        (true, RelativeState::Deleted, RelativeState::Same(_)) => MergeResolution::Delete,
        (true, RelativeState::Same(_), RelativeState::Deleted) => MergeResolution::Delete,
        (true, RelativeState::Deleted, RelativeState::Deleted) => MergeResolution::Delete,
        // A modify/delete is NOT a content conflict, so `-X ours` / `-X theirs`
        // does not settle it — it stays a conflict, exactly as Git leaves it.
        // FIX-MG05-01 (pre-existing, reproduced on the released v0.22.15
        // binary): applying the strategy option here resolved the pair in
        // favour of the DELETION, so the other side's edit was destroyed by a
        // merge that exited 0 and recorded nothing. Measured on git 2.50.1,
        // both directions and both options: `git merge -X ours` and
        // `-X theirs` over `f.txt` deleted on one side and modified on the
        // other print `CONFLICT (modify/delete)` and keep the modified content
        // at stages 1 and 2/3. The user docs already promised this ("a strategy
        // option settles content hunks only").
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

fn favored_resolution(
    favor: MergeFavor,
    ours: Option<MergeTreeEntry>,
    theirs: Option<MergeTreeEntry>,
) -> MergeResolution {
    match match favor {
        MergeFavor::Ours => ours,
        MergeFavor::Theirs => theirs,
    } {
        Some(entry) => MergeResolution::Use(entry),
        None => MergeResolution::Delete,
    }
}

fn try_merge_blob_contents(
    path: &Path,
    base: Option<&MergeTreeEntry>,
    ours: MergeTreeEntry,
    theirs: MergeTreeEntry,
    driver: SelectedMergeDriver,
    context: &mut TreeMergeContext<'_>,
) -> Result<BlobMergeAttempt, PullMergeError> {
    // Git merges the CONTENT and the MODE independently
    // (`merge-ort.c` `handle_content_merge`): a side that only chmod'ed does
    // not stop the line-level merge, and the mode that differs from the base
    // wins. Two sides changing the mode differently is the one case with no
    // answer — that stays a conflict. Verified against `git merge`:
    // rename + `chmod +x` on one side and an edit on the other merges cleanly
    // and keeps `100755`.
    if base.is_some_and(|base| !is_regular_file_mode(base.mode))
        || !is_regular_file_mode(ours.mode)
        || !is_regular_file_mode(theirs.mode)
    {
        return Ok(BlobMergeAttempt::NotApplicable);
    }
    let (merged_mode, mode_clean) = if ours.mode == theirs.mode {
        (ours.mode, true)
    } else if base.is_some_and(|base| ours.mode == base.mode) {
        (theirs.mode, true)
    } else if base.is_some_and(|base| theirs.mode == base.mode) {
        (ours.mode, true)
    } else {
        // There is no common mode answer, but Git still runs the low-level
        // driver for the content and leaves the path unmerged. Keeping ours'
        // mode here lets an external driver's `%A` result survive that
        // independent mode conflict.
        (ours.mode, false)
    };

    let base_blob = match base {
        Some(base) => Some(load_merge_blob(base.hash, context.virtual_blobs)?),
        None => None,
    };
    let ours_blob = load_merge_blob(ours.hash, context.virtual_blobs)?;
    let theirs_blob = load_merge_blob(theirs.hash, context.virtual_blobs)?;
    let base_data = base_blob
        .as_ref()
        .map_or(&[][..], |blob| blob.data.as_slice());
    let marker_length = conflict_marker_length_at_depth(
        &[base_data, &ours_blob.data, &theirs_blob.data],
        context.depth,
    );
    let outcome = match &driver {
        SelectedMergeDriver::Builtin(driver) => merge_bytes_with_driver(
            *driver,
            base_data,
            &ours_blob.data,
            &theirs_blob.data,
            context.favor,
            diffy::ConflictStyle::Diff3,
            2 * context.depth,
        )
        .map_err(PullMergeError::TreeCreate)?,
        SelectedMergeDriver::External(driver) => run_external_merge_driver(
            &context.external_merge_runtime,
            driver,
            ExternalMergeInput {
                path,
                base_id: base.map_or_else(
                    || Blob::from_content_bytes(Vec::new()).id,
                    |entry| entry.hash,
                ),
                ours_id: ours.hash,
                theirs_id: theirs.hash,
                base: base_data,
                ours: &ours_blob.data,
                theirs: &theirs_blob.data,
                marker_length,
                labels: context.external_labels(),
            },
        )
        .map_err(PullMergeError::TreeCreate)?,
    };

    let (merged_bytes, content_clean) = match outcome {
        BuiltinMergeOutcome::Clean(bytes) => (bytes, true),
        BuiltinMergeOutcome::Conflict(bytes) => (bytes, false),
    };
    let clean = content_clean && mode_clean;
    let rendered = matches!(driver, SelectedMergeDriver::External(_));
    if !clean && !rendered {
        return Ok(BlobMergeAttempt::Conflict {
            driver: driver.fallback_builtin(),
            rendered: None,
        });
    }
    let merged_blob = Blob::from_content_bytes(merged_bytes);
    // `--dry-run` (persist=false): the merged OID is computed in memory only —
    // persisting here would write the object store (and, under tiered storage,
    // upload to the durable tier) from a preview. It still has to stay
    // ADDRESSABLE, because inside the recursive fold this blob becomes the
    // virtual ancestor's content and the outer merge loads it by id.
    context.record_merged_blob(&merged_blob)?;

    let entry = MergeTreeEntry {
        hash: merged_blob.id,
        mode: merged_mode,
    };
    if clean {
        Ok(BlobMergeAttempt::Clean(entry))
    } else {
        Ok(BlobMergeAttempt::Conflict {
            driver: driver.fallback_builtin(),
            rendered: Some(entry),
        })
    }
}

/// Choose the requested side only inside `diffy` conflict regions while
/// preserving every cleanly merged range around them. The marker length is
/// chosen so none of the four marker runs can occur anywhere in an input,
/// making byte-level parsing safe even when a conflicted final line has no
/// trailing newline.
fn resolve_favored_content(
    conflicted: Vec<u8>,
    marker_len: usize,
    favor: MergeFavor,
) -> Result<Vec<u8>, String> {
    resolve_conflicted_content(conflicted, marker_len, ConflictResolution::Favor(favor))
}

#[derive(Debug, Copy, Clone)]
enum ConflictResolution {
    Favor(MergeFavor),
    Union,
}

fn resolve_conflicted_content(
    conflicted: Vec<u8>,
    marker_len: usize,
    resolution: ConflictResolution,
) -> Result<Vec<u8>, String> {
    let marker = |byte: u8, label: Option<&[u8]>| {
        let mut line = vec![byte; marker_len];
        if let Some(label) = label {
            line.push(b' ');
            line.extend_from_slice(label);
        }
        line.push(b'\n');
        line
    };
    let open = marker(b'<', Some(b"ours"));
    let original = marker(b'|', Some(b"original"));
    let separator = marker(b'=', None);
    let close = marker(b'>', Some(b"theirs"));

    let find_after = |haystack: &[u8], start: usize, needle: &[u8]| {
        haystack
            .get(start..)
            .and_then(|tail| {
                tail.windows(needle.len())
                    .position(|window| window == needle)
            })
            .map(|relative| start + relative)
    };
    let malformed = || "internal three-way merge produced malformed conflict markers".to_string();

    let mut output = Vec::with_capacity(conflicted.len());
    let mut cursor = 0usize;
    let mut resolved = 0usize;
    while let Some(open_start) = find_after(&conflicted, cursor, &open) {
        output.extend_from_slice(&conflicted[cursor..open_start]);
        let ours_start = open_start + open.len();
        let original_start =
            find_after(&conflicted, ours_start, &original).ok_or_else(malformed)?;
        let base_start = original_start + original.len();
        let separator_start =
            find_after(&conflicted, base_start, &separator).ok_or_else(malformed)?;
        let theirs_start = separator_start + separator.len();
        let close_start = find_after(&conflicted, theirs_start, &close).ok_or_else(malformed)?;
        match resolution {
            ConflictResolution::Favor(MergeFavor::Ours) => {
                output.extend_from_slice(&conflicted[ours_start..original_start]);
            }
            ConflictResolution::Favor(MergeFavor::Theirs) => {
                output.extend_from_slice(&conflicted[theirs_start..close_start]);
            }
            ConflictResolution::Union => {
                output.extend_from_slice(&conflicted[ours_start..original_start]);
                output.extend_from_slice(&conflicted[theirs_start..close_start]);
            }
        }
        cursor = close_start + close.len();
        resolved += 1;
    }
    if resolved == 0 {
        return Err(malformed());
    }
    output.extend_from_slice(&conflicted[cursor..]);
    Ok(output)
}

/// Git's built-in low-level merge drivers. A named driver without an external
/// configuration resolves to `Text`, the same fallback used by
/// `find_ll_merge_driver`.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum BuiltinMergeDriver {
    Text,
    Binary,
    Union,
}

/// The bytes a low-level driver produced, and whether the path remains
/// unmerged. A binary conflict intentionally carries ours verbatim rather than
/// manufacturing text markers in arbitrary bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BuiltinMergeOutcome {
    Clean(Vec<u8>),
    Conflict(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExternalMergeDriver {
    name: String,
    command: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SelectedMergeDriver {
    Builtin(BuiltinMergeDriver),
    External(ExternalMergeDriver),
}

impl SelectedMergeDriver {
    fn fallback_builtin(&self) -> BuiltinMergeDriver {
        match self {
            Self::Builtin(driver) => *driver,
            Self::External(_) => BuiltinMergeDriver::Text,
        }
    }

    fn is_builtin(&self, expected: BuiltinMergeDriver) -> bool {
        matches!(self, Self::Builtin(driver) if *driver == expected)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ExternalMergeCacheKey {
    driver: String,
    path: PathBuf,
    base: ObjectHash,
    ours: ObjectHash,
    theirs: ObjectHash,
    marker_length: usize,
    ancestor_label: String,
    ours_label: String,
    theirs_label: String,
}

#[derive(Default)]
struct ExternalMergeRuntime {
    drivers: HashMap<String, ExternalMergeDriver>,
    cache: Mutex<HashMap<ExternalMergeCacheKey, BuiltinMergeOutcome>>,
}

type SharedExternalMergeRuntime = Arc<ExternalMergeRuntime>;

struct ExternalMergeLabels<'a> {
    ancestor: &'a str,
    ours: &'a str,
    theirs: &'a str,
}

struct ExternalMergeInput<'a> {
    path: &'a Path,
    base_id: ObjectHash,
    ours_id: ObjectHash,
    theirs_id: ObjectHash,
    base: &'a [u8],
    ours: &'a [u8],
    theirs: &'a [u8],
    marker_length: usize,
    labels: ExternalMergeLabels<'a>,
}

struct ExternalMergeTempFiles {
    _directory: tempfile::TempDir,
    base: tempfile::NamedTempFile,
    ours: tempfile::NamedTempFile,
    theirs: tempfile::NamedTempFile,
}

impl ExternalMergeTempFiles {
    fn create(input: &ExternalMergeInput<'_>) -> Result<Self, String> {
        let worktree = util::try_working_dir()
            .map_err(|error| format!("failed to resolve the worktree: {error}"))?;
        Self::create_in(&worktree, input)
    }

    fn create_in(worktree: &Path, input: &ExternalMergeInput<'_>) -> Result<Self, String> {
        let directory = tempfile::Builder::new()
            .prefix(".libra-merge-driver-")
            .tempdir_in(worktree)
            .map_err(|error| {
                format!("failed to create a worktree-local merge-driver directory: {error}")
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).map_err(
                |error| format!("failed to protect the merge-driver directory: {error}"),
            )?;
        }

        let create_file = |prefix: &str, content: &[u8]| {
            let mut file = tempfile::Builder::new()
                .prefix(prefix)
                .tempfile_in(directory.path())
                .map_err(|error| format!("failed to create merge-driver input: {error}"))?;
            file.write_all(content)
                .map_err(|error| format!("failed to write merge-driver input: {error}"))?;
            file.flush()
                .map_err(|error| format!("failed to flush merge-driver input: {error}"))?;
            Ok::<_, String>(file)
        };
        let base = create_file("base-", input.base)?;
        let ours = create_file("ours-", input.ours)?;
        let theirs = create_file("theirs-", input.theirs)?;
        Ok(Self {
            _directory: directory,
            base,
            ours,
            theirs,
        })
    }
}

#[cfg(unix)]
fn sq_quote_external_merge_value(value: &OsStr) -> OsString {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

    let mut quoted = Vec::with_capacity(value.as_bytes().len() + 2);
    quoted.push(b'\'');
    for byte in value.as_bytes() {
        if *byte == b'\'' {
            quoted.extend_from_slice(b"'\\''");
        } else {
            quoted.push(*byte);
        }
    }
    quoted.push(b'\'');
    OsString::from_vec(quoted)
}

#[cfg(windows)]
fn sq_quote_external_merge_value(value: &OsStr) -> OsString {
    use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};

    let mut quoted = Vec::new();
    quoted.push(b'\'' as u16);
    for unit in value.encode_wide() {
        if unit == b'\'' as u16 {
            quoted.extend("'\\''".encode_utf16());
        } else {
            quoted.push(unit);
        }
    }
    quoted.push(b'\'' as u16);
    OsString::from_wide(&quoted)
}

#[cfg(not(any(unix, windows)))]
fn sq_quote_external_merge_value(value: &OsStr) -> OsString {
    OsString::from(format!(
        "'{}'",
        value.to_string_lossy().replace('\'', "'\\''")
    ))
}

fn expand_external_merge_path(path: &Path) -> OsString {
    let value = path.as_os_str();
    if value.to_str().is_some_and(|value| {
        value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
    }) {
        value.to_os_string()
    } else {
        // Git can insert `%O/%A/%B` verbatim because its temporary basename is
        // shell-safe. Libra's files are worktree-local absolute paths, so an
        // unsafe worktree directory would otherwise turn trusted global driver
        // config into code injection merely by checking out a repository under
        // a hostile name. Safe paths retain Git's unquoted expansion.
        sq_quote_external_merge_value(value)
    }
}

fn expand_external_merge_command(
    command: &str,
    files: &ExternalMergeTempFiles,
    input: &ExternalMergeInput<'_>,
) -> OsString {
    let replacements = |token| match token {
        'O' => Some(expand_external_merge_path(files.base.path())),
        'A' => Some(expand_external_merge_path(files.ours.path())),
        'B' => Some(expand_external_merge_path(files.theirs.path())),
        'L' => Some(OsString::from(input.marker_length.to_string())),
        'P' => Some(sq_quote_external_merge_value(input.path.as_os_str())),
        'S' => Some(sq_quote_external_merge_value(OsStr::new(
            input.labels.ancestor,
        ))),
        'X' => Some(sq_quote_external_merge_value(OsStr::new(input.labels.ours))),
        'Y' => Some(sq_quote_external_merge_value(OsStr::new(
            input.labels.theirs,
        ))),
        '%' => Some(OsString::from("%")),
        _ => None,
    };
    let mut expanded = OsString::new();
    let mut literal = String::with_capacity(command.len());
    let mut chars = command.chars();
    while let Some(current) = chars.next() {
        if current != '%' {
            literal.push(current);
            continue;
        }
        expanded.push(&literal);
        literal.clear();
        let Some(token) = chars.next() else {
            literal.push('%');
            break;
        };
        match replacements(token) {
            Some(value) => expanded.push(value),
            None => {
                literal.push('%');
                literal.push(token);
            }
        }
    }
    expanded.push(literal);
    expanded
}

fn external_driver_shell(command: &OsStr) -> Command {
    let mut process = Command::new("sh");
    process.arg("-c").arg(command);
    process
}

fn run_external_merge_driver(
    runtime: &ExternalMergeRuntime,
    driver: &ExternalMergeDriver,
    input: ExternalMergeInput<'_>,
) -> Result<BuiltinMergeOutcome, String> {
    let key = ExternalMergeCacheKey {
        driver: driver.name.clone(),
        path: input.path.to_path_buf(),
        base: input.base_id,
        ours: input.ours_id,
        theirs: input.theirs_id,
        marker_length: input.marker_length,
        ancestor_label: input.labels.ancestor.to_string(),
        ours_label: input.labels.ours.to_string(),
        theirs_label: input.labels.theirs.to_string(),
    };
    if let Some(cached) = runtime
        .cache
        .lock()
        .map_err(|_| "external merge-driver result cache is unavailable".to_string())?
        .get(&key)
        .cloned()
    {
        return Ok(cached);
    }

    let files = ExternalMergeTempFiles::create(&input)?;
    let expanded = expand_external_merge_command(&driver.command, &files, &input);
    // The driver protocol communicates only through `%A`. Discarding the
    // child's streams prevents an untrusted amount of output from being held
    // in memory and keeps command/output details out of Libra diagnostics.
    let status = external_driver_shell(&expanded)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| {
            format!(
                "external merge driver '{}' could not start for '{}': {error}; check merge.{}.driver and its executable",
                driver.name,
                input.path.display(),
                driver.name
            )
        })?;
    let code = status.code().ok_or_else(|| {
        format!(
            "external merge driver '{}' was interrupted for '{}'; its temporary files were protected and cleaned",
            driver.name,
            input.path.display()
        )
    })?;
    if !(0..=128).contains(&code) {
        return Err(format!(
            "external merge driver '{}' exited with status {code} for '{}'; inspect merge.{}.driver without exposing it in logs",
            driver.name,
            input.path.display(),
            driver.name
        ));
    }
    let merged = fs::read(files.ours.path()).map_err(|error| {
        format!(
            "external merge driver '{}' did not leave a readable %A result for '{}': {error}",
            driver.name,
            input.path.display()
        )
    })?;
    let outcome = if code == 0 {
        BuiltinMergeOutcome::Clean(merged)
    } else {
        BuiltinMergeOutcome::Conflict(merged)
    };
    runtime
        .cache
        .lock()
        .map_err(|_| "external merge-driver result cache is unavailable".to_string())?
        .insert(key, outcome.clone());
    Ok(outcome)
}

fn builtin_driver_named(name: &str) -> BuiltinMergeDriver {
    match name {
        "binary" => BuiltinMergeDriver::Binary,
        "union" => BuiltinMergeDriver::Union,
        "text" => BuiltinMergeDriver::Text,
        _ => BuiltinMergeDriver::Text,
    }
}

fn selected_driver_named(name: &str, runtime: &ExternalMergeRuntime) -> SelectedMergeDriver {
    if let Some(driver) = runtime.drivers.get(name) {
        SelectedMergeDriver::External(driver.clone())
    } else {
        SelectedMergeDriver::Builtin(builtin_driver_named(name))
    }
}

fn select_merge_driver(
    attribute: Option<AttributeState>,
    default_driver: Option<&str>,
    runtime: &ExternalMergeRuntime,
) -> SelectedMergeDriver {
    match attribute {
        Some(AttributeState::Set) => SelectedMergeDriver::Builtin(BuiltinMergeDriver::Text),
        Some(AttributeState::Unset) => SelectedMergeDriver::Builtin(BuiltinMergeDriver::Binary),
        Some(AttributeState::Value(name)) => selected_driver_named(&name, runtime),
        Some(AttributeState::Unspecified) | None => default_driver.map_or(
            SelectedMergeDriver::Builtin(BuiltinMergeDriver::Text),
            |name| selected_driver_named(name, runtime),
        ),
    }
}

fn select_builtin_merge_driver(
    attribute: Option<AttributeState>,
    default_driver: Option<&str>,
) -> BuiltinMergeDriver {
    match attribute {
        Some(AttributeState::Set) => BuiltinMergeDriver::Text,
        Some(AttributeState::Unset) => BuiltinMergeDriver::Binary,
        Some(AttributeState::Value(name)) => builtin_driver_named(&name),
        Some(AttributeState::Unspecified) | None => {
            default_driver.map_or(BuiltinMergeDriver::Text, builtin_driver_named)
        }
    }
}

/// Resolve the effective built-in driver for a worktree-relative path through
/// the existing gitattributes engine. A named-but-unknown attribute never
/// consults `merge.default`; it falls straight back to text, matching Git.
pub(crate) fn builtin_merge_driver_for_path(
    path: &Path,
    default_driver: Option<&str>,
) -> BuiltinMergeDriver {
    select_builtin_merge_driver(
        attributes::attribute_state_for_path("merge", path),
        default_driver,
    )
}

/// Read `merge.default` through the same strict local-to-system cascade used
/// by the other merge configuration. The caller decides whether it is inside
/// a repository; merge-file outside a repository must not consult config.
pub(crate) async fn read_merge_default_driver() -> Result<Option<String>, String> {
    use crate::internal::config::{LocalIdentityTarget, read_cascaded_config_value_strict};

    read_cascaded_config_value_strict(LocalIdentityTarget::CurrentRepo, "merge.default")
        .await
        .map_err(|error| format!("{error:#}"))
}

async fn read_external_merge_runtime() -> Result<SharedExternalMergeRuntime, String> {
    use crate::internal::config::{LocalIdentityTarget, read_cascaded_subsection_values_strict};

    let configured =
        read_cascaded_subsection_values_strict(LocalIdentityTarget::CurrentRepo, "merge", "driver")
            .await
            .map_err(|error| format!("{error:#}"))?;
    let mut drivers = HashMap::with_capacity(configured.len());
    for (name, command) in configured {
        if command.is_empty() {
            return Err(format!(
                "merge.{name}.driver is empty; set a shell command or remove the configuration"
            ));
        }
        if command.contains('\0') {
            return Err(format!(
                "merge.{name}.driver contains a NUL byte; replace it with a valid shell command"
            ));
        }
        drivers.insert(name.clone(), ExternalMergeDriver { name, command });
    }
    Ok(Arc::new(ExternalMergeRuntime {
        drivers,
        cache: Mutex::new(HashMap::new()),
    }))
}

/// Apply one built-in low-level driver. The text and union drivers use a diff3
/// rendering internally so hunk selection can be parsed without ambiguity;
/// the configured style is preserved when a text conflict is returned.
pub(crate) fn merge_bytes_with_driver(
    driver: BuiltinMergeDriver,
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    favor: Option<MergeFavor>,
    conflict_style: diffy::ConflictStyle,
    extra_marker_size: usize,
) -> Result<BuiltinMergeOutcome, String> {
    // High-level merge consumers normally settle these OID-equality cases
    // before low-level dispatch. Keep the shared helper equally safe for
    // byte-oriented consumers such as merge-file.
    if ours == theirs {
        return Ok(BuiltinMergeOutcome::Clean(ours.to_vec()));
    }
    if ours == base {
        return Ok(BuiltinMergeOutcome::Clean(theirs.to_vec()));
    }
    if theirs == base {
        return Ok(BuiltinMergeOutcome::Clean(ours.to_vec()));
    }
    let binary_fallback = driver == BuiltinMergeDriver::Binary
        || (driver == BuiltinMergeDriver::Union
            && (merge_input_is_binary(base)
                || merge_input_is_binary(ours)
                || merge_input_is_binary(theirs)));
    if binary_fallback {
        // Git's union driver sets the union variant itself; if xdiff rejects
        // binary input, that variant is not ours/theirs and the binary fallback
        // therefore reports a conflict with ours.
        let effective_favor = if driver == BuiltinMergeDriver::Union {
            None
        } else {
            favor
        };
        return Ok(match effective_favor {
            None => BuiltinMergeOutcome::Conflict(ours.to_vec()),
            Some(MergeFavor::Ours) => BuiltinMergeOutcome::Clean(ours.to_vec()),
            Some(MergeFavor::Theirs) => BuiltinMergeOutcome::Clean(theirs.to_vec()),
        });
    }

    let marker_len =
        unambiguous_conflict_marker_length(&[base, ours, theirs]).saturating_add(extra_marker_size);
    let mut options = diffy::MergeOptions::new();
    options
        .set_conflict_style(if favor.is_some() || driver == BuiltinMergeDriver::Union {
            diffy::ConflictStyle::Diff3
        } else {
            conflict_style
        })
        .set_conflict_marker_length(marker_len);
    match options.merge_bytes(base, ours, theirs) {
        Ok(bytes) => Ok(BuiltinMergeOutcome::Clean(bytes)),
        Err(conflicted) => match driver {
            BuiltinMergeDriver::Union => {
                resolve_conflicted_content(conflicted, marker_len, ConflictResolution::Union)
                    .map(BuiltinMergeOutcome::Clean)
            }
            BuiltinMergeDriver::Text => match favor {
                Some(favor) => resolve_favored_content(conflicted, marker_len, favor)
                    .map(BuiltinMergeOutcome::Clean),
                None => Ok(BuiltinMergeOutcome::Conflict(conflicted)),
            },
            BuiltinMergeDriver::Binary => {
                Err("internal binary merge driver unexpectedly reached the text merger".to_string())
            }
        },
    }
}

/// Merge three blob payloads and resolve only overlapping regions in favor of
/// the requested side. Cleanly merged ranges are preserved. Cherry-pick and
/// revert share this with merge so `-X ours`/`-X theirs` have identical hunk
/// semantics across all non-interactive history controls.
pub(crate) fn merge_bytes_with_favor(
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    favor: MergeFavor,
) -> Result<Vec<u8>, String> {
    let marker_len = unambiguous_conflict_marker_length(&[base, ours, theirs]);
    let mut merge_options = diffy::MergeOptions::new();
    merge_options
        .set_conflict_style(diffy::ConflictStyle::Diff3)
        .set_conflict_marker_length(marker_len);
    match merge_options.merge_bytes(base, ours, theirs) {
        Ok(merged) => Ok(merged),
        Err(conflicted) => resolve_favored_content(conflicted, marker_len, favor),
    }
}

fn unambiguous_conflict_marker_length(sides: &[&[u8]]) -> usize {
    const DEFAULT_MARKER_LENGTH: usize = 7;
    let mut longest = 0usize;
    for side in sides {
        for marker in *b"<>=|" {
            let mut run = 0usize;
            for byte in *side {
                if *byte == marker {
                    run += 1;
                    longest = longest.max(run);
                } else {
                    run = 0;
                }
            }
        }
    }
    DEFAULT_MARKER_LENGTH.max(longest.saturating_add(1))
}

/// Load a blob a three-way merge needs, preferring the content the recursive
/// virtual ancestor synthesized over the object store.
///
/// The fold keeps every blob it creates in [`VirtualBlobs`] as well as (when
/// the merge is real) writing it, which is what lets a `--dry-run` preview of a
/// criss-cross history compute the same result without writing anything.
fn load_merge_blob(hash: ObjectHash, virtual_blobs: &VirtualBlobs) -> Result<Blob, PullMergeError> {
    if let Some(data) = virtual_blobs.get(&hash) {
        return Ok(Blob::from_content_bytes(data.clone()));
    }
    load_object(&hash).map_err(|error| PullMergeError::ObjectLoad {
        object_id: hash.to_string(),
        detail: error.to_string(),
    })
}

/// How deep the recursive virtual-ancestor fold may nest before `merge` gives
/// up (MG-02).
///
/// Git recurses without a ceiling (`merge-ort.c:5313`); Libra folds the bases
/// with real recursion, so a ceiling is what turns a pathological history into
/// a message instead of a stack overflow. Twenty levels is far past anything
/// real history produces — each level needs the merge bases of the previous
/// level's bases to themselves be multiple — while still bounding the stack.
pub(crate) const MAX_VIRTUAL_ANCESTOR_DEPTH: usize = 20;

/// How many merge bases one level of the fold may combine.
///
/// The fold's cost is quadratic in this width: folding the `k`-th base asks
/// for the merge bases of `next` against each of the `k − 1` already folded
/// ([`merge_bases_of_folded`]), and each of those is a full paint-down walk
/// through the object store — `internal::merge_base` deliberately exposes only
/// per-call APIs (MG-01), so the fold cannot share one graph read across them.
/// The depth ceiling bounds nesting, not width; this bounds width. Thirty-two
/// mutually independent common ancestors is far past any real history (a
/// criss-cross has two).
pub(crate) const MAX_VIRTUAL_ANCESTOR_BASES: usize = 32;

/// The two side labels Git renders inside a virtual-ancestor merge
/// (`merge-ort.c` `merge_ort_internal`, lines 5371-5372 at git@`3cb9185f6`,
/// swaps `opt->branch1`/`branch2` for these inside the fold loop).
const VIRTUAL_OURS_LABEL: &str = "Temporary merge branch 1";
const VIRTUAL_THEIRS_LABEL: &str = "Temporary merge branch 2";

/// Commit message of the synthetic commit standing in for a folded ancestor.
const VIRTUAL_ANCESTOR_MESSAGE: &str = "merged common ancestors\n";

/// Blobs synthesized while folding several merge bases into one virtual
/// ancestor, addressable by object id without going through the object store.
///
/// Always empty for a merge with zero or one merge base.
type VirtualBlobs = HashMap<ObjectHash, Vec<u8>>;

/// The recursive virtual ancestor of a multi-base merge: the flattened tree the
/// outer three-way merge uses as its base, plus the blobs the fold created for
/// it.
struct VirtualAncestor {
    items: HashMap<PathBuf, MergeTreeEntry>,
    blobs: VirtualBlobs,
}

/// Everything a three-way tree merge needs beyond the three item maps.
struct TreeMergeContext<'a> {
    /// Write auto-merged blobs to the object store. `false` under `--dry-run`,
    /// where the merged object ids are computed in memory only.
    persist_merged_blobs: bool,
    /// Resolve otherwise-conflicting hunks in favor of one side. Always `None`
    /// inside the virtual-ancestor fold: Git disables `-X ours`/`-X theirs`
    /// there (`merge-ort.c` sets `ll_opts.variant = 0` whenever
    /// `call_depth` is non-zero), because a virtual ancestor is an input the
    /// user never asked to bias.
    favor: Option<MergeFavor>,
    /// Recursion depth: 0 for the merge the user asked for, 1 for the merges
    /// that fold its merge bases, 2 for the merges that fold *those* bases, and
    /// so on — Git's `call_depth`.
    depth: usize,
    /// Configured fallback used only when the path has no `merge` attribute.
    default_driver: Option<String>,
    /// One config snapshot and result cache shared by the incremental walk,
    /// flat fallback, rename replay, and recursive virtual-ancestor fold.
    external_merge_runtime: SharedExternalMergeRuntime,
    ancestor_label: String,
    ours_label: String,
    theirs_label: String,
    /// Blobs this merge (and, inside the recursive fold, every level below it)
    /// synthesized WITHOUT writing them, consulted by [`load_merge_blob`] ahead
    /// of the object store.
    ///
    /// Mutable because a merged blob has to land somewhere the next reader can
    /// find it: when `persist_merged_blobs` is set that place is the object
    /// store, and when it is not — a `--dry-run` preview — it is this map. A
    /// virtual ancestor's content is exactly the case where the difference
    /// matters: the outer merge reads it back by object id.
    virtual_blobs: &'a mut VirtualBlobs,
}

impl TreeMergeContext<'_> {
    /// The context for a real (depth 0) merge.
    #[cfg(test)]
    fn top_level<'a>(
        persist_merged_blobs: bool,
        favor: Option<MergeFavor>,
        default_driver: Option<&str>,
        virtual_blobs: &'a mut VirtualBlobs,
    ) -> TreeMergeContext<'a> {
        TreeMergeContext {
            persist_merged_blobs,
            favor,
            depth: 0,
            default_driver: default_driver.map(str::to_owned),
            external_merge_runtime: Arc::new(ExternalMergeRuntime::default()),
            ancestor_label: "base".to_string(),
            ours_label: "HEAD".to_string(),
            theirs_label: "theirs".to_string(),
            virtual_blobs,
        }
    }

    fn top_level_with_external<'a>(
        persist_merged_blobs: bool,
        favor: Option<MergeFavor>,
        default_driver: Option<&str>,
        theirs_label: &str,
        external_merge_runtime: SharedExternalMergeRuntime,
        virtual_blobs: &'a mut VirtualBlobs,
    ) -> TreeMergeContext<'a> {
        TreeMergeContext {
            persist_merged_blobs,
            favor,
            depth: 0,
            default_driver: default_driver.map(str::to_owned),
            external_merge_runtime,
            ancestor_label: "base".to_string(),
            ours_label: "HEAD".to_string(),
            theirs_label: theirs_label.to_string(),
            virtual_blobs,
        }
    }

    /// The context for a merge INSIDE the virtual-ancestor fold. `favor` is
    /// always `None` there: Git disables `-X ours`/`-X theirs` whenever
    /// `call_depth` is non-zero, because a virtual ancestor is an input the
    /// user never asked to bias.
    #[cfg(test)]
    fn nested<'a>(
        persist_merged_blobs: bool,
        depth: usize,
        default_driver: Option<&str>,
        virtual_blobs: &'a mut VirtualBlobs,
    ) -> TreeMergeContext<'a> {
        Self::nested_with_external(
            persist_merged_blobs,
            depth,
            default_driver,
            Arc::new(ExternalMergeRuntime::default()),
            virtual_blobs,
        )
    }

    fn nested_with_external<'a>(
        persist_merged_blobs: bool,
        depth: usize,
        default_driver: Option<&str>,
        external_merge_runtime: SharedExternalMergeRuntime,
        virtual_blobs: &'a mut VirtualBlobs,
    ) -> TreeMergeContext<'a> {
        TreeMergeContext {
            persist_merged_blobs,
            favor: None,
            depth,
            default_driver: default_driver.map(str::to_owned),
            external_merge_runtime,
            ancestor_label: "merged common ancestors".to_string(),
            ours_label: VIRTUAL_OURS_LABEL.to_string(),
            theirs_label: VIRTUAL_THEIRS_LABEL.to_string(),
            virtual_blobs,
        }
    }

    /// Record a blob this merge produced: written to the object store, or —
    /// when nothing may be written — kept addressable in memory instead.
    fn record_merged_blob(&mut self, blob: &Blob) -> Result<(), PullMergeError> {
        if self.persist_merged_blobs {
            save_object(blob, &blob.id).map_err(|error| {
                PullMergeError::TreeCreate(format!(
                    "failed to save auto-merged blob {}: {error}",
                    blob.id
                ))
            })?;
        } else {
            self.virtual_blobs.insert(blob.id, blob.data.clone());
        }
        Ok(())
    }

    fn driver_for_path(&self, path: &Path) -> SelectedMergeDriver {
        select_merge_driver(
            attributes::attribute_state_for_path("merge", path),
            self.default_driver.as_deref(),
            &self.external_merge_runtime,
        )
    }

    fn external_labels(&self) -> ExternalMergeLabels<'_> {
        ExternalMergeLabels {
            ancestor: &self.ancestor_label,
            ours: &self.ours_label,
            theirs: &self.theirs_label,
        }
    }
}

/// The conflict-marker length to use at recursion depth `depth`.
///
/// Git widens the markers by two per level — `merge-ort.c` passes
/// `opt->priv->call_depth * 2` as `extra_marker_size` and `ll_xdl_merge` adds
/// it to `DEFAULT_CONFLICT_MARKER_SIZE` — so a conflict recorded inside a
/// virtual ancestor cannot be mistaken for one the outer merge produced when
/// the ancestor's content is merged again one level up. Composed with Libra's
/// existing content-driven bump ([`unambiguous_conflict_marker_length`]), which
/// is what keeps a marker run distinguishable from the *inputs*.
fn conflict_marker_length_at_depth(sides: &[&[u8]], depth: usize) -> usize {
    unambiguous_conflict_marker_length(sides).saturating_add(2 * depth)
}

/// Refuse to fold merge bases nested deeper than [`MAX_VIRTUAL_ANCESTOR_DEPTH`].
fn ensure_virtual_ancestor_depth(depth: usize) -> Result<(), PullMergeError> {
    if depth > MAX_VIRTUAL_ANCESTOR_DEPTH {
        return Err(PullMergeError::VirtualAncestorTooDeep);
    }
    Ok(())
}

/// Refuse to fold more than [`MAX_VIRTUAL_ANCESTOR_BASES`] bases at one level.
fn ensure_virtual_ancestor_width(bases: usize) -> Result<(), PullMergeError> {
    if bases > MAX_VIRTUAL_ANCESTOR_BASES {
        return Err(PullMergeError::VirtualAncestorTooWide { bases });
    }
    Ok(())
}

/// The order the merge bases are folded in: ascending hex id.
///
/// Git folds them in whatever order `get_merge_bases()` returned, which makes
/// the virtual ancestor's content depend on traversal order. Sorting makes the
/// fold — and therefore the merge result and every object it writes —
/// reproducible, which is what lets `--restart` recompute the same ancestor
/// after `maintenance gc` has reclaimed it (ADR-MG-04).
fn virtual_base_fold_order(bases: &[ObjectHash]) -> Vec<ObjectHash> {
    let mut ordered = bases.to_vec();
    ordered.sort_by_key(|id| id.to_string());
    ordered.dedup();
    ordered
}

/// Load one of the merge bases being folded. Reported as a plain object load:
/// it is neither the current nor the target commit, and naming it as one would
/// misdirect anyone reading the error.
fn load_merge_commit(id: &ObjectHash) -> Result<Commit, PullMergeError> {
    load_object(id).map_err(|error| PullMergeError::ObjectLoad {
        object_id: id.to_string(),
        detail: error.to_string(),
    })
}

/// The merge bases of the ancestor folded from `folded` with `next`.
///
/// Git asks this of the synthetic commit it just built (`merge-ort.c`
/// `merge_ort_internal`, lines 5353-5385 at git@`3cb9185f6`, chains
/// `make_virtual_commit` at line 5380 so the next round can call
/// `get_merge_bases()` on it). The same set falls out of the REAL bases folded
/// so far: the virtual commit's ancestry is exactly the union of theirs, so the
/// common ancestors of it and `next` are
/// `⋃ᵢ (anc(folded[i]) ∩ anc(next))`, and the maximal elements
/// of a union are always among the maximal elements of its parts — i.e. among
/// `⋃ᵢ merge_bases(folded[i], next)`, filtered for domination across the parts.
///
/// Deriving it this way keeps the fold from having to read back an object it
/// wrote, which is what lets a `--dry-run` preview of a criss-cross history
/// write nothing at all.
///
/// Work: one merge-base walk per base already folded, plus — only when MORE
/// than one part contributed candidates — one ancestry walk per candidate pair
/// drawn from DIFFERENT parts (a single part's candidates are already mutually
/// maximal, so they never need checking against each other). The first fold
/// step, which is the whole fold for the common two-base criss-cross, is
/// therefore exactly one walk with no filtering.
///
/// Bounded: the parts are at most [`MAX_VIRTUAL_ANCESTOR_BASES`] (checked
/// here as well as by the caller), and candidate collection stops — with the
/// same refusal — the moment MORE than that many distinct candidates have been
/// seen, BEFORE any pairwise ancestry walk. The maximal set is a subset of the
/// candidates, so this is conservative: a nested history whose candidates
/// exceed the ceiling is refused even if domination would have thinned them.
fn merge_bases_of_folded(
    folded: &[ObjectHash],
    next: &ObjectHash,
) -> Result<Vec<ObjectHash>, PullMergeError> {
    merge_bases_of_folded_with(
        folded,
        next,
        |base, tip| {
            merge_base::merge_bases(base, tip)
                .map_err(|error| PullMergeError::History(error.to_string()))
        },
        |ancestor, descendant| {
            merge_base::is_ancestor(ancestor, descendant)
                .map_err(|error| PullMergeError::History(error.to_string()))
        },
    )
}

/// Candidate collection and cross-part domination for
/// [`merge_bases_of_folded`], parameterized by graph reads so the otherwise
/// rare dominated-candidate branch can be exercised on a small in-memory DAG.
fn merge_bases_of_folded_with(
    folded: &[ObjectHash],
    next: &ObjectHash,
    mut merge_bases: impl FnMut(&ObjectHash, &ObjectHash) -> Result<Vec<ObjectHash>, PullMergeError>,
    mut is_ancestor: impl FnMut(&ObjectHash, &ObjectHash) -> Result<bool, PullMergeError>,
) -> Result<Vec<ObjectHash>, PullMergeError> {
    ensure_virtual_ancestor_width(folded.len())?;
    let [single] = folded else {
        // Candidates tagged with the part (folded base) that produced them.
        let mut candidates: Vec<(usize, ObjectHash)> = Vec::new();
        for (part, base) in folded.iter().enumerate() {
            for candidate in merge_bases(base, next)? {
                if !candidates.iter().any(|(_, known)| *known == candidate) {
                    candidates.push((part, candidate));
                    ensure_virtual_ancestor_width(candidates.len())?;
                }
            }
        }
        let mut maximal = Vec::new();
        for (part, candidate) in &candidates {
            let mut dominated = false;
            for (other_part, other) in &candidates {
                if other_part == part {
                    continue;
                }
                if is_ancestor(candidate, other)? {
                    dominated = true;
                    break;
                }
            }
            if !dominated {
                maximal.push(*candidate);
            }
        }
        return Ok(virtual_base_fold_order(&maximal));
    };
    merge_bases(single, next)
}

/// The knobs every level of the virtual-ancestor fold shares.
#[derive(Clone)]
struct VirtualFold<'a> {
    /// `false` under `--dry-run`: the fold keeps its blobs in memory.
    persist: bool,
    conflict_style: diffy::ConflictStyle,
    /// The fold is a merge, so it detects renames like any other (FIX-MG05-02).
    rename_config: &'a MergeRenameConfig,
    default_driver: Option<&'a str>,
    external_merge_runtime: SharedExternalMergeRuntime,
}

/// Fold every merge base of a criss-cross history into ONE virtual ancestor
/// (ADR-MG-04, Git's `merge-ort.c:5313`).
///
/// `persist` is `false` under `--dry-run`: the fold then keeps its blobs in
/// memory and materializes nothing.
fn virtual_merge_base(
    bases: &[ObjectHash],
    gitlinks: &GitlinkEntries,
    persist: bool,
    conflict_style: diffy::ConflictStyle,
    rename_config: &MergeRenameConfig,
    default_driver: Option<&str>,
    external_merge_runtime: SharedExternalMergeRuntime,
) -> Result<VirtualAncestor, PullMergeError> {
    let mut blobs = VirtualBlobs::new();
    let fold = VirtualFold {
        persist,
        conflict_style,
        rename_config,
        default_driver,
        external_merge_runtime,
    };
    let items = fold_merge_bases(bases, gitlinks, 1, &mut blobs, fold)?;
    Ok(VirtualAncestor { items, blobs })
}

/// One level of the fold: merge `bases` pairwise, left to right in hex order,
/// into a single ancestor tree. `depth` is the `call_depth` of the merges this
/// level performs (1 for the bases of the user's merge, 2 for the bases of
/// those, …).
fn fold_merge_bases(
    bases: &[ObjectHash],
    gitlinks: &GitlinkEntries,
    depth: usize,
    blobs: &mut VirtualBlobs,
    fold: VirtualFold<'_>,
) -> Result<HashMap<PathBuf, MergeTreeEntry>, PullMergeError> {
    ensure_virtual_ancestor_depth(depth)?;
    let ordered = virtual_base_fold_order(bases);
    ensure_virtual_ancestor_width(ordered.len())?;
    let Some((first, rest)) = ordered.split_first() else {
        // No common ancestor at this level: the virtual ancestor is the empty
        // tree, exactly as an unrelated-history merge uses one.
        return Ok(HashMap::new());
    };
    let first_commit = load_merge_commit(first)?;
    let mut folded_ids = vec![*first];
    let mut timestamp = first_commit.committer.timestamp;
    let mut items = commit_tree_split_for_merge(&first_commit)?.0;
    for next in rest {
        let next_commit = load_merge_commit(next)?;
        let next_items = commit_tree_split_for_merge(&next_commit)?.0;
        let sub_bases = merge_bases_of_folded(&folded_ids, next)?;
        let sub_items = fold_merge_bases(&sub_bases, gitlinks, depth + 1, blobs, fold.clone())?;
        items = merge_virtual_items(&sub_items, &items, &next_items, depth, blobs, fold.clone())?;
        folded_ids.push(*next);
        timestamp = timestamp.max(next_commit.committer.timestamp);
        if fold.persist {
            materialize_virtual_ancestor(&items, gitlinks, &folded_ids, timestamp)?;
        }
    }
    Ok(items)
}

/// Merge one pair of ancestors inside the fold.
///
/// The difference from the user's merge is that this one can never fail: a
/// virtual ancestor is a synthetic input, so every path has to end up with
/// SOME content. Git resolves the same way — a content conflict is recorded
/// with its markers, and a modify/delete selects the original entry while
/// `call_depth` is non-zero (`merge-ort.c` `process_entry`, lines 4374-4381 at
/// git@`3cb9185f6`).
fn merge_virtual_items(
    base_items: &HashMap<PathBuf, MergeTreeEntry>,
    our_items: &HashMap<PathBuf, MergeTreeEntry>,
    their_items: &HashMap<PathBuf, MergeTreeEntry>,
    depth: usize,
    blobs: &mut VirtualBlobs,
    fold: VirtualFold<'_>,
) -> Result<HashMap<PathBuf, MergeTreeEntry>, PullMergeError> {
    // FIX-MG05-02: the fold is a merge, so it detects renames like any other.
    // Without this the virtual ancestor keeps the OLD path while the sides
    // carry the new one, and the outer merge then compares each side against a
    // base that has nothing at the renamed path — which silently resurrects
    // content one side had reverted. Measured on git 2.50.1 with bases
    // `A` (renames `old` to `new`) and `B` (edits line 2), ours merging both
    // and reverting B's edit, theirs merging both and editing line 7: Git keeps
    // the revert, and this fold used to restore `B edit`. Git runs the same
    // detection at `call_depth > 0`. The notices are dropped: Git announces
    // nothing inside a virtual merge, and the user never chose these inputs.
    let mut base_items = base_items.clone();
    let mut our_items = our_items.clone();
    let mut their_items = their_items.clone();
    // MG-06: the fold raises the same path-level rename shapes the user's own
    // merge does. It needs no `forced` conflicts, though: a virtual ancestor
    // can never fail, so every shape settles as CONTENT here — a 1to2 leaves
    // the one merged blob at both destinations and drops the source, a
    // collision leaves the rename's merge to be resolved against the occupant,
    // and a rename/delete reuses the base version — which is exactly what the
    // path-by-path resolution below produces from the maps this rewrites.
    detect_and_apply_renames(
        &mut base_items,
        &mut our_items,
        &mut their_items,
        fold.rename_config,
        fold.conflict_style,
        (VIRTUAL_OURS_LABEL, VIRTUAL_THEIRS_LABEL),
        &mut TreeMergeContext::nested_with_external(
            fold.persist,
            depth,
            fold.default_driver,
            fold.external_merge_runtime.clone(),
            blobs,
        ),
    )?;
    let (base_items, our_items, their_items) = (&base_items, &our_items, &their_items);

    let mut all_paths: BTreeSet<PathBuf> = base_items.keys().cloned().collect();
    all_paths.extend(our_items.keys().cloned());
    all_paths.extend(their_items.keys().cloned());

    let mut merged = HashMap::new();
    for path in all_paths {
        let [base, ours, theirs] = sides_without_empty_dir_beside_file(
            base_items.get(&path),
            our_items.get(&path),
            their_items.get(&path),
        );
        let resolution = {
            let mut context = TreeMergeContext {
                persist_merged_blobs: fold.persist,
                favor: None,
                depth,
                default_driver: fold.default_driver.map(str::to_owned),
                external_merge_runtime: fold.external_merge_runtime.clone(),
                ancestor_label: "merged common ancestors".to_string(),
                ours_label: VIRTUAL_OURS_LABEL.to_string(),
                theirs_label: VIRTUAL_THEIRS_LABEL.to_string(),
                virtual_blobs: blobs,
            };
            resolve_three_way(&path, base, ours, theirs, &mut context)?
        };
        let entry = match resolution {
            MergeResolution::Use(entry) => Some(entry),
            MergeResolution::Delete => None,
            MergeResolution::Conflict(kind) => {
                if let ConflictKind::BothChanged {
                    rendered: Some(entry),
                    ..
                } = kind
                {
                    merged.insert(path, entry);
                    continue;
                }
                let driver = match kind {
                    ConflictKind::BothChanged { driver, .. } => driver,
                    _ => BuiltinMergeDriver::Text,
                };
                virtual_conflict_resolution(base, ours, theirs, driver, depth, blobs, fold.clone())?
            }
        };
        if let Some(entry) = entry {
            merged.insert(path, entry);
        }
    }
    // MG-04 inside the fold (Git at `call_depth > 0`): a file whose path is
    // also a directory in the result cannot go into one tree; Git moves it to
    // `unique_path(path, "Temporary merge branch N")` and keeps folding — no
    // user is asked. Without the move the ancestor tree would carry a blob and
    // a subtree under one name.
    relocate_virtual_df_files(&mut merged, base_items, our_items, their_items);
    // Empty-directory markers stay in the ancestor: the outer merge must see
    // that the base HAD a directory there (Codex R3), and
    // `create_tree_from_items_map` writes such an entry verbatim.
    Ok(merged)
}

/// Git's D/F rule inside a recursive merge (`merge-ort.c:4120-4198` under
/// `call_depth`): the directory keeps the path and the file moves to
/// `unique_path`, labelled after the temporary branch that held it —
/// `Temporary merge branch 1` for the fold's ours, `2` for its theirs. The
/// "in the way" test is the outer merge's ([`directory_is_in_the_way`]).
fn relocate_virtual_df_files(
    merged: &mut HashMap<PathBuf, MergeTreeEntry>,
    base_items: &HashMap<PathBuf, MergeTreeEntry>,
    our_items: &HashMap<PathBuf, MergeTreeEntry>,
    their_items: &HashMap<PathBuf, MergeTreeEntry>,
) {
    let file_at = |items: &HashMap<PathBuf, MergeTreeEntry>, path: &PathBuf| {
        items
            .get(path)
            .copied()
            .filter(|entry| entry.mode != TreeItemMode::Tree)
    };
    let mut entries: Vec<(PathBuf, MergeTreeEntry)> = merged
        .iter()
        .map(|(path, entry)| (path.clone(), *entry))
        .collect();
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));
    let mut base_paths: Vec<&PathBuf> = base_items.keys().collect();
    base_paths.sort();
    let mut no_subtrees = |_: &ObjectHash| Ok(false);
    let mut moves: Vec<(PathBuf, &'static str)> = Vec::new();
    let mut drops: Vec<PathBuf> = Vec::new();
    for (path, _) in &entries {
        let label = match (file_at(our_items, path), file_at(their_items, path)) {
            (Some(_), None) => VIRTUAL_OURS_LABEL,
            (None, Some(_)) => VIRTUAL_THEIRS_LABEL,
            _ => continue,
        };
        let at = base_paths.partition_point(|candidate| candidate.as_path() < path.as_path());
        let base_present = base_paths
            .get(at)
            .is_some_and(|candidate| candidate.starts_with(path));
        // The fold's maps hold leaves and empty-directory markers only, so no
        // subtree ever needs reading here.
        let file_survives = merged
            .get(path)
            .is_some_and(|entry| entry.mode != TreeItemMode::Tree);
        if !file_survives {
            continue;
        }
        if directory_is_in_the_way(path, &entries, base_present, &mut no_subtrees).unwrap_or(false)
        {
            moves.push((path.clone(), label));
        } else {
            // Empty-only entries beneath a surviving file would put a blob and
            // a subtree under one name in the ancestor tree.
            drops.push(path.clone());
        }
    }
    for path in drops {
        merged.retain(|other, _| other == &path || !other.starts_with(&path));
    }
    if moves.is_empty() {
        return;
    }
    // The same occupancy as the outer merge: every input path (a name only
    // the base had, deleted by both folded sides, still counts), every result
    // path, and their ancestors.
    let mut taken = df_occupied_names(&[base_items, our_items, their_items], merged, &[]);
    for (path, label) in moves {
        let Some(entry) = merged.remove(&path) else {
            continue;
        };
        let target = unique_df_path(&path, label, &taken);
        taken.insert(target.clone());
        merged.insert(target, entry);
    }
}

/// Turn a conflict inside the fold into an ancestor entry (see
/// [`merge_virtual_items`]).
///
/// Every branch here mirrors Git at `call_depth > 0`, where the answer is never
/// "ask the user":
///
/// * only one side survives (modify/delete) — keep the original, because there
///   is no midpoint between "changed" and "gone" (`merge-ort.c`
///   `process_entry`, lines 4374-4381 at git@`3cb9185f6`);
/// * the two sides are different KINDS of entry, or are not regular files
///   (symlinks) — keep the original, which is *nothing* when there is none
///   (`merge-ort.c` `handle_content_merge`: `result->mode = o->mode;
///   oidcpy(&result->oid, &o->oid)` under `call_depth`);
/// * any side is binary — keep the original's CONTENT, the empty blob when
///   there is no original (`merge-ll.c` `ll_binary_merge` steals `orig` for a
///   virtual ancestor, and `read_mmblob` of a null oid is empty);
/// * otherwise merge the text and record it with its markers.
fn virtual_conflict_resolution(
    base: Option<&MergeTreeEntry>,
    ours: Option<&MergeTreeEntry>,
    theirs: Option<&MergeTreeEntry>,
    driver: BuiltinMergeDriver,
    depth: usize,
    blobs: &mut VirtualBlobs,
    fold: VirtualFold<'_>,
) -> Result<Option<MergeTreeEntry>, PullMergeError> {
    let (Some(ours), Some(theirs)) = (ours, theirs) else {
        return Ok(base.copied());
    };
    if tree_item_kind(ours.mode) != tree_item_kind(theirs.mode) || !is_regular_file_mode(ours.mode)
    {
        return Ok(base.copied());
    }
    // Git treats an original of a DIFFERENT type as no original at all and
    // merges two-way (`merge-ort.c`'s `two_way`). The MODE rule below still
    // sees the real original, exactly as Git's does.
    let base_content = base.filter(|entry| is_regular_file_mode(entry.mode));
    let base_bytes = match base_content {
        Some(entry) => Some(load_merge_blob(entry.hash, blobs)?.data),
        None => None,
    };
    let ours_blob = load_merge_blob(ours.hash, blobs)?;
    let theirs_blob = load_merge_blob(theirs.hash, blobs)?;
    let mode = virtual_merged_mode(base, ours, theirs);
    let base_bytes = base_bytes.as_deref().unwrap_or(&[]);

    let mut record = |blob: &Blob| {
        TreeMergeContext {
            persist_merged_blobs: fold.persist,
            favor: None,
            depth,
            default_driver: None,
            external_merge_runtime: fold.external_merge_runtime.clone(),
            ancestor_label: "merged common ancestors".to_string(),
            ours_label: VIRTUAL_OURS_LABEL.to_string(),
            theirs_label: VIRTUAL_THEIRS_LABEL.to_string(),
            virtual_blobs: blobs,
        }
        .record_merged_blob(blob)
    };

    if driver == BuiltinMergeDriver::Binary
        || merge_input_is_binary(base_bytes)
        || merge_input_is_binary(&ours_blob.data)
        || merge_input_is_binary(&theirs_blob.data)
    {
        let Some(entry) = base_content else {
            // No original: Git's empty buffer, materialized as the empty blob
            // so the ancestor still HAS the path (an absent one would turn the
            // outer merge's add/add into a one-sided add).
            let empty = Blob::from_content_bytes(Vec::new());
            record(&empty)?;
            return Ok(Some(MergeTreeEntry {
                hash: empty.id,
                mode,
            }));
        };
        return Ok(Some(MergeTreeEntry {
            hash: entry.hash,
            mode,
        }));
    }

    let content = merge_virtual_content(
        driver,
        base_bytes,
        &ours_blob.data,
        &theirs_blob.data,
        depth,
        fold.conflict_style,
    )
    .map_err(PullMergeError::TreeCreate)?;
    let blob = Blob::from_content_bytes(content);
    record(&blob)?;
    Ok(Some(MergeTreeEntry {
        hash: blob.id,
        mode,
    }))
}

/// Git's largest input the line-level merge accepts (`xdiff/xdiff.h`
/// `MAX_XDIFF_SIZE`, 1023 MiB). `merge-ll.c` `ll_xdl_merge` hands anything
/// larger to `ll_binary_merge` exactly as it hands NUL-carrying content.
const MAX_XDIFF_SIZE: usize = 1024 * 1024 * 1023;

/// Whether `ll_xdl_merge` would refuse to line-merge this input: larger than
/// [`MAX_XDIFF_SIZE`], or a NUL byte anywhere in the first 8000 bytes
/// (`xdiff-interface.c` `buffer_is_binary`). The NUL half is duplicated rather
/// than shared with `grep`'s private copy of the same rule — promoting it would
/// move a helper into `src/utils/`, a cross-cutting surface this card has no
/// reason to touch.
pub(crate) fn merge_input_is_binary(content: &[u8]) -> bool {
    merge_input_exceeds_xdiff_size(content.len())
        || content.iter().take(8000).any(|&byte| byte == 0)
}

/// The size half of [`merge_input_is_binary`], on the length alone so the
/// boundary can be pinned without allocating a gibibyte.
fn merge_input_exceeds_xdiff_size(len: usize) -> bool {
    len > MAX_XDIFF_SIZE
}

/// The `S_IFMT` class of a tree entry: Git only content-merges two entries of
/// the SAME class, and decides everything else elsewhere.
fn tree_item_kind(mode: TreeItemMode) -> u8 {
    match mode {
        TreeItemMode::Blob | TreeItemMode::BlobExecutable => 0,
        TreeItemMode::Link => 1,
        TreeItemMode::Tree => 2,
        TreeItemMode::Commit => 3,
    }
}

fn is_regular_file_mode(mode: TreeItemMode) -> bool {
    matches!(mode, TreeItemMode::Blob | TreeItemMode::BlobExecutable)
}

/// Git's mode rule for a conflicted content merge (`merge-ort.c`
/// `handle_content_merge`, lines 2211-2217 at git@`3cb9185f6`): take theirs
/// when the two sides agree or when ours is unchanged, otherwise keep ours.
fn virtual_merged_mode(
    base: Option<&MergeTreeEntry>,
    ours: &MergeTreeEntry,
    theirs: &MergeTreeEntry,
) -> TreeItemMode {
    if ours.mode == theirs.mode || base.is_some_and(|base| base.mode == ours.mode) {
        theirs.mode
    } else {
        ours.mode
    }
}

/// The content one path of a virtual ancestor ends up with: the clean merge
/// when there is one, otherwise the conflicted text with markers widened for
/// `depth` and labelled the way Git labels a virtual-ancestor merge.
fn merge_virtual_content(
    driver: BuiltinMergeDriver,
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    depth: usize,
    conflict_style: diffy::ConflictStyle,
) -> Result<Vec<u8>, String> {
    let marker_len = conflict_marker_length_at_depth(&[base, ours, theirs], depth);
    match merge_bytes_with_driver(driver, base, ours, theirs, None, conflict_style, 2 * depth)? {
        BuiltinMergeOutcome::Clean(merged) => Ok(merged),
        BuiltinMergeOutcome::Conflict(conflicted) => Ok(relabel_conflict_markers(
            conflicted,
            marker_len,
            VIRTUAL_OURS_LABEL,
            VIRTUAL_THEIRS_LABEL,
            "base",
        )),
    }
}

/// Write the folded ancestor as a one-shot tree + synthetic commit (ADR-MG-04).
///
/// The commit's parents are exactly the real bases folded so far, so its
/// reachability matches the chained virtual commit Git builds. It is
/// DELIBERATELY not recorded in `merge-state.json` and therefore not a GC root:
/// `maintenance gc` may reclaim it, and `merge --restart` recomputes it from
/// the real bases. Nothing reads it back — the fold derives the next step's
/// merge bases from those same real bases ([`merge_bases_of_folded`]) — so a
/// `--dry-run` can skip this entirely and still preview the same result.
fn materialize_virtual_ancestor(
    items: &HashMap<PathBuf, MergeTreeEntry>,
    gitlinks: &GitlinkEntries,
    parents: &[ObjectHash],
    timestamp: usize,
) -> Result<ObjectHash, PullMergeError> {
    let mut tree_items = items.clone();
    for (path, gitlink) in gitlinks {
        tree_items.insert(
            path.clone(),
            MergeTreeEntry {
                hash: *gitlink,
                mode: TreeItemMode::Commit,
            },
        );
    }
    let tree_id = create_tree_from_items_map(&tree_items).map_err(PullMergeError::TreeCreate)?;
    let signature = |signature_type| Signature {
        signature_type,
        name: "Libra".to_string(),
        email: "virtual-merge-base@libra.invalid".to_string(),
        timestamp,
        timezone: "+0000".to_string(),
    };
    let commit = Commit::new(
        signature(SignatureType::Author),
        signature(SignatureType::Committer),
        tree_id,
        parents.to_vec(),
        VIRTUAL_ANCESTOR_MESSAGE,
    );
    save_object(&commit, &commit.id)
        .map_err(|error| PullMergeError::CommitSave(error.to_string()))?;
    Ok(commit.id)
}

// ---------------------------------------------------------------------------
// MG-03: incremental (directory-pruning) three-way tree merge.
//
// The flattening path (`commit_tree_split` → `merge_tree_items`) reads every
// tree and lists every leaf of all three inputs before deciding anything —
// O(whole tree) object reads for a merge that may touch one file. The walk
// below is Git's `collect_merge_info_callback` (merge-ort.c:1259) reduced to
// what Libra's engine decides: it descends a directory ONLY when the three
// sides disagree about it, adopts a subtree verbatim when one side equals the
// base (the other side's subtree IS the result there), and skips a directory
// all three agree on without opening it. Leaves still go through
// `resolve_three_way`, so the resolution set is the flattening path's.
// ---------------------------------------------------------------------------

/// Where the incremental walk reads tree objects from. The object store is the
/// real source; unit tests supply an in-memory graph that COUNTS reads, which
/// is how the pruning guarantees (G1/G2) are asserted rather than assumed.
trait TreeSource {
    fn tree(&mut self, id: &ObjectHash) -> Result<Tree, PullMergeError>;
}

/// Tree objects read from the object store by every [`ObjectStoreTrees`] in
/// this process — the preflight gate's and the engine's alike — so the
/// `LIBRA_TEST_MERGE_TREE_STATS` seam reports the whole production read set
/// of a merge, not one pass of it.
static MERGE_TREE_STORE_READS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Object-store [`TreeSource`], one read per tree id per walk.
struct ObjectStoreTrees {
    cache: HashMap<ObjectHash, Tree>,
    /// Object-store read ATTEMPTS so far (cache hits excluded; a failed load
    /// counts, it was still a read) — what the `LIBRA_TEST_MERGE_TREE_STATS`
    /// seam reports for the production walk.
    reads: usize,
    /// Whether `refs/replace` substitutions apply to the trees read. The merge
    /// walk and its gate read RAW — matching the flattening path's `Tree::load`
    /// for nested trees — while the `--dry-run` availability probe reads
    /// replacement-aware, matching the checkout it stands in for
    /// (`reset::rebuild_index_from_tree` uses `load_object`). A source is one
    /// or the other for its whole life; the two views are never mixed in one
    /// cache.
    replacement_aware: bool,
}

impl ObjectStoreTrees {
    fn new() -> Self {
        Self {
            cache: HashMap::new(),
            reads: 0,
            replacement_aware: false,
        }
    }

    /// The checkout's view of trees, for the preview probe.
    fn as_checkout_sees_them() -> Self {
        Self {
            replacement_aware: true,
            ..Self::new()
        }
    }
}

impl TreeSource for ObjectStoreTrees {
    fn tree(&mut self, id: &ObjectHash) -> Result<Tree, PullMergeError> {
        if let Some(tree) = self.cache.get(id) {
            return Ok(tree.clone());
        }
        // Counted as an attempt before the load: a failed read was a read.
        self.reads += 1;
        MERGE_TREE_STORE_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let loaded = if self.replacement_aware {
            load_object(id)
        } else {
            // RAW, exactly like the flattening path's `Tree::load`:
            // `refs/replace` substitutions are not applied to merge inputs on
            // either path, so the two paths see — and record — the same ids.
            load_object_raw(id)
        };
        let tree: Tree = loaded.map_err(|error| PullMergeError::TreeLoad {
            tree_id: id.to_string(),
            detail: error.to_string(),
        })?;
        self.cache.insert(*id, tree.clone());
        Ok(tree)
    }
}

/// One side's view of a directory entry during the walk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct WalkEntry {
    id: ObjectHash,
    mode: TreeItemMode,
}

impl WalkEntry {
    fn is_tree(self) -> bool {
        self.mode == TreeItemMode::Tree
    }

    fn leaf(self) -> MergeTreeEntry {
        MergeTreeEntry {
            hash: self.id,
            mode: self.mode,
        }
    }
}

/// What the incremental walk produced for one merge.
///
/// `merged` holds leaves AND adopted subtrees: an entry whose mode is
/// [`TreeItemMode::Tree`] names a whole subtree taken verbatim from one side,
/// which `create_tree_from_items_map` writes as-is. Consumers that need leaves
/// (the conflict-path index, the untracked-collision check) expand adopted
/// subtrees with [`expand_adopted_subtrees`] — reading the ADOPTED side only,
/// never the side that equalled the base.
#[derive(Debug)]
struct IncrementalMergeResult {
    merged: HashMap<PathBuf, MergeTreeEntry>,
    conflicts: Vec<(PathBuf, ConflictKind)>,
    /// Paths whose resolution differs from `ours`, for `files_changed`. Adopted
    /// subtrees contribute through [`pruned_subtree_diff`] against ours' subtree.
    changed_paths: usize,
    /// Subtrees adopted from theirs (or added by theirs) in place of what ours
    /// had there — `(path, ours' entry if any, adopted entry)` — recorded at
    /// adoption time so the ADR-MG-01 scan never looks anything up again.
    adopted_from_theirs: Vec<(PathBuf, Option<WalkEntry>, WalkEntry)>,
    /// Paths that are a FILE on one side and a DIRECTORY on another —
    /// `(path, side holding the file, base's file if any)` — resolved into
    /// D/F conflicts by [`resolve_df_conflicts`] once the whole result is known
    /// (only a directory whose contents SURVIVE is "in the way").
    df_candidates: Vec<DfCandidate>,
    /// MG-05 rename candidates, `[ours, theirs]`: paths the side DELETED
    /// relative to the base (with the base entry and whatever the other side
    /// still has there) and paths it ADDED. Collected while the walk and its
    /// pruned subtree diffs already visit the differing paths, so COLLECTION
    /// itself costs no reads beyond what MG-03 already performs. The deferred
    /// enumeration is the part that can read more; see `unexplored`.
    rename_sources: [Vec<(PathBuf, MergeTreeEntry, Option<MergeTreeEntry>)>; 2],
    rename_dests: [Vec<(PathBuf, MergeTreeEntry)>; 2],
    /// Subtrees the pruned walk deliberately did NOT open, which may hold
    /// rename candidates.
    ///
    /// This is where detection can read more than MG-03 did, so the contract is
    /// exact (Codex R10): they are enumerated only if that side turns out to
    /// have both a possible source AND a possible destination — with neither,
    /// the merge reads exactly what MG-03 read — and enumerating opens only the
    /// subtrees that DIFFER on that side, which is that side's own diff against
    /// the base and precisely what per-side rename detection has to read (Git's
    /// detection reads each side's full diff too). Subtrees identical on both
    /// sides are never opened, so MG-03's pruning survives everywhere a rename
    /// cannot reach. Pinned by
    /// `tree::rename_collection_reads_only_the_subtrees_that_differ`.
    unexplored: [Vec<Unexplored>; 2],
    /// Whether the walk should collect the two lists above at all.
    collect_renames: bool,
}

/// Read one directory of each side (where present) into name-keyed maps.
fn read_walk_level(
    source: &mut dyn TreeSource,
    sides: [Option<WalkEntry>; 3],
) -> Result<[BTreeMap<String, WalkEntry>; 3], PullMergeError> {
    let mut levels: [BTreeMap<String, WalkEntry>; 3] = Default::default();
    // Identical tree ids are read once and shared: the object is the same.
    let mut loaded: HashMap<ObjectHash, BTreeMap<String, WalkEntry>> = HashMap::new();
    for (index, side) in sides.iter().enumerate() {
        let Some(entry) = side.filter(|entry| entry.is_tree()) else {
            continue;
        };
        if let Some(level) = loaded.get(&entry.id) {
            levels[index] = level.clone();
            continue;
        }
        let tree = source.tree(&entry.id)?;
        let level: BTreeMap<String, WalkEntry> = tree
            .tree_items
            .iter()
            .map(|item| {
                (
                    item.name.clone(),
                    WalkEntry {
                        id: item.id,
                        mode: item.mode,
                    },
                )
            })
            .collect();
        loaded.insert(entry.id, level.clone());
        levels[index] = level;
    }
    Ok(levels)
}

/// Git's `collect_merge_info_callback` shape: walk the three trees together,
/// deciding each directory from its three ids before (and instead of) opening
/// it wherever possible.
fn incremental_merge_walk(
    source: &mut dyn TreeSource,
    dir: &Path,
    sides: [Option<WalkEntry>; 3],
    context: &mut TreeMergeContext<'_>,
    out: &mut IncrementalMergeResult,
) -> Result<(), PullMergeError> {
    let levels = read_walk_level(source, sides)?;
    let mut names: BTreeSet<&String> = BTreeSet::new();
    for level in &levels {
        names.extend(level.keys());
    }
    for name in names {
        let path = dir.join(name);
        let entry = |index: usize| levels[index].get(name).copied();
        let (base, ours, theirs) = (entry(0), entry(1), entry(2));
        let all_trees = |entries: &[Option<WalkEntry>]| {
            entries
                .iter()
                .all(|entry| entry.is_none_or(|entry| entry.is_tree()))
        };

        // Gitlinks are never merged (ADR-MG-01). `incremental_gitlink_gate` has
        // already refused every pointer the three sides disagree about, so a
        // gitlink reached here is the same on all three sides: carry it through
        // verbatim, exactly as the flattening path does.
        let is_gitlink = |entry: Option<WalkEntry>| {
            entry.is_some_and(|entry| entry.mode == TreeItemMode::Commit)
        };
        if is_gitlink(base) || is_gitlink(ours) || is_gitlink(theirs) {
            if let (Some(b), Some(o), Some(t)) = (base, ours, theirs)
                && b == o
                && o == t
            {
                out.merged.insert(path, o.leaf());
            }
            continue;
        }

        if all_trees(&[base, ours, theirs])
            && (base.is_some() || ours.is_some() || theirs.is_some())
        {
            // Directory on every present side. Decide from the three ids.
            match (base, ours, theirs) {
                // All three agree: nothing under here can differ. Not opened.
                (Some(b), Some(o), Some(t)) if b == o && o == t => {
                    out.merged.insert(path, o.leaf());
                }
                // Ours equals base: theirs' subtree is the result, verbatim.
                (Some(b), Some(o), Some(t)) if b == o => {
                    out.merged.insert(path.clone(), t.leaf());
                    out.adopted_from_theirs.push((path, Some(o), t));
                }
                // Theirs equals base: ours' subtree is the result, verbatim.
                (Some(b), Some(o), Some(t)) if b == t => {
                    out.note_unexplored(
                        MergeSide::Ours,
                        &path,
                        Some(t),
                        Some(o),
                        UnexploredKind::Both,
                        // `theirs` equals the base here, so `left` IS the
                        // other side.
                        true,
                    );
                    out.merged.insert(path, o.leaf());
                }
                // Both sides made the SAME change: take it, verbatim. Both
                // sides could have made the same rename in there; recording
                // both candidates lets the rename pass move their base to the
                // shared destination and run a normal three-way merge there.
                (base_entry, Some(o), Some(t)) if o == t => {
                    for side in [MergeSide::Ours, MergeSide::Theirs] {
                        // `left` is the BASE: the other side equals this one,
                        // so it does not hold what the base held.
                        out.note_unexplored(
                            side,
                            &path,
                            base_entry,
                            Some(o),
                            UnexploredKind::Both,
                            false,
                        );
                    }
                    out.merged.insert(path, o.leaf());
                }
                // Added on one side only (no base): take it, verbatim.
                (None, Some(o), None) => {
                    out.note_unexplored(
                        MergeSide::Ours,
                        &path,
                        None,
                        Some(o),
                        UnexploredKind::DestsOnly,
                        true,
                    );
                    out.merged.insert(path, o.leaf());
                }
                (None, None, Some(t)) => {
                    out.merged.insert(path.clone(), t.leaf());
                    out.adopted_from_theirs.push((path, None, t));
                }
                // Deleted on one side, untouched on the other: gone.
                (Some(b), Some(o), None) if b == o => {
                    let mut gone = SubtreeDiff::default();
                    pruned_subtree_diff(source, &path, Some(o), None, &mut gone)?;
                    out.changed_paths += gone.changed_leaves;
                    out.note_subtree_candidates(MergeSide::Theirs, gone, true);
                }
                (Some(b), None, Some(t)) if b == t => {
                    out.note_unexplored(
                        MergeSide::Ours,
                        &path,
                        Some(t),
                        None,
                        UnexploredKind::SourcesOnly,
                        // `theirs` equals the base, so `left` IS the other side.
                        true,
                    );
                }
                // Anything else needs the entries: recurse.
                _ => incremental_merge_walk(source, &path, [base, ours, theirs], context, out)?,
            }
            continue;
        }

        if base.is_some_and(|entry| entry.is_tree())
            || ours.is_some_and(|entry| entry.is_tree())
            || theirs.is_some_and(|entry| entry.is_tree())
        {
            // A directory on some side and a file on another. The flattening
            // path saw the directory's leaves and the file as unrelated paths;
            // reproduce that exactly: recurse into the tree sides with the
            // file sides absent, and resolve the file with the tree sides
            // absent.
            let tree_sides =
                [base, ours, theirs].map(|entry| entry.filter(|entry| entry.is_tree()));
            incremental_merge_walk(source, &path, tree_sides, context, out)?;
            let file_sides =
                [base, ours, theirs].map(|entry| entry.filter(|entry| !entry.is_tree()));
            resolve_walk_leaf(&path, file_sides, context, out)?;
            // MG-04: remember which SIDE holds the file; whether the directory
            // is actually in the way is decided once the result is complete.
            let file = match (file_sides[1], file_sides[2]) {
                (Some(ours), None) => Some((MergeSide::Ours, ours.leaf())),
                (None, Some(theirs)) => Some((MergeSide::Theirs, theirs.leaf())),
                _ => None,
            };
            if let Some((file_side, file)) = file {
                out.df_candidates.push(DfCandidate {
                    path,
                    file_side,
                    file,
                    base_file: file_sides[0].map(WalkEntry::leaf),
                    // Anything at all on the base side — file or directory.
                    base_present: base.is_some(),
                });
            }
            continue;
        }

        resolve_walk_leaf(&path, [base, ours, theirs], context, out)?;
    }
    Ok(())
}

/// What a subtree the walk skipped could contribute to rename detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnexploredKind {
    /// Only the base has it: every leaf inside is a rename SOURCE.
    SourcesOnly,
    /// Only this side has it: every leaf inside is a rename DESTINATION.
    DestsOnly,
    /// The side changed it: leaves may be either.
    Both,
}

/// A subtree the pruned walk resolved without opening (MG-03), remembered so
/// rename detection can look inside — but only when it must.
#[derive(Debug, Clone)]
struct Unexplored {
    path: PathBuf,
    /// The diff the enumeration would run: base-ish side, then this side.
    left: Option<WalkEntry>,
    right: Option<WalkEntry>,
    kind: UnexploredKind,
    /// Does the OTHER side of the merge still hold what `left` holds?
    ///
    /// Almost everywhere `left` IS the other side (it equals the base there,
    /// which is why the walk could skip the subtree). The exception is the
    /// branch where both sides made the SAME change: `left` is the base and
    /// the other side is byte-identical to THIS side, so a path this side
    /// deleted is gone on the other side too. Recording the base entry as the
    /// other side's there let a rename whose source both sides deleted escape
    /// the rename/delete check, which the flattening engine reports.
    other_holds_left: bool,
}

impl IncrementalMergeResult {
    fn note_unexplored(
        &mut self,
        side: MergeSide,
        path: &Path,
        left: Option<WalkEntry>,
        right: Option<WalkEntry>,
        kind: UnexploredKind,
        other_holds_left: bool,
    ) {
        if !self.collect_renames {
            return;
        }
        let index = match side {
            MergeSide::Ours => 0,
            MergeSide::Theirs => 1,
        };
        self.unexplored[index].push(Unexplored {
            path: path.to_path_buf(),
            left,
            right,
            kind,
            other_holds_left,
        });
    }

    /// MG-05: note a leaf that one side deleted (a rename source) or added (a
    /// rename destination), with the entries the fix-up will need.
    fn note_rename_candidates(&mut self, path: &Path, sides: [Option<MergeTreeEntry>; 3]) {
        if !self.collect_renames {
            return;
        }
        let [base, ours, theirs] = sides;
        for (index, (side, other)) in [(ours, theirs), (theirs, ours)].into_iter().enumerate() {
            match (base, side) {
                (Some(base_entry), None) => self.rename_sources[index].push((
                    path.to_path_buf(),
                    base_entry,
                    // Only a FILE on the other side can take part in the
                    // three-way match at the new path.
                    other.filter(|entry| entry.mode != TreeItemMode::Tree),
                )),
                (None, Some(entry)) => self.rename_dests[index].push((path.to_path_buf(), entry)),
                _ => {}
            }
        }
    }

    /// The same, for every leaf of a subtree only one side has or changed:
    /// inside such a subtree the OTHER side equals the base, so the entries
    /// the fix-up needs are exactly the diff's.
    fn note_subtree_candidates(
        &mut self,
        side: MergeSide,
        diff: SubtreeDiff,
        other_holds_left: bool,
    ) {
        if !self.collect_renames {
            return;
        }
        let index = match side {
            MergeSide::Ours => 0,
            MergeSide::Theirs => 1,
        };
        for (path, entry) in diff.left_only {
            let other = if other_holds_left { Some(entry) } else { None };
            self.rename_sources[index].push((path, entry, other));
        }
        for (path, entry) in diff.right_only {
            self.rename_dests[index].push((path, entry));
        }
    }
}

/// Leaves go through the same [`resolve_three_way`] the flattening path uses,
/// so the two paths cannot disagree about a file.
fn resolve_walk_leaf(
    path: &Path,
    sides: [Option<WalkEntry>; 3],
    context: &mut TreeMergeContext<'_>,
    out: &mut IncrementalMergeResult,
) -> Result<(), PullMergeError> {
    let [base, ours, theirs] = sides.map(|entry| entry.map(WalkEntry::leaf));
    if base.is_none() && ours.is_none() && theirs.is_none() {
        return Ok(());
    }
    out.note_rename_candidates(path, [base, ours, theirs]);
    let resolution =
        resolve_three_way(path, base.as_ref(), ours.as_ref(), theirs.as_ref(), context)?;
    match resolution {
        MergeResolution::Use(entry) => {
            if ours != Some(entry) {
                out.changed_paths += 1;
            }
            out.merged.insert(path.to_path_buf(), entry);
        }
        MergeResolution::Delete => {
            if ours.is_some() {
                out.changed_paths += 1;
            }
        }
        MergeResolution::Conflict(kind) => {
            // A conflicted path is absent from the merged map, so the flattening
            // path's count (`count_item_map_changes` of ours vs merged) sees it
            // as changed whenever ours had an entry there. Same rule here.
            if ours.is_some() {
                out.changed_paths += 1;
            }
            out.conflicts.push((path.to_path_buf(), kind));
        }
    }
    Ok(())
}

/// What one pruned pass over two differing subtrees reports.
#[derive(Default)]
struct SubtreeDiff {
    /// Leaf paths that differ (a path present on one side only counts once, a
    /// path differing on both sides counts once) — `files_changed`'s share for
    /// an adopted subtree.
    changed_leaves: usize,
    /// MG-05: leaves only the LEFT side has (rename sources) and leaves only
    /// the RIGHT side has (rename destinations), recorded while the diff is
    /// already walking the differing paths — so rename candidates inside an
    /// adopted subtree cost nothing beyond this diff.
    left_only: Vec<(PathBuf, MergeTreeEntry)>,
    right_only: Vec<(PathBuf, MergeTreeEntry)>,
}

/// Count differing leaves between two subtrees opening ONLY the directories
/// whose ids differ — never a subtree the two sides share — so an adopted
/// subtree costs each differing directory one read per side and nothing more.
/// (Gitlink arbitration is NOT this function's job: `incremental_gitlink_gate`
/// runs before the walk and has already refused every disagreeing pointer.)
fn pruned_subtree_diff(
    source: &mut dyn TreeSource,
    path: &Path,
    left: Option<WalkEntry>,
    right: Option<WalkEntry>,
    out: &mut SubtreeDiff,
) -> Result<(), PullMergeError> {
    match (left, right) {
        (Some(l), Some(r)) if l == r => return Ok(()),
        (None, None) => return Ok(()),
        _ => {}
    }
    let tree = |entry: Option<WalkEntry>| entry.is_none_or(|entry| entry.is_tree());
    if !(tree(left) && tree(right)) {
        // A file on at least one side: the file is one changed path; a tree on
        // the other side contributes every leaf beneath it.
        let mut leaves = 0;
        for (index, side) in [left, right].into_iter().enumerate() {
            match side {
                Some(entry) if entry.is_tree() => {
                    let mut sub = SubtreeDiff::default();
                    pruned_subtree_diff(source, path, None, Some(entry), &mut sub)?;
                    leaves += sub.changed_leaves;
                    // Everything under a one-sided tree belongs to that side.
                    if index == 0 {
                        out.left_only.append(&mut sub.right_only);
                    } else {
                        out.right_only.append(&mut sub.right_only);
                    }
                }
                Some(entry) => {
                    leaves += 1;
                    // A FILE the other side does not have AS A FILE — the other
                    // side may hold nothing there, or a directory (an empty
                    // marker included). Git pairs such a path too: verified
                    // with `git merge-tree`, where `old.txt` moves onto a path
                    // that was an empty tree and the merge stays clean.
                    let opposite_is_file =
                        [left, right][1 - index].is_some_and(|other| !other.is_tree());
                    if !opposite_is_file {
                        if index == 0 {
                            out.left_only.push((path.to_path_buf(), entry.leaf()));
                        } else {
                            out.right_only.push((path.to_path_buf(), entry.leaf()));
                        }
                    }
                }
                None => {}
            }
        }
        if left.is_some_and(|e| !e.is_tree()) && right.is_some_and(|e| !e.is_tree()) {
            leaves -= 1; // the same path differing on both sides is ONE change
        }
        out.changed_leaves += leaves;
        return Ok(());
    }
    let levels = read_walk_level(source, [left, right, None])?;
    let mut names: BTreeSet<&String> = BTreeSet::new();
    names.extend(levels[0].keys());
    names.extend(levels[1].keys());
    for name in names {
        pruned_subtree_diff(
            source,
            &path.join(name),
            levels[0].get(name).copied(),
            levels[1].get(name).copied(),
            out,
        )?;
    }
    Ok(())
}

/// Replace EVERY subtree entry in `items` — adopted from theirs, kept from ours,
/// or agreed by all three — with its leaves. Used by consumers that need
/// per-file entries: the conflict path's index lists every file, so this reads
/// every carried subtree there (the conflict path is O(tree) in reads, like the
/// checkout on the clean path). The untracked-collision check passes only the
/// colliding subtrees, so it reads just those.
fn expand_adopted_subtrees(
    source: &mut dyn TreeSource,
    items: &mut HashMap<PathBuf, MergeTreeEntry>,
) -> Result<(), PullMergeError> {
    let subtrees: Vec<(PathBuf, ObjectHash)> = items
        .iter()
        .filter(|(_, entry)| entry.mode == TreeItemMode::Tree)
        .map(|(path, entry)| (path.clone(), entry.hash))
        .collect();
    for (dir, id) in subtrees {
        items.remove(&dir);
        let mut stack = vec![(dir, id)];
        while let Some((prefix, tree_id)) = stack.pop() {
            let tree = source.tree(&tree_id)?;
            for item in &tree.tree_items {
                let path = prefix.join(&item.name);
                if item.mode == TreeItemMode::Tree {
                    stack.push((path, item.id));
                } else {
                    items.insert(
                        path,
                        MergeTreeEntry {
                            hash: item.id,
                            mode: item.mode,
                        },
                    );
                }
            }
        }
    }
    Ok(())
}

/// Whether the incremental walk is in effect. Production: always. Tests may
/// force the flattening path with `LIBRA_TEST=1` plus
/// `LIBRA_TEST_MERGE_TREE_WALK=flat` — the same sentinel-gated failpoint shape
/// `am` and `stash` use — which is how the two paths are compared (G3/G4).
fn incremental_tree_walk_enabled() -> bool {
    incremental_tree_walk_enabled_for(
        std::env::var_os("LIBRA_TEST").as_deref(),
        std::env::var_os("LIBRA_TEST_MERGE_TREE_WALK").as_deref(),
    )
}

/// The pure half of [`incremental_tree_walk_enabled`]: only the exact pair
/// `LIBRA_TEST=<set>` + `LIBRA_TEST_MERGE_TREE_WALK=flat` selects the flattening
/// path; anything else — including `flat` WITHOUT the test sentinel — keeps the
/// incremental walk, so a stray variable can never change a production merge.
fn incremental_tree_walk_enabled_for(
    test_sentinel: Option<&std::ffi::OsStr>,
    walk_mode: Option<&std::ffi::OsStr>,
) -> bool {
    !(test_sentinel.is_some() && walk_mode.is_some_and(|mode| mode == "flat"))
}

/// Test seam (`LIBRA_TEST=1` + `LIBRA_TEST_MERGE_TREE_STATS=<file>`, the same
/// sentinel shape as the failpoints): record which tree walk the PRODUCTION
/// merge took and how many tree objects the whole merge read from the object
/// store — the preflight gate's pass AND the engine's pass (each has its own
/// per-pass cache, so the changed-path set is read once per pass) — so the
/// pruning guarantees can be asserted through the CLI rather than only on an
/// in-memory graph. Written at each EXIT of the incremental engine, after its
/// last source-backed consumer (the untracked-collision rechecks, the
/// conflict-path expansion, the preview's availability probe). Inert without
/// both variables; a failure to write the file is ignored (it is evidence,
/// never behaviour).
fn report_incremental_walk_stats() {
    report_tree_walk_stats(
        "incremental",
        Some(MERGE_TREE_STORE_READS.load(std::sync::atomic::Ordering::Relaxed)),
    );
}

fn report_tree_walk_stats(walk: &str, tree_reads: Option<usize>) {
    if std::env::var_os("LIBRA_TEST").is_none() {
        return;
    }
    let Some(path) = std::env::var_os("LIBRA_TEST_MERGE_TREE_STATS") else {
        return;
    };
    let stats = serde_json::json!({ "walk": walk, "tree_reads": tree_reads });
    let _ = fs::write(path, stats.to_string());
}

/// Run the incremental walk over three root trees, gate first.
///
/// **Unopened-tree invariant.** Every tree object the result references but
/// this function never opened is already referenced by `ours` (HEAD). Proof:
/// the gate recurses into every directory the three sides do not all agree
/// on, so a directory it leaves unopened has the same id on all present sides
/// — in particular on ours; inside an adopted-from-theirs subtree the same
/// holds one level down (the gate stopped only where theirs' nested tree equals
/// base's, and base's equals ours' there). Newly added directories have no
/// counterpart and are enumerated in full. Consequently the merge can never
/// introduce an unreadable tree the flattening path would have caught: a
/// missing tree the walk did not open is pre-existing corruption of the
/// checked-out commit, failing identically on both paths at checkout. This is
/// what lets the result carry subtrees by id without a validation pass whose
/// cost would be the size of the repository (`unopened_trees_are_heads_own`
/// pins it). Availability — as opposed to ownership — is settled by the
/// clean path's write ORDER: the checkout, which reads every carried tree,
/// runs before the commit and HEAD are written. `None` for `base` is the virtual empty tree of
/// an unrelated-history merge.
fn incremental_merge_trees(
    source: &mut dyn TreeSource,
    base: Option<ObjectHash>,
    ours: ObjectHash,
    theirs: ObjectHash,
    context: &mut TreeMergeContext<'_>,
    collect_renames: bool,
) -> Result<(IncrementalMergeResult, GitlinkEntries), PullMergeError> {
    let root = |id: ObjectHash| WalkEntry {
        id,
        mode: TreeItemMode::Tree,
    };
    let sides = [base.map(root), Some(root(ours)), Some(root(theirs))];
    // ADR-MG-01 FIRST, read-only: refuse before the merge walk can persist a
    // single auto-merged blob. The gate opens exactly the directories the walk
    // would (those the sides disagree about), so with a caching source the walk
    // re-reads none of them from the object store.
    let passthrough = incremental_gitlink_gate(source, &sides)?;
    let mut out = IncrementalMergeResult {
        merged: HashMap::new(),
        conflicts: Vec::new(),
        changed_paths: 0,
        adopted_from_theirs: Vec::new(),
        df_candidates: Vec::new(),
        rename_sources: [Vec::new(), Vec::new()],
        rename_dests: [Vec::new(), Vec::new()],
        unexplored: [Vec::new(), Vec::new()],
        collect_renames,
    };
    incremental_merge_walk(source, Path::new(""), sides, context, &mut out)?;
    // `files_changed` for adopted subtrees: one pruned diff each, against what
    // ours had there.
    let adopted = std::mem::take(&mut out.adopted_from_theirs);
    for (dir, replaced, adopted_entry) in &adopted {
        let mut scan = SubtreeDiff::default();
        pruned_subtree_diff(source, dir, *replaced, Some(*adopted_entry), &mut scan)?;
        out.changed_paths += scan.changed_leaves;
        out.note_subtree_candidates(MergeSide::Theirs, scan, true);
    }
    out.adopted_from_theirs = adopted;
    // MG-05 (Codex R2): open the subtrees the walk skipped ONLY for a side
    // that has both a possible rename source and a possible destination — a
    // merge without renames reads exactly what MG-03 read. When both are
    // present, `pruned_subtree_diff` opens only the subtrees that DIFFER on
    // that side (see `unexplored` for the measured contract).
    for index in 0..2 {
        let side = if index == 0 {
            MergeSide::Ours
        } else {
            MergeSide::Theirs
        };
        let deferred = std::mem::take(&mut out.unexplored[index]);
        let may_hold = |kind: UnexploredKind, wanted: UnexploredKind| {
            kind == wanted || kind == UnexploredKind::Both
        };
        let has_source = !out.rename_sources[index].is_empty()
            || deferred
                .iter()
                .any(|entry| may_hold(entry.kind, UnexploredKind::SourcesOnly));
        let has_dest = !out.rename_dests[index].is_empty()
            || deferred
                .iter()
                .any(|entry| may_hold(entry.kind, UnexploredKind::DestsOnly));
        if !(has_source && has_dest) {
            continue;
        }
        for entry in deferred {
            let mut diff = SubtreeDiff::default();
            pruned_subtree_diff(source, &entry.path, entry.left, entry.right, &mut diff)?;
            out.note_subtree_candidates(side, diff, entry.other_holds_left);
        }
    }
    // The D/F collisions the walk recorded are NOT settled here: on this
    // engine the result is not complete until the rename fix-up has run.
    // `settle_incremental_df_conflicts` is the caller's job.
    Ok((out, passthrough))
}

/// Settle the D/F collisions the pruned walk recorded, once the result really
/// is complete — which on this engine means AFTER the rename fix-up, not at
/// the end of the walk.
///
/// An accepted rename can move the last file out of a directory the other side
/// replaced with a file, and the collision then does not exist. Measured on
/// git 2.50.1 with base `dir/y` + `new/child`, ours editing `new/child`, and
/// theirs renaming `dir/y` to `new` and `new/child` to `a`: accepting
/// `new/child` -> `a` empties `new/`, and `git merge` (like
/// `git merge-tree --write-tree --messages`) merges cleanly, holding `a` and a
/// plain file `new`. Settling before the fix-up instead reported
/// `CONFLICT (file/directory) ... moving it to new~theirs`, which the
/// flattening engine — whose order has always been renames, resolve, D/F —
/// never did.
fn settle_incremental_df_conflicts(
    source: &mut dyn TreeSource,
    merged: &mut HashMap<PathBuf, MergeTreeEntry>,
    conflicts: &mut Vec<(PathBuf, ConflictKind)>,
    candidates: Vec<DfCandidate>,
    files_changed: &mut usize,
) -> Result<(), PullMergeError> {
    // A carried subtree beneath a collision has to be read to know whether it
    // holds a file at all (only collisions pay this).
    let mut has_file = |id: &ObjectHash| subtree_holds_a_file(source, id);
    let delta = resolve_df_conflicts(merged, conflicts, candidates, &mut has_file)?;
    *files_changed = files_changed.saturating_add_signed(delta);
    Ok(())
}

/// MG-05: how merge asks [`rename_detect`] for per-side renames.
///
/// Git runs detection once per side of the merge (`merge-ort.c:3429`
/// `detect_regular_renames`) over the paths that side ADDED against the paths
/// it DELETED relative to the merge base, then rewrites the third side's
/// entries onto the new path (`:2913` `process_renames`).
#[derive(Debug, Clone)]
struct MergeRenameConfig {
    enabled: bool,
    threshold: u32,
    rename_limit: usize,
    directory_renames: DirectoryRenameMode,
}

/// Git's three directory-rename modes (`merge.directoryRenames`). `conflict`
/// is the default: infer the destination, but leave every relocated path
/// unmerged so the user explicitly confirms it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectoryRenameMode {
    False,
    Conflict,
    True,
}

/// Git's effective `merge.renameLimit` when the value is unset or <= 0
/// (`merge-ort.c:3452-3453`, git@3cb9185f6). Git starts the option at -1
/// (`:5504`), so "unset", `0` and any negative all land here.
const GIT_MERGE_RENAME_LIMIT_DEFAULT: usize = 7000;

impl Default for MergeRenameConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            // Git's default similarity index: 50% on the 0..=60000 scale.
            threshold: 30_000,
            // Git's `merge.renameLimit` default, read straight from its own
            // source: merge-ort.c:3452-3453 maps any value <= 0 (including
            // "unset", which starts at -1 on :5504) to 7000. Measured too —
            // `git -c merge.renameLimit=0` detects renames a 30-path merge
            // needs, so `0` is the default and not "no cap".
            rename_limit: GIT_MERGE_RENAME_LIMIT_DEFAULT,
            directory_renames: DirectoryRenameMode::Conflict,
        }
    }
}

/// `merge.renames` falling back to `diff.renames`, and `merge.renameLimit`
/// falling back to `diff.renameLimit` — the same cascade Git uses, read
/// through the STRICT cascaded reader (`--local` through `--system`, the same
/// one `status` and `diff` use for their rename keys) and parsed with Git's
/// own boolean and integer rules: an unparseable value is an error, never a
/// silent "detection on".
async fn merge_rename_config() -> Result<MergeRenameConfig, PullMergeError> {
    use crate::internal::config::{
        LocalIdentityTarget, parse_git_config_bool, parse_git_config_int,
        read_cascaded_config_value_strict,
    };

    async fn read(key: &str) -> Result<Option<String>, PullMergeError> {
        read_cascaded_config_value_strict(LocalIdentityTarget::CurrentRepo, key)
            .await
            .map_err(|error| PullMergeError::RenameConfigRead {
                key: key.to_string(),
                detail: format!("{error:#}"),
            })
    }
    async fn first(keys: [&str; 2]) -> Result<Option<(String, String)>, PullMergeError> {
        for key in keys {
            if let Some(value) = read(key).await? {
                return Ok(Some((key.to_string(), value)));
            }
        }
        Ok(None)
    }

    let mut config = MergeRenameConfig::default();
    if let Some((key, value)) = first(["merge.renames", "diff.renames"]).await? {
        let trimmed = value.trim();
        config.enabled = match trimmed.to_ascii_lowercase().as_str() {
            // Git accepts these and treats them as "renames on, plus copies";
            // Libra detects no copies, so renames stay on.
            "copy" | "copies" => true,
            _ => parse_git_config_bool(trimmed).ok_or_else(|| {
                PullMergeError::InvalidRenameConfig {
                    key,
                    value: value.clone(),
                    expected: "true, false, copy or copies",
                }
            })?,
        };
    }
    if let Some((key, value)) = first(["merge.renameLimit", "diff.renameLimit"]).await? {
        // Git's integer suffixes are case-insensitive (`1K` == `1k`), and the
        // shared parser wants them lowercase — as `status` and `diff` do.
        // Git accepts any integer here and maps everything <= 0 onto its
        // default — `0` is NOT "no cap", and a negative value is Git's own
        // "unset" sentinel rather than an error (`merge-ort.c:3452-3453`,
        // `:5504`). Only a value that is not an integer at all is fatal, which
        // is what Git reports as `bad numeric config value`.
        let parsed = parse_git_config_int(&value.trim().to_ascii_lowercase()).ok_or_else(|| {
            PullMergeError::InvalidRenameConfig {
                key,
                value: value.clone(),
                expected: "an integer",
            }
        })?;
        config.rename_limit = usize::try_from(parsed)
            .ok()
            .filter(|limit| *limit > 0)
            .unwrap_or(GIT_MERGE_RENAME_LIMIT_DEFAULT);
    }
    if let Some(value) = read("merge.directoryRenames").await? {
        let trimmed = value.trim();
        config.directory_renames = match trimmed.to_ascii_lowercase().as_str() {
            "conflict" => DirectoryRenameMode::Conflict,
            _ => match parse_git_config_bool(trimmed) {
                Some(true) => DirectoryRenameMode::True,
                Some(false) => DirectoryRenameMode::False,
                None => {
                    return Err(PullMergeError::InvalidRenameConfig {
                        key: "merge.directoryRenames".to_string(),
                        value,
                        expected: "true, false or conflict",
                    });
                }
            },
        };
    }
    Ok(config)
}

/// Blob content for the rename engine. A merge compares two committed trees,
/// so there is no working-tree side — but there IS a virtual ancestor whose
/// blobs a `--dry-run` keeps in memory only (MG-02), and those must score
/// exactly like the real merge's. Reads go through the same bounded,
/// deadline-carrying reader `diff` uses, shared across both sides of the merge
/// and cached by object id so a blob is read at most once per merge.
struct MergeRenameReader {
    objects: rename_detect::ObjectReadBudget,
    cache: HashMap<ObjectHash, std::rc::Rc<Vec<u8>>>,
}

impl MergeRenameReader {
    fn new() -> Self {
        Self {
            objects: rename_detect::ObjectReadBudget::new(
                u64::MAX,
                u64::MAX,
                u32::MAX,
                rename_detect::OBJECT_READ_DEADLINE,
            ),
            cache: HashMap::new(),
        }
    }

    fn content(
        &mut self,
        entry: &MergeTreeEntry,
        virtual_blobs: &VirtualBlobs,
    ) -> rename_detect::ContentOutcome {
        if let Some(bytes) = self.cache.get(&entry.hash) {
            return rename_detect::ContentOutcome::Content(bytes.clone());
        }
        // A blob the recursive fold synthesized lives only in memory under
        // `--dry-run`; scoring must see it, or a preview would pair renames
        // differently from the merge it previews.
        if let Some(data) = virtual_blobs.get(&entry.hash) {
            let bytes = std::rc::Rc::new(data.clone());
            self.cache.insert(entry.hash, bytes.clone());
            return rename_detect::ContentOutcome::Content(bytes);
        }
        match self.objects.read_blob_tracked(&entry.hash) {
            (rename_detect::ContentOutcome::Content(bytes), _) => {
                self.cache.insert(entry.hash, bytes.clone());
                rename_detect::ContentOutcome::Content(bytes)
            }
            // A COMPLETED skip means the worker structurally cannot serve the
            // object (local-only, frame-capped); fall back in-process exactly
            // as `diff` does. A TRANSPORT skip keeps the killable bound final.
            (
                rename_detect::ContentOutcome::Skipped(reason),
                rename_detect::ObjectReadProvenance::Completed,
            ) => match load_object::<Blob>(&entry.hash) {
                Ok(blob) => {
                    let bytes = std::rc::Rc::new(blob.data);
                    self.cache.insert(entry.hash, bytes.clone());
                    rename_detect::ContentOutcome::Content(bytes)
                }
                Err(_) => rename_detect::ContentOutcome::Skipped(reason),
            },
            (skip, rename_detect::ObjectReadProvenance::Transport) => skip,
        }
    }
}

#[derive(Clone, Copy)]
enum RenameSide {
    Old,
    New,
}

struct MergeRenameSource<'a> {
    old: &'a HashMap<PathBuf, MergeTreeEntry>,
    new: &'a HashMap<PathBuf, MergeTreeEntry>,
    virtual_blobs: &'a VirtualBlobs,
    reader: &'a mut MergeRenameReader,
}

impl MergeRenameSource<'_> {
    fn read(&mut self, side: RenameSide, path: &Path) -> rename_detect::ContentOutcome {
        let map = match side {
            RenameSide::Old => self.old,
            RenameSide::New => self.new,
        };
        let Some(entry) = map.get(path).copied() else {
            return rename_detect::ContentOutcome::Skipped(
                rename_detect::SkipReason::ObjectMissing,
            );
        };
        self.reader.content(&entry, self.virtual_blobs)
    }
}

impl rename_detect::RenameContentSource for MergeRenameSource<'_> {
    fn old_content(
        &mut self,
        path: &Path,
        _blob: &rename_detect::BlobRef,
    ) -> rename_detect::ContentOutcome {
        self.read(RenameSide::Old, path)
    }

    fn new_content(
        &mut self,
        path: &Path,
        _blob: &rename_detect::BlobRef,
    ) -> rename_detect::ContentOutcome {
        self.read(RenameSide::New, path)
    }
}

/// The rename candidates of ONE side: what it deleted relative to the base
/// (sources) and what it added (destinations). Gitlinks never take part —
/// arbitrating a submodule pointer is refused long before this (ADR-MG-01).
fn rename_snapshot(
    base: &HashMap<PathBuf, MergeTreeEntry>,
    side: &HashMap<PathBuf, MergeTreeEntry>,
    // Git turns empty-blob pairing off for the MERGE (`rename_empty = 0`), but
    // its diffstat is an ordinary diff and pairs them as usual — so a merge
    // that emptied the source still reports `1 file changed` for a pure rename.
    // The exclusion therefore belongs to detection, not to reporting
    // (Codex R21).
    allow_empty: bool,
) -> rename_detect::RenameSnapshot {
    let blob_ref = |entry: &MergeTreeEntry| -> Option<rename_detect::BlobRef> {
        let mode = tree_item_mode_to_index_mode(entry.mode).ok()?;
        if entry.mode == TreeItemMode::Commit || entry.mode == TreeItemMode::Tree {
            return None;
        }
        Some(rename_detect::BlobRef {
            kind: rename_detect::BlobKind::from_mode(mode),
            mode,
            size: None,
            evidence: rename_detect::BlobEvidence::KnownObjectId { oid: entry.hash },
        })
    };
    // "Present on the other side" means present AS A FILE: an empty-directory
    // marker (MG-04) is not a file, so a path that turns from an empty tree
    // into a file is a rename destination, and one that turns from a file into
    // an empty tree is a rename source. Verified with `git merge-tree`: moving
    // `old.txt` onto a path that was an empty tree merges cleanly as a rename.
    let file_at = |items: &HashMap<PathBuf, MergeTreeEntry>, path: &PathBuf| {
        items
            .get(path)
            .is_some_and(|entry| entry.mode != TreeItemMode::Tree)
    };
    // Git turns rename detection OFF for EMPTY blobs when it merges
    // (`merge-ort.c:3449` sets `diff_opts.flags.rename_empty = 0`), and it has
    // to: every empty file is a 100% match for every other, so an emptied or
    // newly-created placeholder would pair with anything. Measured on
    // git 2.50.1 — base holds an empty `old`, ours moves it to an empty `new`,
    // theirs fills `old` — Git stops at `CONFLICT (modify/delete)` and keeps
    // theirs' content on `old`'s stage 3; pairing them instead carries that
    // content to `new`, deletes `old` and exits 0, losing the conflict. `diff`
    // and `status` keep Git's own default (`rename_empty = 1`), so the filter
    // lives here in merge's adapter rather than in the shared engine.
    let empty_blob = Blob::from_content_bytes(Vec::new()).id;
    let is_empty = |entry: &MergeTreeEntry| !allow_empty && entry.hash == empty_blob;
    let mut snapshot = rename_detect::RenameSnapshot::default();
    for (path, entry) in base {
        if file_at(side, path) || is_empty(entry) {
            continue;
        }
        if let Some(reference) = blob_ref(entry) {
            snapshot.old_map.insert(path.clone(), reference);
        }
    }
    for (path, entry) in side {
        if file_at(base, path) || is_empty(entry) {
            continue;
        }
        if let Some(reference) = blob_ref(entry) {
            snapshot.new_map.insert(path.clone(), reference);
        }
    }
    snapshot
}

/// One side's detected renames, plus whether the engine had to give up on the
/// inexact stage (Git still keeps exact and unique-basename pairs then).
struct SideRenames {
    matches: Vec<rename_detect::RenameMatch>,
    skipped_by_limit: bool,
}

/// One provisional directory rename selected by Git's plurality rule: the
/// unique destination with the largest number of file renames wins. A tie is
/// kept separately because it is a split-directory conflict, not a rename.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectoryRename {
    old: PathBuf,
    new: PathBuf,
    side: MergeSide,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectoryRenameSplit {
    old: PathBuf,
    destinations: Vec<PathBuf>,
    side: MergeSide,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct DirectoryRenamePlan {
    renames: Vec<DirectoryRename>,
    splits: Vec<DirectoryRenameSplit>,
}

/// Aggregate per-file rename pairs into provisional directory renames.
///
/// Each pair votes for its immediate parent and every non-root ancestor whose
/// relative suffix can be removed from the destination. The unique highest
/// count wins; a tied highest count is a split. This is Git's
/// `get_provisional_directory_renames` rule — it is deliberately a plurality,
/// not a strict-majority test.
fn infer_provisional_directory_renames(
    matches: &[rename_detect::RenameMatch],
    side: MergeSide,
) -> DirectoryRenamePlan {
    let mut votes: BTreeMap<PathBuf, BTreeMap<PathBuf, usize>> = BTreeMap::new();
    for pair in matches {
        for old_dir in pair
            .old
            .ancestors()
            .skip(1)
            .take_while(|path| !path.as_os_str().is_empty())
        {
            let Ok(relative) = pair.old.strip_prefix(old_dir) else {
                continue;
            };
            let mut new_dir = pair.new.as_path();
            let mut valid = true;
            for _ in relative.components() {
                let Some(parent) = new_dir.parent() else {
                    valid = false;
                    break;
                };
                new_dir = parent;
            }
            if !valid || new_dir.as_os_str().is_empty() || new_dir == old_dir {
                continue;
            }
            *votes
                .entry(old_dir.to_path_buf())
                .or_default()
                .entry(new_dir.to_path_buf())
                .or_default() += 1;
        }
    }

    let mut plan = DirectoryRenamePlan::default();
    for (old, destinations) in votes {
        let Some(highest) = destinations.values().copied().max() else {
            continue;
        };
        let winners: Vec<PathBuf> = destinations
            .into_iter()
            .filter_map(|(path, count)| (count == highest).then_some(path))
            .collect();
        match winners.as_slice() {
            [new] => plan.renames.push(DirectoryRename {
                old,
                new: new.clone(),
                side,
            }),
            [] => {}
            _ => plan.splits.push(DirectoryRenameSplit {
                old,
                destinations: winners,
                side,
            }),
        }
    }
    // Apply the most specific mapping first. This keeps a nested rename from
    // being consumed by an ancestor mapping and is deterministic across map
    // iteration order.
    plan.renames.sort_by(|left, right| {
        right
            .old
            .components()
            .count()
            .cmp(&left.old.components().count())
            .then_with(|| left.old.cmp(&right.old))
    });
    plan
}

fn detect_side_renames(
    base: &HashMap<PathBuf, MergeTreeEntry>,
    side: &HashMap<PathBuf, MergeTreeEntry>,
    config: &MergeRenameConfig,
    virtual_blobs: &VirtualBlobs,
    reader: &mut MergeRenameReader,
) -> SideRenames {
    detect_side_renames_inner(base, side, config, virtual_blobs, reader, false)
}

/// [`detect_side_renames`] for the REPORTING paths, which pair empty blobs the
/// way Git's diffstat does.
fn detect_side_renames_for_report(
    base: &HashMap<PathBuf, MergeTreeEntry>,
    side: &HashMap<PathBuf, MergeTreeEntry>,
    config: &MergeRenameConfig,
    virtual_blobs: &VirtualBlobs,
    reader: &mut MergeRenameReader,
) -> SideRenames {
    detect_side_renames_inner(base, side, config, virtual_blobs, reader, true)
}

fn detect_side_renames_inner(
    base: &HashMap<PathBuf, MergeTreeEntry>,
    side: &HashMap<PathBuf, MergeTreeEntry>,
    config: &MergeRenameConfig,
    virtual_blobs: &VirtualBlobs,
    reader: &mut MergeRenameReader,
    allow_empty: bool,
) -> SideRenames {
    let snapshot = rename_snapshot(base, side, allow_empty);
    if snapshot.old_map.is_empty() || snapshot.new_map.is_empty() {
        return SideRenames {
            matches: Vec::new(),
            skipped_by_limit: false,
        };
    }
    let mut source = MergeRenameSource {
        old: base,
        new: side,
        virtual_blobs,
        reader,
    };
    let outcome = rename_detect::match_pairs(
        &snapshot,
        &rename_detect::RenameDetectConfig {
            threshold: config.threshold,
            rename_limit: config.rename_limit,
            comparison_budget: None,
        },
        &mut source,
    );
    SideRenames {
        matches: outcome.matches,
        skipped_by_limit: outcome.stats.skipped_by_limit,
    }
}

/// How a detected rename must be handled when it cannot use the ordinary
/// one-sided remap. These are path-level classifications, not all failures:
/// some are resolved by specialized rename handling and only real conflicts
/// produce a user-visible line.
#[derive(Debug, PartialEq, Eq)]
enum RenameDeclined {
    /// Both sides renamed the same file to different paths: handled as a
    /// rename/rename conflict.
    DivergentRenames { theirs: PathBuf },
    /// Both sides renamed it to the SAME path: move the base there and merge.
    SameDestination,
    /// The other side DELETED the source — Git's rename/delete
    /// (`merge-ort.c:3213-3221`).
    SourceDeleted,
    /// The other side replaced the source with something of a different kind —
    /// a symbolic link, a directory, a gitlink. Git takes its own
    /// `type_changed` branch (`merge-ort.c:3205-3212`): the base still follows
    /// the rename to the new path, but the source is NOT reported as deleted
    /// and the type-changed entry SURVIVES under the old name. Measured on
    /// git 2.50.1 (`/Volumes/Data/tmp/mg06-git/ktype`): the result holds
    /// `120000 old` beside a `new` conflicted at stages 1 and 2, and the only
    /// message is `CONFLICT (modify/delete): new deleted in <them> and
    /// modified in <us>.` — never `rename/delete`.
    SourceTypeChanged,
    /// The other side has a file at the exact destination (rename/add,
    /// MG-06). The collision consumes the source even though the rename does
    /// not take the ordinary accepted path through the fix-up.
    DestinationCollision,
    /// A file ancestor, descendant, or directory marker structurally blocks
    /// the destination. Unlike an exact collision, the rename does not happen
    /// and its source continues to occupy its old path.
    DestinationBlocked,
}

/// A rename accepted into the three-way match, or the reason it was not.
struct RenameDecision {
    old: PathBuf,
    new: PathBuf,
    side: MergeSide,
    declined: Option<RenameDeclined>,
}

/// Rename candidates whose destinations can be blocked by one path becoming
/// occupied. A file at `a/b` blocks a destination at `a`, while a file at `a`
/// blocks a destination at `a/b`; both directions matter when a declined
/// rename puts its source back into the occupancy index.
struct DestinationDependents<'a> {
    exact: HashMap<&'a Path, Vec<usize>>,
    strict_descendants: HashMap<&'a Path, Vec<usize>>,
}

impl<'a> DestinationDependents<'a> {
    fn from_pairs(
        pairs: &[(usize, &'a rename_detect::RenameMatch)],
        declined: &[Option<RenameDeclined>],
    ) -> Self {
        let mut exact: HashMap<&Path, Vec<usize>> = HashMap::new();
        let mut strict_descendants: HashMap<&Path, Vec<usize>> = HashMap::new();
        for (slot, (_, pair)) in pairs.iter().enumerate() {
            if declined[slot].is_some() {
                continue;
            }
            exact.entry(pair.new.as_path()).or_default().push(slot);
            for ancestor in pair
                .new
                .ancestors()
                .skip(1)
                .take_while(|path| !path.as_os_str().is_empty())
            {
                strict_descendants.entry(ancestor).or_default().push(slot);
            }
        }
        Self {
            exact,
            strict_descendants,
        }
    }

    fn requeue_blocked_by(&self, occupied: &Path, queue: &mut Vec<usize>) {
        // Destinations equal to or above the occupied path are blocked by an
        // entry beneath them.
        for path in occupied
            .ancestors()
            .take_while(|path| !path.as_os_str().is_empty())
        {
            if let Some(affected) = self.exact.get(path) {
                queue.extend(affected.iter().copied());
            }
        }
        // Destinations below the occupied path are blocked when that occupant
        // is a file. Requeueing directory destinations is harmless; the
        // occupancy check below remains the source of truth for entry kind.
        if let Some(affected) = self.strict_descendants.get(occupied) {
            queue.extend(affected.iter().copied());
        }
    }
}

/// Decide which detected renames the three-way match can use. One place, so
/// the two engines and the tests see the same rules.
fn decide_renames(
    base: &HashMap<PathBuf, MergeTreeEntry>,
    ours: &HashMap<PathBuf, MergeTreeEntry>,
    theirs: &HashMap<PathBuf, MergeTreeEntry>,
    our_matches: &[rename_detect::RenameMatch],
    their_matches: &[rename_detect::RenameMatch],
) -> Vec<RenameDecision> {
    let sides = [ours, theirs];
    let by_old: [HashMap<&PathBuf, &PathBuf>; 2] = [
        our_matches
            .iter()
            .map(|pair| (&pair.old, &pair.new))
            .collect(),
        their_matches
            .iter()
            .map(|pair| (&pair.old, &pair.new))
            .collect(),
    ];
    // Every pair, tagged with the side that made it, so the two passes below
    // can look at all of them at once.
    let pairs: Vec<(usize, &rename_detect::RenameMatch)> = our_matches
        .iter()
        .map(|pair| (0usize, pair))
        .chain(their_matches.iter().map(|pair| (1usize, pair)))
        .collect();

    // PASS 1 — everything that does not depend on occupancy.
    let mut declined: Vec<Option<RenameDeclined>> = Vec::with_capacity(pairs.len());
    for (index, pair) in &pairs {
        let other = sides[1 - index];
        let this = sides[*index];
        declined.push(match by_old[1 - index].get(&pair.old) {
            Some(destination) if **destination == pair.new => Some(RenameDeclined::SameDestination),
            Some(destination) => Some(RenameDeclined::DivergentRenames {
                theirs: (*destination).clone(),
            }),
            // The other side must still hold the source AS A FILE, and as the
            // same KIND the rename moved, for a three-way match at the new
            // path. Nothing there at all is Git's rename/delete; something of
            // a DIFFERENT kind (a directory, an empty marker, a symlink) is its
            // `type_changed` branch instead — the two are split below because
            // Git reports and resolves them differently.
            None if !other.get(&pair.old).is_some_and(|entry| {
                entry.mode != TreeItemMode::Tree
                    && this
                        .get(&pair.new)
                        .is_some_and(|moved| same_entry_kind(entry.mode, moved.mode))
            }) =>
            {
                Some(if other.contains_key(&pair.old) {
                    RenameDeclined::SourceTypeChanged
                } else {
                    RenameDeclined::SourceDeleted
                })
            }
            None => None,
        });
    }

    // PASS 2 — occupancy, as a fixed point.
    //
    // A rename takes its source away, so that source stops occupying its own
    // path and the directories above it, and two renames can free each other's
    // destination. But a rename structurally BLOCKED by an ancestor,
    // descendant, or marker never happens, so its source stays — and that can
    // block a further rename in turn. An exact file collision is different:
    // MG-06 resolves it at the destination and consumes the source, as Git
    // does. Releasing once and deciding once gets the structural case wrong
    // (Codex R15 gave the first, R16 the second, which left conflicting index
    // entries for `new` and `new/child`). So: release every eligible source,
    // then re-take only the structurally blocked ones, re-checking the renames
    // that re-taking could affect. Each rename is blocked at most once, so the
    // walk terminates and costs the paths' depth, not the square of the rename
    // count.
    let base_names: HashSet<PathBuf> = occupied_names(base.keys());
    let mut occupancy = [
        side_occupancy(base, sides[0]),
        side_occupancy(base, sides[1]),
    ];
    // Only an entry `side_occupancy` actually COUNTED may be released, and it
    // must be released with the same facts it was counted with. An entry the
    // other side carries unchanged from the base was never counted — releasing
    // it anyway decremented some other occupant's count to zero and let a
    // blocked rename through, which committed a file and a directory under one
    // name (Codex R17).
    let counted_at = |side: usize, path: &PathBuf| -> Option<bool> {
        let entry = sides[side].get(path)?;
        if base.get(path) == Some(entry) {
            return None;
        }
        Some(entry.mode == TreeItemMode::Tree)
    };
    for (slot, (index, pair)) in pairs.iter().enumerate() {
        if declined[slot].is_some() {
            continue;
        }
        let other = 1 - index;
        if let Some(marker) = counted_at(other, &pair.old) {
            occupancy[other].apply(&pair.old, marker, true, -1);
        }
    }
    let destination_dependents = DestinationDependents::from_pairs(&pairs, &declined);
    let mut queue: Vec<usize> = (0..pairs.len())
        .filter(|slot| declined[*slot].is_none())
        .collect();
    while let Some(slot) = queue.pop() {
        if declined[slot].is_some() {
            continue;
        }
        let (index, pair) = pairs[slot];
        let other = 1 - index;
        if !occupancy[other].occupied(&pair.new, base, &base_names) {
            continue;
        }
        let exact_collision = occupancy[other].file_at_path(&pair.new);
        declined[slot] = Some(if exact_collision {
            RenameDeclined::DestinationCollision
        } else {
            RenameDeclined::DestinationBlocked
        });
        if exact_collision {
            continue;
        }
        // The source stays where it is, so it occupies its path again.
        if let Some(marker) = counted_at(other, &pair.old) {
            occupancy[other].apply(&pair.old, marker, true, 1);
            destination_dependents.requeue_blocked_by(&pair.old, &mut queue);
        }
    }

    pairs
        .into_iter()
        .zip(declined)
        .map(|((index, pair), declined)| RenameDecision {
            old: pair.old.clone(),
            new: pair.new.clone(),
            side: if index == 0 {
                MergeSide::Ours
            } else {
                MergeSide::Theirs
            },
            declined,
        })
        .collect()
}

/// Do two tree entries hold the same KIND of thing — a regular file (either
/// mode), a symbolic link, a directory, a gitlink?
///
/// A rename pairs like with like: the source the other side turned into a
/// symlink cannot be carried onto the new path. Git takes its own
/// `type_changed` branch for this (`merge-ort.c:3205-3212`) — NOT rename/delete
/// — and Libra classifies it as [`RenameDeclined::SourceTypeChanged`]. Measured
/// on git 2.50.1 with base `old` (a regular file), ours renaming it to `new`
/// and theirs replacing `old` with a symlink: the only message is
/// `CONFLICT (modify/delete): new deleted in <them> and modified in <us>.`,
/// `new` carries stages 1 and 2, and the SYMLINK SURVIVES at `old` in the
/// result tree (`/Volumes/Data/tmp/mg06-git/ktype`).
///
/// Accepting the pair instead would let the type-changed entry be remapped onto
/// the new path, where the ordinary three-way match takes it as the only change
/// — and the renamed file's content disappears from a merge that exits 0.
///
/// (An earlier revision of this note said Git "reports rename/delete" here,
/// which contradicted the measurement quoted in the same paragraph — Codex R1
/// P2-6.)
fn same_entry_kind(left: TreeItemMode, right: TreeItemMode) -> bool {
    fn kind(mode: TreeItemMode) -> u8 {
        match mode {
            TreeItemMode::Blob | TreeItemMode::BlobExecutable => 0,
            TreeItemMode::Link => 1,
            TreeItemMode::Tree => 2,
            TreeItemMode::Commit => 3,
        }
    }
    kind(left) == kind(right)
}

/// Did the merge base have ANY entry at or under `path`? MG-04's base-presence
/// rule turns on exactly this, and the pruned walk does not carry the base's
/// full map — so the base tree is walked down the path's components, which
/// costs its depth in (cached) reads and only for a destination whose sole
/// occupant is an empty-directory marker.
/// One directory listing per tree id, and one answer per path, for the whole
/// fix-up (Codex R16). Without the listings each probe cloned the base root and
/// scanned it linearly, so N destinations under a root of N entries cost Θ(N²)
/// even though the object-store reads themselves were bounded.
#[derive(Default)]
struct BasePresence {
    listings: HashMap<ObjectHash, HashMap<String, (ObjectHash, TreeItemMode)>>,
    answers: HashMap<PathBuf, bool>,
}

fn base_holds_anything_at(
    source: &mut dyn TreeSource,
    base_tree: Option<ObjectHash>,
    path: &Path,
    cache: &mut BasePresence,
) -> Result<bool, PullMergeError> {
    let Some(root) = base_tree else {
        return Ok(false);
    };
    if let Some(answer) = cache.answers.get(path) {
        return Ok(*answer);
    }
    let mut current = root;
    let mut answer = true;
    for component in path.components() {
        let name = component.as_os_str().to_string_lossy().to_string();
        if let std::collections::hash_map::Entry::Vacant(slot) = cache.listings.entry(current) {
            slot.insert(
                source
                    .tree(&current)?
                    .tree_items
                    .into_iter()
                    .map(|item| (item.name, (item.id, item.mode)))
                    .collect(),
            );
        }
        let Some((id, mode)) = cache
            .listings
            .get(&current)
            .and_then(|listing| listing.get(&name))
            .copied()
        else {
            answer = false;
            break;
        };
        if mode != TreeItemMode::Tree {
            // A file at a prefix of the path: the base had something here.
            answer = true;
            break;
        }
        current = id;
    }
    cache.answers.insert(path.to_path_buf(), answer);
    Ok(answer)
}

/// How many surviving entries live at or under each path, so a rename that
/// takes its source away releases exactly what that source held — and puts it
/// back if the rename turns out to be blocked after all.
///
/// Counting rather than keeping a flat set is what makes several departing
/// siblings work: with `new/child` AND `new/second` both renamed away, neither
/// one alone empties `new`, and a "does any other entry remain" scan answers
/// "yes" for both, so neither release fires (Codex R16). Counts also make a
/// release cost the path's depth instead of a scan of the whole side, which is
/// what turned N renames into Θ(N²) work.
#[derive(Default)]
struct DestinationOccupancy {
    /// Files — anything that is not a directory marker — at or under a path.
    files: HashMap<PathBuf, usize>,
    /// Non-directory entries at their exact paths. `files` answers whether
    /// anything is at or below a destination; this companion index answers
    /// whether a FILE sits on the way to a descendant destination.
    files_at_path: HashMap<PathBuf, usize>,
    /// Empty-directory markers under a path. A marker never counts at its OWN
    /// path, only the directories above it, and blocks a destination only where
    /// the merge base had nothing there (MG-04's base-presence rule).
    markers: HashMap<PathBuf, usize>,
}

impl DestinationOccupancy {
    /// Count one entry in or out. `delta` is +1 to take, -1 to release.
    ///
    /// `at_own_path` is false for an entry that IS what a rename would produce
    /// there — the pruned walk's own result at the destination must not block
    /// the rename that creates it — and for an empty-directory marker, which
    /// only ever occupies the directories above it.
    fn apply(&mut self, path: &Path, marker: bool, at_own_path: bool, delta: i64) {
        if !marker && at_own_path {
            let slot = self.files_at_path.entry(path.to_path_buf()).or_insert(0);
            *slot = slot.saturating_add_signed(delta as isize);
        }
        let counts = if marker {
            &mut self.markers
        } else {
            &mut self.files
        };
        let mut current = if marker || !at_own_path {
            path.parent()
        } else {
            Some(path)
        };
        while let Some(prefix) = current {
            if prefix.as_os_str().is_empty() {
                break;
            }
            let slot = counts.entry(prefix.to_path_buf()).or_insert(0);
            *slot = slot.saturating_add_signed(delta as isize);
            current = prefix.parent();
        }
    }

    fn file_at_path(&self, path: &Path) -> bool {
        self.files_at_path.get(path).is_some_and(|count| *count > 0)
    }

    fn file_occupied(&self, path: &Path) -> bool {
        self.files.get(path).is_some_and(|count| *count > 0)
            || path
                .ancestors()
                .skip(1)
                .take_while(|ancestor| !ancestor.as_os_str().is_empty())
                .any(|ancestor| {
                    self.files_at_path
                        .get(ancestor)
                        .is_some_and(|count| *count > 0)
                })
    }

    /// Is anything left at or under `path` once the accepted renames have taken
    /// their sources away?
    fn occupied(
        &self,
        path: &Path,
        base: &HashMap<PathBuf, MergeTreeEntry>,
        base_names: &HashSet<PathBuf>,
    ) -> bool {
        if self.file_occupied(path) {
            return true;
        }
        self.markers.get(path).is_some_and(|count| *count > 0)
            && !base_holds_anything(base, base_names, path)
    }
}

/// Did the merge base have ANY entry at, under, or on the way to `path`?
///
/// MG-04's base-presence rule turns on exactly this, and the two engines have
/// to answer it the same way (Codex R18 caught them disagreeing). Three cases
/// count, and the third is the one a set of "base paths plus their ancestors"
/// misses: the base may hold a FILE at a PREFIX of the path — base `d` is a
/// file while the destination is `d/new` — and Git treats that as the base
/// having had something there, so the directory is traversed rather than
/// adopted whole. The pruned walk's `base_holds_anything_at` reaches the same
/// answer by walking the base tree down the path's components.
fn base_holds_anything(
    base: &HashMap<PathBuf, MergeTreeEntry>,
    // `occupied_names(base.keys())`, computed once: every base path and every
    // directory above one. Scanning the base per call would put the quadratic
    // cost R16 removed straight back.
    base_names: &HashSet<PathBuf>,
    path: &Path,
) -> bool {
    // At the path itself, or anything beneath it.
    if base_names.contains(path) {
        return true;
    }
    // Or a FILE strictly on the way to it — the case a name set alone cannot
    // express. It must be a FILE: an empty-directory marker at a prefix means
    // the base had a directory there and nothing in it, which does not make the
    // destination base-present (Codex R19 — accepting markers here let the
    // flattening walk commit a rename git and the pruned walk both refuse).
    path.ancestors()
        .skip(1)
        .take_while(|ancestor| !ancestor.as_os_str().is_empty())
        .any(|ancestor| {
            base.get(ancestor)
                .is_some_and(|entry| entry.mode != TreeItemMode::Tree)
        })
}

/// The other side's surviving entries, counted. "Surviving" is the same fact
/// `rename_destination_occupancy` measured: an entry the other side merely
/// carries UNCHANGED from the base is deleted by the merge once the renaming
/// side has cleared the destination, so it is not in the way.
fn side_occupancy(
    base: &HashMap<PathBuf, MergeTreeEntry>,
    other: &HashMap<PathBuf, MergeTreeEntry>,
) -> DestinationOccupancy {
    let mut occupancy = DestinationOccupancy::default();
    for (path, entry) in other {
        if base.get(path) == Some(entry) {
            continue;
        }
        occupancy.apply(path, entry.mode == TreeItemMode::Tree, true, 1);
    }
    occupancy
}

/// How many accepted renames the given side made — the moves the merge
/// performs on the OTHER side's behalf, which `files_changed` must count.
/// Would Git's diffstat still render this pair as ONE `old => new` line?
///
/// A merge may pair two blobs that the FINAL diff no longer considers similar
/// enough — ours deleting most of the file while theirs renames and edits it
/// drops HEAD-to-result similarity under the threshold, and `git merge` then
/// reports two changed files, not one (Codex R20). The question is asked with
/// the same engine and the same configuration detection used, over just this
/// one pair, so it costs two (cached) blob reads per accepted rename.
fn pair_reads_as_a_rename(
    old: &MergeTreeEntry,
    new: &MergeTreeEntry,
    config: &MergeRenameConfig,
    virtual_blobs: &VirtualBlobs,
    reader: &mut MergeRenameReader,
) -> bool {
    if old.hash == new.hash {
        return true;
    }
    let old_path = PathBuf::from("old");
    let new_path = PathBuf::from("new");
    let before: HashMap<PathBuf, MergeTreeEntry> = [(old_path.clone(), *old)].into_iter().collect();
    let after: HashMap<PathBuf, MergeTreeEntry> = [(new_path.clone(), *new)].into_iter().collect();
    let detected = detect_side_renames_for_report(&before, &after, config, virtual_blobs, reader);
    detected
        .matches
        .iter()
        .any(|pair| pair.old == old_path && pair.new == new_path)
}

/// How many accepted renames by `side` the remapped comparison CANNOT see.
///
/// A rename the other side made moves a file ours still had at the old path,
/// and after the remap both maps hold the same path — so a PURE rename is
/// invisible in the comparison and has to be counted here. A rename that also
/// changed the content is NOT invisible: the comparison already reports that
/// path as changed, and adding one again double-counts it. Git renders either
/// as a single `old => new` line — measured with theirs renaming `old` to `new`
/// AND editing a line while ours touched an unrelated file: `1 file changed`
/// (Codex R19).
fn unseen_renames_by(
    decisions: &[RenameDecision],
    side: MergeSide,
    ours: &HashMap<PathBuf, MergeTreeEntry>,
    merged: &HashMap<PathBuf, MergeTreeEntry>,
    config: &MergeRenameConfig,
    virtual_blobs: &VirtualBlobs,
) -> usize {
    let mut reader = MergeRenameReader::new();
    decisions
        .iter()
        .filter(|decision| decision.declined.is_none() && decision.side == side)
        .filter(|decision| {
            let (Some(before), Some(after)) = (ours.get(&decision.new), merged.get(&decision.new))
            else {
                return true;
            };
            // Unchanged content: the comparison sees nothing, Git sees one.
            // Changed content that still reads as a rename: both see one.
            // Changed content that no longer does: the comparison sees one,
            // Git sees two — so it needs the extra count as well.
            before == after
                || !pair_reads_as_a_rename(before, after, config, virtual_blobs, &mut reader)
        })
        .count()
}

/// MG-06: a path-level rename conflict, in the words Git reports it with.
///
/// The paths live here rather than inside [`ConflictKind`], which stays `Copy`
/// because it travels through the whole conflict pipeline by value.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RenameConflictNote {
    /// `CONFLICT (rename/rename): <old> renamed to <a> in <ours> and to <b> in
    /// <theirs>.` — `merge-ort.c:3069-3076`. One note per PAIR: Git handles
    /// both sides' entries in a single step (`merge-ort.c:3078`, `i++`).
    RenameRename {
        old: PathBuf,
        ours_path: PathBuf,
        theirs_path: PathBuf,
    },
    /// `CONFLICT (rename/delete): <old> renamed to <new> in <X>, but deleted in
    /// <Y>.` — `merge-ort.c:3215-3221`.
    RenameDelete {
        old: PathBuf,
        new: PathBuf,
        rename_side: MergeSide,
    },
    /// `CONFLICT (rename involved in collision): rename of <old> -> <new> has
    /// content conflicts AND collides with another path; this may result in
    /// nested conflict markers.` — `merge-ort.c:3169-3178`, printed ONLY when
    /// the rename's OWN content merge came out unclean.
    Collision { old: PathBuf, new: PathBuf },
    /// A path added by the non-renaming side follows an inferred directory
    /// rename. `conflict` distinguishes `merge.directoryRenames=conflict`
    /// from the automatic `true` mode.
    DirectoryMove {
        old: PathBuf,
        new: PathBuf,
        added_side: MergeSide,
        rename_side: MergeSide,
        conflict: bool,
    },
    /// No unique directory destination won. Kept deterministic and visible;
    /// the affected paths remain at their original names.
    DirectorySplit { old: PathBuf },
}

/// Relocate additions (including ordinary rename destinations) made by the
/// opposite side through the inferred directory rename. Base paths themselves
/// are left alone: the regular rename pass below already carries those to
/// their per-file destinations.
#[allow(clippy::too_many_arguments)]
fn apply_directory_renames(
    base: &HashMap<PathBuf, MergeTreeEntry>,
    ours: &mut HashMap<PathBuf, MergeTreeEntry>,
    theirs: &mut HashMap<PathBuf, MergeTreeEntry>,
    our_matches: &mut [rename_detect::RenameMatch],
    their_matches: &mut [rename_detect::RenameMatch],
    config: &MergeRenameConfig,
    conflict_style: diffy::ConflictStyle,
    branches: (&str, &str),
    context: &mut TreeMergeContext<'_>,
    forced: &mut Vec<(PathBuf, ConflictKind)>,
) -> Result<Vec<RenameConflictNote>, PullMergeError> {
    if config.directory_renames == DirectoryRenameMode::False || context.depth != 0 {
        return Ok(Vec::new());
    }

    let ours_plan = infer_provisional_directory_renames(our_matches, MergeSide::Ours);
    let theirs_plan = infer_provisional_directory_renames(their_matches, MergeSide::Theirs);
    let mut notes = Vec::new();

    // A split is actionable only when the renaming side actually removed the
    // old directory. Otherwise it is just a set of unrelated file renames.
    for split in ours_plan.splits.iter().chain(theirs_plan.splits.iter()) {
        let (renaming, other): (
            &HashMap<PathBuf, MergeTreeEntry>,
            &HashMap<PathBuf, MergeTreeEntry>,
        ) = match split.side {
            MergeSide::Ours => (&*ours, &*theirs),
            MergeSide::Theirs => (&*theirs, &*ours),
        };
        if !renaming.keys().any(|path| path.starts_with(&split.old)) {
            let mut affected: Vec<(PathBuf, MergeTreeEntry)> = other
                .iter()
                .filter(|(path, entry)| {
                    entry.mode != TreeItemMode::Tree
                        && path.starts_with(&split.old)
                        && base
                            .get(*path)
                            .is_none_or(|base_entry| base_entry.mode == TreeItemMode::Tree)
                })
                .map(|(path, entry)| (path.clone(), *entry))
                .collect();
            affected.sort_by(|(left, _), (right, _)| left.cmp(right));
            let Some((conflict_path, content)) = affected.into_iter().next() else {
                continue;
            };
            if !forced.iter().any(|(path, _)| path == &conflict_path) {
                forced.push((conflict_path, ConflictKind::DirectorySplit { content }));
            }
            notes.push(RenameConflictNote::DirectorySplit {
                old: split.old.clone(),
            });
        }
    }

    for rename in ours_plan.renames.into_iter().chain(theirs_plan.renames) {
        let (renaming, other, other_matches, added_side): (
            &HashMap<PathBuf, MergeTreeEntry>,
            &mut HashMap<PathBuf, MergeTreeEntry>,
            &mut [rename_detect::RenameMatch],
            MergeSide,
        ) = match rename.side {
            MergeSide::Ours => (&*ours, &mut *theirs, their_matches, MergeSide::Theirs),
            MergeSide::Theirs => (&*theirs, &mut *ours, our_matches, MergeSide::Ours),
        };
        // Git's directory rename rule applies only when the old directory is
        // gone on the side that supplied the file renames.
        if renaming.keys().any(|path| path.starts_with(&rename.old)) {
            continue;
        }

        let mut additions: Vec<PathBuf> = other
            .iter()
            .filter(|(path, entry)| {
                entry.mode != TreeItemMode::Tree
                    && path.starts_with(&rename.old)
                    && base
                        .get(*path)
                        .is_none_or(|base_entry| base_entry.mode == TreeItemMode::Tree)
            })
            .map(|(path, _)| path.clone())
            .collect();
        additions.sort();
        for old_path in additions {
            let Ok(relative) = old_path.strip_prefix(&rename.old) else {
                continue;
            };
            let new_path = rename.new.join(relative);
            if new_path == old_path {
                continue;
            }
            let Some(moved) = other.get(&old_path).copied() else {
                continue;
            };

            // Never overwrite a second path from the SAME side. Leave the
            // source addressable and make the ambiguity explicit instead.
            if other.contains_key(&new_path) {
                forced.push((
                    old_path.clone(),
                    ConflictKind::RenameMerged {
                        content: moved,
                        kind: RenameConflictKind::DirectoryRename,
                    },
                ));
                notes.push(RenameConflictNote::DirectoryMove {
                    old: old_path,
                    new: new_path,
                    added_side,
                    rename_side: rename.side,
                    conflict: true,
                });
                continue;
            }

            other.remove(&old_path);
            other.insert(new_path.clone(), moved);
            // If this addition was itself a regular rename destination, the
            // later per-file pass must use its relocated name too.
            for pair in other_matches.iter_mut() {
                if pair.new == old_path {
                    pair.new = new_path.clone();
                }
            }

            let conflict = config.directory_renames == DirectoryRenameMode::Conflict;
            if conflict {
                let counterpart = renaming
                    .get(&new_path)
                    .copied()
                    .filter(|entry| entry.mode != TreeItemMode::Tree);
                let kind = match (rename.side, counterpart) {
                    (MergeSide::Ours, Some(ours_entry)) => rename_destination_conflict(
                        &new_path,
                        &ours_entry,
                        &moved,
                        branches.1,
                        conflict_style,
                        context,
                    )?,
                    (MergeSide::Theirs, Some(theirs_entry)) => rename_destination_conflict(
                        &new_path,
                        &moved,
                        &theirs_entry,
                        branches.1,
                        conflict_style,
                        context,
                    )?,
                    (_, None) => ConflictKind::RenameMerged {
                        content: moved,
                        kind: RenameConflictKind::DirectoryRename,
                    },
                };
                forced.push((new_path.clone(), kind));
            }
            notes.push(RenameConflictNote::DirectoryMove {
                old: old_path,
                new: new_path,
                added_side,
                rename_side: rename.side,
                conflict,
            });
        }
    }
    Ok(notes)
}

/// MG-06: the content merge Git runs for a rename before it settles the
/// path-level conflict.
///
/// `handle_content_merge` is called with `extra_marker_size = 1 + 2 *
/// call_depth` (`merge-ort.c:3027` for rename/rename(1to2), `:3161` for a
/// collision), so a rename-involved conflict's markers are ONE character
/// longer than an ordinary content conflict's, and the labels are
/// `<branch>:<path>` rather than the bare branch name. Measured on git 2.50.1
/// (`/Volumes/Data/tmp/mg06-git/r1to2c`): `<<<<<<<< HEAD:a` / `========` /
/// `>>>>>>>> theirs:b`.
///
/// Unlike [`try_merge_blob_contents`] this returns a blob even on a conflict —
/// Git passes `record_object = true` and stores a new result when needed, which
/// lets both destinations of a 1to2 point at the same object. Trivial merges
/// reuse an input object. Returns the entry and whether the merge was clean.
#[allow(clippy::too_many_arguments)]
fn merge_rename_content(
    path: &Path,
    base: Option<&MergeTreeEntry>,
    ours: &MergeTreeEntry,
    theirs: &MergeTreeEntry,
    ours_label: &str,
    theirs_label: &str,
    base_label: &str,
    conflict_style: diffy::ConflictStyle,
    context: &mut TreeMergeContext<'_>,
) -> Result<(MergeTreeEntry, bool), PullMergeError> {
    // Git merges the mode independently of the content: the side that differs
    // from the base wins, and when BOTH differ from it there is no answer —
    // `handle_content_merge` keeps ours' and reports the merge UNCLEAN
    // (`merge-ort.c:2211-2218`), which is what makes a collision print Git's
    // `rename involved in collision` line even for a mode-only divergence
    // (Codex R1 P2-3).
    let (merged_mode, mode_clean) = match base {
        Some(base) if ours.mode != theirs.mode => {
            if ours.mode == base.mode {
                (theirs.mode, true)
            } else if theirs.mode == base.mode {
                (ours.mode, true)
            } else {
                (ours.mode, false)
            }
        }
        None if ours.mode != theirs.mode => (ours.mode, false),
        _ => (ours.mode, true),
    };
    // Git handles trivial OID merges before the file-type-specific rules
    // (`merge-ort.c:2243-2246`). A binary pure rename must carry the other
    // side's source edit, just as a text rename does, without invoking the
    // unmergeable-binary fallback or losing the independently merged mode.
    let trivial_hash =
        if ours.hash == theirs.hash || base.is_some_and(|base| ours.hash == base.hash) {
            Some(theirs.hash)
        } else if base.is_some_and(|base| theirs.hash == base.hash) {
            Some(ours.hash)
        } else {
            None
        };
    if let Some(hash) = trivial_hash {
        return Ok((
            MergeTreeEntry {
                hash,
                mode: merged_mode,
            },
            mode_clean,
        ));
    }
    // `-X ours` / `-X theirs` reach the rename's own content merge too: Git
    // maps `MERGE_VARIANT_OURS`/`THEIRS` onto `ll_opts.variant`
    // (`merge-ort.c:2129-2143`), so the favoured side settles the hunks and the
    // merge comes out clean. `context.favor` is always `None` inside the
    // virtual-ancestor fold, exactly as Git disables the variant at
    // `call_depth > 0` (Codex R1 P1-1).
    let favor = context.favor;
    // A symlink, a gitlink, or binary content has no line-level merge:
    // `ll_binary_merge` takes one whole side. Without `-X` this path keeps
    // ours and reports UNCLEAN; with `-X` it takes the favoured side and is clean.
    if !is_regular_file_mode(ours.mode) || !is_regular_file_mode(theirs.mode) {
        if context.depth > 0 {
            // Git restores the full original entry for recursive symlink
            // conflicts, but keeps clean=false (merge-ort.c:2301-2305).
            // Every rename caller supplies an original; an absent entry needs
            // the optional-path result of virtual_conflict_resolution instead.
            let original = base.copied().ok_or_else(|| {
                PullMergeError::TreeCreate(format!(
                    "cannot construct the virtual ancestor for {base_label}: \
                     the renamed non-file entry has no original version"
                ))
            })?;
            return Ok((original, false));
        }
        return Ok(match favor {
            Some(MergeFavor::Ours) => (*ours, true),
            Some(MergeFavor::Theirs) => (*theirs, true),
            None => (*ours, false),
        });
    }
    // A different original kind is a two-way content merge in Git; its mode
    // still participates independently above (merge-ort.c:2254-2263).
    let base_blob = match base.filter(|entry| is_regular_file_mode(entry.mode)) {
        Some(base) => Some(load_merge_blob(base.hash, context.virtual_blobs)?),
        None => None,
    };
    let ours_blob = load_merge_blob(ours.hash, context.virtual_blobs)?;
    let theirs_blob = load_merge_blob(theirs.hash, context.virtual_blobs)?;
    let base_data: &[u8] = match &base_blob {
        Some(blob) => &blob.data,
        None => &[],
    };
    let driver = context.driver_for_path(path);
    if matches!(driver, SelectedMergeDriver::Builtin(_))
        && (driver.is_builtin(BuiltinMergeDriver::Binary)
            || merge_input_is_binary(base_data)
            || merge_input_is_binary(&ours_blob.data)
            || merge_input_is_binary(&theirs_blob.data))
    {
        if context.depth > 0 {
            // ll_binary_merge(virtual_ancestor) returns orig with LL_MERGE_OK.
            // Preserve the merged mode; no regular original means an empty
            // blob, not either side's bytes or a missing ancestor path.
            let hash = match &base_blob {
                Some(original) => original.id,
                None => {
                    let empty = Blob::from_content_bytes(Vec::new());
                    context.record_merged_blob(&empty)?;
                    empty.id
                }
            };
            return Ok((
                MergeTreeEntry {
                    hash,
                    mode: merged_mode,
                },
                mode_clean,
            ));
        }
        // ll_binary_merge selects bytes only; handle_content_merge retains
        // the mode result even when the selected side has a different mode.
        let (hash, content_clean) = match (driver.fallback_builtin(), favor) {
            (BuiltinMergeDriver::Union, _) | (_, None) => (ours.hash, false),
            (_, Some(MergeFavor::Ours)) => (ours.hash, true),
            (_, Some(MergeFavor::Theirs)) => (theirs.hash, true),
        };
        return Ok((
            MergeTreeEntry {
                hash,
                mode: merged_mode,
            },
            content_clean && mode_clean,
        ));
    }
    let marker_len = conflict_marker_length_at_depth(
        &[base_data, &ours_blob.data, &theirs_blob.data],
        context.depth,
    )
    .saturating_add(1);
    let outcome = match &driver {
        SelectedMergeDriver::Builtin(driver) => merge_bytes_with_driver(
            *driver,
            base_data,
            &ours_blob.data,
            &theirs_blob.data,
            favor,
            conflict_style,
            1 + 2 * context.depth,
        )
        .map_err(PullMergeError::TreeCreate)?,
        SelectedMergeDriver::External(driver) => run_external_merge_driver(
            &context.external_merge_runtime,
            driver,
            ExternalMergeInput {
                path,
                base_id: base.map_or_else(
                    || Blob::from_content_bytes(Vec::new()).id,
                    |entry| entry.hash,
                ),
                ours_id: ours.hash,
                theirs_id: theirs.hash,
                base: base_data,
                ours: &ours_blob.data,
                theirs: &theirs_blob.data,
                marker_length: marker_len,
                labels: ExternalMergeLabels {
                    ancestor: base_label,
                    ours: ours_label,
                    theirs: theirs_label,
                },
            },
        )
        .map_err(PullMergeError::TreeCreate)?,
    };
    let external = matches!(driver, SelectedMergeDriver::External(_));
    let (bytes, clean) = match outcome {
        BuiltinMergeOutcome::Clean(bytes) => (bytes, true),
        BuiltinMergeOutcome::Conflict(bytes) if external => (bytes, false),
        BuiltinMergeOutcome::Conflict(bytes) => (
            relabel_conflict_markers(bytes, marker_len, ours_label, theirs_label, base_label),
            false,
        ),
    };
    let blob = Blob::from_content_bytes(bytes);
    context.record_merged_blob(&blob)?;
    Ok((
        MergeTreeEntry {
            hash: blob.id,
            mode: merged_mode,
        },
        // A mode divergence with no answer keeps the merge unclean even when
        // the CONTENT merged cleanly.
        clean && mode_clean,
    ))
}

/// Merge a colliding destination's content without clearing its path conflict.
fn rename_destination_conflict(
    path: &Path,
    ours: &MergeTreeEntry,
    theirs: &MergeTreeEntry,
    upstream: &str,
    conflict_style: diffy::ConflictStyle,
    context: &mut TreeMergeContext<'_>,
) -> Result<ConflictKind, PullMergeError> {
    let ours_blob = load_merge_blob(ours.hash, context.virtual_blobs)?;
    let theirs_blob = load_merge_blob(theirs.hash, context.virtual_blobs)?;
    let driver = context.driver_for_path(path);
    let content = if ours.hash == theirs.hash {
        *ours
    } else if !is_regular_file_mode(ours.mode)
        || !is_regular_file_mode(theirs.mode)
        || (matches!(driver, SelectedMergeDriver::Builtin(_))
            && (driver.is_builtin(BuiltinMergeDriver::Binary)
                || merge_input_is_binary(&ours_blob.data)
                || merge_input_is_binary(&theirs_blob.data)))
    {
        // Git's binary fallback selects one complete input. Even a favored
        // result still carries the path conflict and both original stages.
        match (driver.fallback_builtin(), context.favor) {
            (BuiltinMergeDriver::Union, _) | (_, Some(MergeFavor::Ours) | None) => *ours,
            (_, Some(MergeFavor::Theirs)) => *theirs,
        }
    } else {
        // A base-less add/add still merges against the empty blob. In
        // particular, an empty added file has no conflicting hunk for -X to
        // choose, so it must not erase the other side's nonempty content.
        match try_merge_blob_contents(path, None, *ours, *theirs, driver.clone(), context)? {
            BlobMergeAttempt::Clean(merged)
            | BlobMergeAttempt::Conflict {
                rendered: Some(merged),
                ..
            } => MergeTreeEntry {
                hash: merged.hash,
                mode: ours.mode,
            },
            BlobMergeAttempt::Conflict { .. } | BlobMergeAttempt::NotApplicable => {
                let bytes = if driver.is_builtin(BuiltinMergeDriver::Binary) {
                    ours_blob.data
                } else {
                    both_changed_conflict_content(
                        None,
                        &ours_blob.data,
                        &theirs_blob.data,
                        conflict_marker_eol(),
                        upstream,
                        conflict_style,
                    )
                    .map_err(PullMergeError::TreeCreate)?
                };
                let blob = Blob::from_content_bytes(bytes);
                context.record_merged_blob(&blob)?;
                MergeTreeEntry {
                    hash: blob.id,
                    mode: ours.mode,
                }
            }
        }
    };
    Ok(ConflictKind::RenameMerged {
        content,
        kind: RenameConflictKind::Content,
    })
}

/// Rewrite the base and the non-renaming side onto the new path so the
/// ordinary three-way match sees one triple there (Git's `process_renames`).
/// The source path disappears from all three maps: it is the same file.
#[allow(clippy::too_many_arguments)]
fn apply_renames(
    base: &mut HashMap<PathBuf, MergeTreeEntry>,
    ours: &mut HashMap<PathBuf, MergeTreeEntry>,
    theirs: &mut HashMap<PathBuf, MergeTreeEntry>,
    decisions: &[RenameDecision],
    forced: &mut Vec<(PathBuf, ConflictKind)>,
    conflict_style: diffy::ConflictStyle,
    // How each side is named in a conflict marker. `HEAD` / the upstream ref
    // for the merge the user asked for, and Git's virtual-ancestor labels
    // inside the fold — Git labels a rename-involved merge `<branch>:<path>`
    // at every call depth.
    branches: (&str, &str),
    context: &mut TreeMergeContext<'_>,
) -> Result<Vec<RenameConflictNote>, PullMergeError> {
    let label = |side: MergeSide, path: &Path| {
        let branch = match side {
            MergeSide::Ours => branches.0,
            MergeSide::Theirs => branches.1,
        };
        format!("{branch}:{}", path.display())
    };
    let mut notes = Vec::new();
    // A rename/rename(1to2) arrives as one decision per side; Git reports and
    // resolves the PAIR once (`merge-ort.c:3078` consumes `i+1` as well).
    let mut folded: HashSet<PathBuf> = HashSet::new();
    for decision in decisions {
        match &decision.declined {
            // A plain rename, and rename/rename(1to1): Git carries the base to
            // the new path for both. For 1to1 (`merge-ort.c:2991-3018`) BOTH
            // sides already hold the destination, so moving the base is the
            // whole of it and the ordinary content merge decides the rest —
            // which is why 1to1 is a CLEAN merge in Git whenever the contents
            // agree, not the base-less add/add Libra degraded it to before.
            None | Some(RenameDeclined::SameDestination) => {
                let Some(base_entry) = base.remove(&decision.old) else {
                    continue;
                };
                base.insert(decision.new.clone(), base_entry);
                let other: &mut HashMap<PathBuf, MergeTreeEntry> = match decision.side {
                    MergeSide::Ours => &mut *theirs,
                    MergeSide::Theirs => &mut *ours,
                };
                if let Some(entry) = other.remove(&decision.old) {
                    other.insert(decision.new.clone(), entry);
                }
            }
            Some(RenameDeclined::DivergentRenames { theirs: other_path }) => {
                if !folded.insert(decision.old.clone()) {
                    continue;
                }
                let (ours_path, theirs_path) = match decision.side {
                    MergeSide::Ours => (decision.new.clone(), other_path.clone()),
                    MergeSide::Theirs => (other_path.clone(), decision.new.clone()),
                };
                let (Some(base_entry), Some(ours_entry), Some(theirs_entry)) = (
                    base.get(&decision.old).copied(),
                    ours.get(&ours_path).copied(),
                    theirs.get(&theirs_path).copied(),
                ) else {
                    continue;
                };
                let (merged, clean) = merge_rename_content(
                    &ours_path,
                    Some(&base_entry),
                    &ours_entry,
                    &theirs_entry,
                    &label(MergeSide::Ours, &ours_path),
                    &label(MergeSide::Theirs, &theirs_path),
                    &format!("base:{}", decision.old.display()),
                    conflict_style,
                    context,
                )?;
                // Git's `was_binary_blob` fallback (`merge-ort.c:3032-3053`):
                // when the content merge could not actually be performed it
                // just TOOK one whole side, so copying that side's blob to both
                // destinations would overwrite the other side's data. Git
                // detects exactly that shape — an unclean merge whose result IS
                // ours' input — and hands the second destination theirs'
                // original object instead. Git's own regression
                // `t/t6422-merge-rename-corner-cases.sh:1423-1438` requires the
                // two destinations to equal the two sides' originals
                // (Codex R1 P1-2).
                let theirs_content = if !clean && merged == ours_entry {
                    theirs_entry
                } else {
                    merged
                };
                // Git copies ONE merge result into both sides' stages and
                // leaves the base under the OLD name — `merge-ort.c:3036-3068`
                // documents keeping the source at stage 1 as deliberate.
                ours.insert(ours_path.clone(), merged);
                theirs.insert(theirs_path.clone(), theirs_content);
                forced.push((
                    ours_path.clone(),
                    match theirs.get(&ours_path) {
                        Some(added) if context.depth == 0 => rename_destination_conflict(
                            &ours_path,
                            &merged,
                            added,
                            branches.1,
                            conflict_style,
                            context,
                        )?,
                        // The fold discards forced conflicts and resolves the
                        // remapped stages with virtual_conflict_resolution.
                        Some(_) | None => ConflictKind::RenameMerged {
                            content: merged,
                            kind: RenameConflictKind::RenameRename,
                        },
                    },
                ));
                forced.push((
                    theirs_path.clone(),
                    match ours.get(&theirs_path) {
                        Some(added) if context.depth == 0 => rename_destination_conflict(
                            &theirs_path,
                            added,
                            &theirs_content,
                            branches.1,
                            conflict_style,
                            context,
                        )?,
                        Some(_) | None => ConflictKind::RenameMerged {
                            content: theirs_content,
                            kind: RenameConflictKind::RenameRename,
                        },
                    },
                ));
                // INTENTIONAL DEVIATION from Git: the source is resolved by
                // REMOVAL rather than left unmerged at stage 1.
                //
                // Git's own comment at `merge-ort.c:3057-3068` calls keeping it
                // legacy — "For renames we normally remove the path at the old
                // name. It would thus seem consistent to do the same for
                // rename/rename(1to2) cases, but we haven't done so
                // traditionally and a number of the regression tests now encode
                // an expectation that the file is left there at stage 1" — and
                // spells out the two lines that would change it.
                //
                // Libra takes the consistent branch. The primary reason is
                // Git's own verdict above; the secondary one is that keeping
                // the stage would put the conflict out of reach of the ordinary
                // staging flow. The source has no working-tree file, and
                // `libra add` / `libra rm` / `libra restore --staged` all match
                // paths through the staged entry, so none of them can address
                // it (`LBR-CLI-003: pathspec did not match any files`, or
                // `path is unmerged`); `add -A` silently no-ops on it.
                //
                // It is NOT unresolvable, and an earlier version of this note
                // wrongly said so (Codex R1 P1-4): `libra read-tree HEAD`
                // followed by `add -A .` does clear it, as does
                // `update-index --cacheinfo`. Both are blunt — `read-tree`
                // replaces the whole index and so discards every resolution
                // staged so far, and `update-index` is plumbing — so the shape
                // would still be a trap for anyone following the documented
                // `add <path>` + `merge --continue` workflow.
                //
                // Both destinations carry the merged result, which already
                // incorporates the base, so nothing is lost by dropping it.
                base.remove(&decision.old);
                notes.push(RenameConflictNote::RenameRename {
                    old: decision.old.clone(),
                    ours_path,
                    theirs_path,
                });
            }
            Some(reason @ (RenameDeclined::SourceDeleted | RenameDeclined::SourceTypeChanged)) => {
                let type_changed = *reason == RenameDeclined::SourceTypeChanged;
                let other: &HashMap<PathBuf, MergeTreeEntry> = match decision.side {
                    MergeSide::Ours => &*theirs,
                    MergeSide::Theirs => &*ours,
                };
                // rename/add/delete: the destination is ALSO taken by the other
                // side. Git reports rename/delete but leaves the destination
                // "as-is so they look like an add/add conflict"
                // (`merge-ort.c:3180-3188`) — so the base is NOT carried over.
                let destination_taken = other.contains_key(&decision.new);
                let Some(base_entry) = base.remove(&decision.old) else {
                    continue;
                };
                let side: &HashMap<PathBuf, MergeTreeEntry> = match decision.side {
                    MergeSide::Ours => &*ours,
                    MergeSide::Theirs => &*theirs,
                };
                let Some(entry) = side.get(&decision.new).copied() else {
                    continue;
                };
                // A type-changed source SURVIVES under the old name (Git keeps
                // the two side stages and only clears the base's,
                // `merge-ort.c:3209-3211`); a deleted one is already gone from
                // both sides.
                if !type_changed {
                    ours.remove(&decision.old);
                    theirs.remove(&decision.old);
                }
                // Git copies the base to the NEW path's stage 1 whenever it
                // reaches the rename branch (`merge-ort.c:3202-3204`, before
                // the `type_changed` / `source_deleted` split). A COLLISION is
                // what suppresses that — and for a type change Git clears the
                // collision first (`:3090-3120`), so the base still travels.
                // Only rename/add/delete (`collision && source_deleted`) leaves
                // the destination base-less, "so they look like an add/add
                // conflict" (`:3180-3188`). Codex R1 P1-3.
                if type_changed || !destination_taken {
                    base.insert(decision.new.clone(), base_entry);
                }
                if !destination_taken {
                    // A PURE rename plus a delete is still a conflict for Git
                    // even though the content never changed (measured: stages
                    // 1 and 2 hold the same blob), so it is forced here — the
                    // ordinary match would call it a clean delete.
                    forced.push((
                        decision.new.clone(),
                        match decision.side {
                            MergeSide::Ours => {
                                ConflictKind::OursModifiedTheirsDeleted { ours: entry.hash }
                            }
                            MergeSide::Theirs => {
                                ConflictKind::TheirsModifiedOursDeleted { theirs: entry.hash }
                            }
                        },
                    ));
                } else if !type_changed && context.depth == 0 {
                    let (Some(ours_entry), Some(theirs_entry)) =
                        (ours.get(&decision.new), theirs.get(&decision.new))
                    else {
                        continue;
                    };
                    // Git keeps path_conflict after the add/add content merge,
                    // including when -X settles every hunk (merge-ort.c:3190).
                    forced.push((
                        decision.new.clone(),
                        rename_destination_conflict(
                            &decision.new,
                            ours_entry,
                            theirs_entry,
                            branches.1,
                            conflict_style,
                            context,
                        )?,
                    ));
                }
                // Git prints NOTHING extra for a type change: the destination's
                // own modify/delete line is the whole report.
                if !type_changed {
                    notes.push(RenameConflictNote::RenameDelete {
                        old: decision.old.clone(),
                        new: decision.new.clone(),
                        rename_side: decision.side,
                    });
                }
            }
            Some(RenameDeclined::DestinationCollision) => {
                let Some(base_entry) = base.get(&decision.old).copied() else {
                    continue;
                };
                let (side_entry, other_entry) = match decision.side {
                    MergeSide::Ours => (
                        ours.get(&decision.new).copied(),
                        theirs.get(&decision.old).copied(),
                    ),
                    MergeSide::Theirs => (
                        theirs.get(&decision.new).copied(),
                        ours.get(&decision.old).copied(),
                    ),
                };
                let (Some(side_entry), Some(other_entry)) = (side_entry, other_entry) else {
                    continue;
                };
                let (ours_entry, theirs_entry) = match decision.side {
                    MergeSide::Ours => (side_entry, other_entry),
                    MergeSide::Theirs => (other_entry, side_entry),
                };
                let (merged, clean) = merge_rename_content(
                    &decision.new,
                    Some(&base_entry),
                    &ours_entry,
                    &theirs_entry,
                    // Codex R1 P2-1: Git's collision branch sets
                    // `pathnames[other_source_index] = oldpath` and
                    // `pathnames[target_index] = newpath`
                    // (`merge-ort.c:3144-3146`) — only the side that DID the
                    // rename is labelled with the destination.
                    &label(
                        MergeSide::Ours,
                        match decision.side {
                            MergeSide::Ours => &decision.new,
                            MergeSide::Theirs => &decision.old,
                        },
                    ),
                    &label(
                        MergeSide::Theirs,
                        match decision.side {
                            MergeSide::Theirs => &decision.new,
                            MergeSide::Ours => &decision.old,
                        },
                    ),
                    &format!("base:{}", decision.old.display()),
                    conflict_style,
                    context,
                )?;
                // Git stores the rename's own merge at the renaming side's
                // stage of the destination, leaves the other side's entry at
                // its stage, records NO base there, and resolves the source by
                // removal (`merge-ort.c:3137-3179`) — so the destination comes
                // out as the add/add that was measured, for rename/add and
                // rename/rename(2to1) alike.
                match decision.side {
                    MergeSide::Ours => ours.insert(decision.new.clone(), merged),
                    MergeSide::Theirs => theirs.insert(decision.new.clone(), merged),
                };
                base.remove(&decision.old);
                ours.remove(&decision.old);
                theirs.remove(&decision.old);
                if !clean {
                    notes.push(RenameConflictNote::Collision {
                        old: decision.old.clone(),
                        new: decision.new.clone(),
                    });
                }
            }
            Some(RenameDeclined::DestinationBlocked) => {}
        }
    }
    Ok(notes)
}

/// What the rename pass decided, held until the merge is known to proceed.
///
/// The notices are NOT printed where the decision is made: the writer's
/// preflight (untracked collisions, symlink traversal) can still refuse the
/// whole merge, and Git prints no rename decision when it refuses. This mirrors
/// what MG-04 already does with its file/directory lines (Codex R12 P2).
struct RenameOutcome {
    decisions: Vec<RenameDecision>,
    limited: Vec<MergeSide>,
    /// MG-06: the path-level conflicts the renames produced, in Git's words.
    notes: Vec<RenameConflictNote>,
    /// MG-06: conflicts the ordinary three-way match must NOT decide for
    /// itself — a rename/rename(1to2)'s two destinations and a rename/delete
    /// whose content never changed (which the plain match would call a clean
    /// delete).
    forced: Vec<(PathBuf, ConflictKind)>,
}

fn announce_rename_notices(
    notes: &[RenameConflictNote],
    limited_sides: &[MergeSide],
    upstream: &str,
    output: &OutputConfig,
) {
    if output.is_json() {
        return;
    }
    for side in limited_sides {
        info_println!(
            output,
            "notice: skipped inexact rename detection for {} because more than the merge.renameLimit paths changed; exact renames were still detected",
            df_branch_label(*side, upstream)
        );
    }
    // MG-06: Git's own wording, verbatim. Each line is a CONFLICT, not a
    // notice: before this card these shapes degraded to "merging without
    // rename detection", which lost the merge base and with it the conflict.
    for note in notes {
        match note {
            RenameConflictNote::RenameRename {
                old,
                ours_path,
                theirs_path,
            } => info_println!(
                output,
                "CONFLICT (rename/rename): {} renamed to {} in {} and to {} in {}.",
                old.display(),
                ours_path.display(),
                df_branch_label(MergeSide::Ours, upstream),
                theirs_path.display(),
                df_branch_label(MergeSide::Theirs, upstream)
            ),
            RenameConflictNote::RenameDelete {
                old,
                new,
                rename_side,
            } => info_println!(
                output,
                "CONFLICT (rename/delete): {} renamed to {} in {}, but deleted in {}.",
                old.display(),
                new.display(),
                df_branch_label(*rename_side, upstream),
                df_branch_label(
                    match rename_side {
                        MergeSide::Ours => MergeSide::Theirs,
                        MergeSide::Theirs => MergeSide::Ours,
                    },
                    upstream
                )
            ),
            RenameConflictNote::Collision { old, new } => info_println!(
                output,
                "CONFLICT (rename involved in collision): rename of {} -> {} has content conflicts AND collides with another path; this may result in nested conflict markers.",
                old.display(),
                new.display()
            ),
            RenameConflictNote::DirectoryMove {
                old,
                new,
                added_side,
                rename_side,
                conflict: false,
            } => info_println!(
                output,
                "Path updated: {} added in {} inside a directory that was renamed in {}; moving it to {}.",
                old.display(),
                df_branch_label(*added_side, upstream),
                df_branch_label(*rename_side, upstream),
                new.display()
            ),
            RenameConflictNote::DirectoryMove {
                old,
                new,
                added_side,
                rename_side,
                conflict: true,
            } => info_println!(
                output,
                "CONFLICT (file location): {} added in {} inside a directory that was renamed in {}, suggesting it should perhaps be moved to {}.",
                old.display(),
                df_branch_label(*added_side, upstream),
                df_branch_label(*rename_side, upstream),
                new.display()
            ),
            RenameConflictNote::DirectorySplit { old } => info_println!(
                output,
                "CONFLICT (directory rename split): Unclear where to rename {} to; it was renamed to multiple other directories, with no destination getting a majority of the files.",
                old.display()
            ),
        }
    }
}

/// Detect renames on both sides and rewrite the maps in place. Returns the
/// decisions and deferred notices so the caller can announce them only after
/// the merge's write preflight succeeds (or while producing a dry-run).
#[allow(clippy::too_many_arguments)]
fn detect_and_apply_renames(
    base: &mut HashMap<PathBuf, MergeTreeEntry>,
    ours: &mut HashMap<PathBuf, MergeTreeEntry>,
    theirs: &mut HashMap<PathBuf, MergeTreeEntry>,
    config: &MergeRenameConfig,
    conflict_style: diffy::ConflictStyle,
    branches: (&str, &str),
    context: &mut TreeMergeContext<'_>,
) -> Result<RenameOutcome, PullMergeError> {
    if !config.enabled {
        return Ok(RenameOutcome {
            decisions: Vec::new(),
            limited: Vec::new(),
            notes: Vec::new(),
            forced: Vec::new(),
        });
    }
    // ONE reader for both sides: a blob shared by the two detections (the
    // base's, above all) is then read once.
    let mut reader = MergeRenameReader::new();
    let (mut our_side, mut their_side) = {
        let virtual_blobs = &*context.virtual_blobs;
        (
            detect_side_renames(base, ours, config, virtual_blobs, &mut reader),
            detect_side_renames(base, theirs, config, virtual_blobs, &mut reader),
        )
    };
    if our_side.matches.is_empty()
        && their_side.matches.is_empty()
        && !our_side.skipped_by_limit
        && !their_side.skipped_by_limit
    {
        return Ok(RenameOutcome {
            decisions: Vec::new(),
            limited: Vec::new(),
            notes: Vec::new(),
            forced: Vec::new(),
        });
    }
    let mut forced = Vec::new();
    let mut notes = apply_directory_renames(
        base,
        ours,
        theirs,
        &mut our_side.matches,
        &mut their_side.matches,
        config,
        conflict_style,
        branches,
        context,
        &mut forced,
    )?;
    let decisions = decide_renames(base, ours, theirs, &our_side.matches, &their_side.matches);
    notes.extend(apply_renames(
        base,
        ours,
        theirs,
        &decisions,
        &mut forced,
        conflict_style,
        branches,
        context,
    )?);
    let mut limited = Vec::new();
    if our_side.skipped_by_limit {
        limited.push(MergeSide::Ours);
    }
    if their_side.skipped_by_limit {
        limited.push(MergeSide::Theirs);
    }
    Ok(RenameOutcome {
        decisions,
        limited,
        notes,
        forced,
    })
}

/// The incremental walk keeps its pruning contract for ordinary merges. When
/// its already-collected rename candidates can imply a directory rename, the
/// caller reuses the flattening engine: directory relocation changes paths
/// before the ordinary three-way resolution, and one implementation remains
/// the source of truth for the resulting index stages and conflicts.
fn incremental_may_need_flat_directory_renames(
    sources: &[Vec<(PathBuf, MergeTreeEntry, Option<MergeTreeEntry>)>; 2],
    dests: &[Vec<(PathBuf, MergeTreeEntry)>; 2],
    config: &MergeRenameConfig,
    virtual_blobs: &VirtualBlobs,
) -> bool {
    if !config.enabled || config.directory_renames == DirectoryRenameMode::False {
        return false;
    }
    let mut reader = MergeRenameReader::new();
    for (index, side) in [MergeSide::Ours, MergeSide::Theirs].into_iter().enumerate() {
        if sources[index].is_empty() || dests[index].is_empty() {
            continue;
        }
        let base_map: HashMap<PathBuf, MergeTreeEntry> = sources[index]
            .iter()
            .map(|(path, base, _)| (path.clone(), *base))
            .collect();
        let side_map: HashMap<PathBuf, MergeTreeEntry> = dests[index].iter().cloned().collect();
        let detected =
            detect_side_renames(&base_map, &side_map, config, virtual_blobs, &mut reader);
        let plan = infer_provisional_directory_renames(&detected.matches, side);
        if !plan.renames.is_empty() || !plan.splits.is_empty() {
            return true;
        }
    }
    false
}

/// MG-05 for the pruned walk: the walk resolved the rename's source and
/// destination as unrelated paths, so once detection pairs them the two
/// outcomes are replaced by ONE three-way resolution at the new path — the
/// same triple the flattening engine forms before it resolves anything.
#[allow(clippy::too_many_arguments)]
fn apply_incremental_renames(
    source: &mut dyn TreeSource,
    sources: &[Vec<(PathBuf, MergeTreeEntry, Option<MergeTreeEntry>)>; 2],
    dests: &[Vec<(PathBuf, MergeTreeEntry)>; 2],
    config: &MergeRenameConfig,
    merged: &mut HashMap<PathBuf, MergeTreeEntry>,
    conflicts: &mut Vec<(PathBuf, ConflictKind)>,
    files_changed: &mut usize,
    context: &mut TreeMergeContext<'_>,
    // MG-06: the style the rename-driven content merges are rendered with, and
    // the label the other side's paths carry in their conflict markers.
    conflict_style: diffy::ConflictStyle,
    upstream: &str,
    // The merge base's root, for MG-04's base-presence rule: a destination
    // directory holding nothing but empty trees is in the way only when the
    // base had NOTHING there (Codex R15). Probed per destination, and only for
    // destinations no FILE already occupies, so a merge without empty markers
    // reads nothing extra.
    base_tree: Option<ObjectHash>,
) -> Result<RenameOutcome, PullMergeError> {
    if !config.enabled {
        return Ok(RenameOutcome {
            decisions: Vec::new(),
            limited: Vec::new(),
            notes: Vec::new(),
            forced: Vec::new(),
        });
    }
    let mut decisions = Vec::new();
    let mut limited = Vec::new();
    let mut per_side: [Vec<rename_detect::RenameMatch>; 2] = [Vec::new(), Vec::new()];
    // Indexed once: the loops below look each candidate up by path, and the
    // lists grow with the change set.
    let source_index: [HashMap<&PathBuf, (MergeTreeEntry, Option<MergeTreeEntry>)>; 2] = [0, 1]
        .map(|index| {
            sources[index]
                .iter()
                .map(|(path, base, other)| (path, (*base, *other)))
                .collect()
        });
    let dest_index: [HashMap<&PathBuf, MergeTreeEntry>; 2] = [0, 1].map(|index| {
        dests[index]
            .iter()
            .map(|(path, entry)| (path, *entry))
            .collect()
    });
    // The same occupancy index the flattening engine builds, from the input
    // facts plus the result's own FILE paths (a carried subtree is checked
    // separately, below): one hash probe per rename instead of a scan.
    let mut reader = MergeRenameReader::new();
    for (index, side) in [MergeSide::Ours, MergeSide::Theirs].into_iter().enumerate() {
        if sources[index].is_empty() || dests[index].is_empty() {
            continue;
        }
        let base_map: HashMap<PathBuf, MergeTreeEntry> = sources[index]
            .iter()
            .map(|(path, base, _)| (path.clone(), *base))
            .collect();
        let side_map: HashMap<PathBuf, MergeTreeEntry> = dests[index].iter().cloned().collect();
        let detected = detect_side_renames(
            &base_map,
            &side_map,
            config,
            context.virtual_blobs,
            &mut reader,
        );
        if detected.skipped_by_limit {
            limited.push(side);
        }
        per_side[index] = detected.matches;
    }
    let other_at = |index: usize, path: &PathBuf| -> Option<MergeTreeEntry> {
        source_index[index].get(path).and_then(|(_, other)| *other)
    };
    let base_at = |index: usize, path: &PathBuf| -> Option<MergeTreeEntry> {
        source_index[index].get(path).map(|(base, _)| *base)
    };
    let dest_at = |index: usize, path: &PathBuf| -> Option<MergeTreeEntry> {
        dest_index[index].get(path).copied()
    };
    // The same rules the flattening engine applies, expressed over the
    // candidate lists: the other side must still HAVE the source, must not
    // have renamed it elsewhere, and must not occupy the destination.
    let mut ours_by_old: HashMap<&PathBuf, &PathBuf> = HashMap::new();
    for pair in &per_side[0] {
        ours_by_old.insert(&pair.old, &pair.new);
    }
    let mut theirs_by_old: HashMap<&PathBuf, &PathBuf> = HashMap::new();
    for pair in &per_side[1] {
        theirs_by_old.insert(&pair.old, &pair.new);
    }
    // What the OTHER side ADDED, as an input fact — independent of how the walk
    // resolved it. The result alone is not enough (Codex R17): when the other
    // side independently adds a file at the rename's destination and the merge
    // resolves that path in its favour, the result holds ONE entry there and
    // the walk cannot tell it apart from the rename's own product, so the
    // rename overwrote the other side's file and exited 0.
    let other_adds: [HashSet<PathBuf>; 2] =
        [0, 1].map(|index| occupied_names(dest_index[index].keys().copied()));
    // The same counted occupancy the flattening engine uses, built from what
    // the walk actually produced: every merged entry and every conflicted path.
    // A conflicted path occupies its name too (Codex R5 P1) — the flattening
    // engine sees it in the side maps, the walk only in this list.
    let mut occupancy = DestinationOccupancy::default();
    for (path, entry) in merged.iter() {
        // Ancestors only: the walk resolved the destination as a plain add, and
        // that entry IS the rename's product.
        occupancy.apply(path, entry.mode == TreeItemMode::Tree, false, 1);
    }
    for (path, _) in conflicts.iter() {
        occupancy.apply(path, false, true, 1);
    }
    let occupies_marker_only = |path: &Path, occupancy: &DestinationOccupancy| {
        !occupancy.files.get(path).is_some_and(|count| *count > 0)
            && occupancy.markers.get(path).is_some_and(|count| *count > 0)
    };
    let conflicted_paths: HashSet<&PathBuf> = conflicts.iter().map(|(path, _)| path).collect();
    // `(marker, at_own_path, already_counted)` — the facts needed to release
    // and, when a rename is blocked, restore the other side's source. A D/F
    // candidate is deliberately not yet in `merged` or `conflicts`; the input
    // source tuple is still authoritative there and must be indexed before it
    // can participate in the optimistic release.
    let held_at = |index: usize, path: &PathBuf| -> Option<(bool, bool, bool)> {
        merged
            .get(path)
            .map(|entry| (entry.mode == TreeItemMode::Tree, false, true))
            .or_else(|| {
                conflicted_paths
                    .contains(path)
                    .then_some((false, true, true))
            })
            .or_else(|| {
                other_at(index, path).map(|entry| (entry.mode == TreeItemMode::Tree, true, false))
            })
    };
    // PASS 1 — the declines that do not depend on occupancy.
    let pairs: Vec<(usize, &rename_detect::RenameMatch)> = per_side[0]
        .iter()
        .map(|pair| (0usize, pair))
        .chain(per_side[1].iter().map(|pair| (1usize, pair)))
        .collect();
    let mut declined: Vec<Option<RenameDeclined>> = Vec::with_capacity(pairs.len());
    for (index, pair) in &pairs {
        let other_renamed = if *index == 0 {
            theirs_by_old.get(&pair.old).copied()
        } else {
            ours_by_old.get(&pair.old).copied()
        };
        declined.push(match other_renamed {
            Some(destination) if *destination == pair.new => Some(RenameDeclined::SameDestination),
            Some(destination) => Some(RenameDeclined::DivergentRenames {
                theirs: destination.clone(),
            }),
            // The other side must still hold the source, and hold it as the
            // SAME kind of entry the rename moved: a type change is a delete
            // for rename purposes, as Git treats it.
            None if !other_at(*index, &pair.old).is_some_and(|entry| {
                dest_at(*index, &pair.new)
                    .is_some_and(|moved| same_entry_kind(entry.mode, moved.mode))
            }) =>
            {
                Some(if other_at(*index, &pair.old).is_some() {
                    RenameDeclined::SourceTypeChanged
                } else {
                    RenameDeclined::SourceDeleted
                })
            }
            None => None,
        });
    }

    // PASS 2 — occupancy, as the same fixed point the flattening engine runs:
    // release every eligible source, then re-take only the ones structurally
    // blocked at their destination and re-check what that could affect. An
    // exact file collision consumes its source in the MG-06 collision pass.
    for (slot, (index, pair)) in pairs.iter().enumerate() {
        if declined[slot].is_some() {
            continue;
        }
        if let Some((marker, at_own_path, already_counted)) = held_at(*index, &pair.old) {
            if !already_counted {
                occupancy.apply(&pair.old, marker, at_own_path, 1);
            }
            occupancy.apply(&pair.old, marker, at_own_path, -1);
        }
    }
    let destination_dependents = DestinationDependents::from_pairs(&pairs, &declined);
    let mut base_presence = BasePresence::default();
    let mut queue: Vec<usize> = (0..pairs.len())
        .filter(|slot| declined[*slot].is_none())
        .collect();
    while let Some(slot) = queue.pop() {
        if declined[slot].is_some() {
            continue;
        }
        let (index, pair) = pairs[slot];
        let taken = other_adds[1 - index].contains(&pair.new)
            || occupancy.file_occupied(&pair.new)
            || (occupies_marker_only(&pair.new, &occupancy)
                && !base_holds_anything_at(source, base_tree, &pair.new, &mut base_presence)?)
            // A subtree the walk carries whole IS a directory there — but only
            // if it holds a file (an empty one is not in the way; `git merge`
            // uses the rename then, verified).
            || carried_subtree_holds_a_file(source, merged, &pair.new)?;
        if !taken {
            continue;
        }
        let exact_collision =
            dest_at(1 - index, &pair.new).is_some_and(|entry| entry.mode != TreeItemMode::Tree);
        declined[slot] = Some(if exact_collision {
            RenameDeclined::DestinationCollision
        } else {
            RenameDeclined::DestinationBlocked
        });
        if exact_collision {
            continue;
        }
        if let Some((marker, at_own_path, _)) = held_at(index, &pair.old) {
            occupancy.apply(&pair.old, marker, at_own_path, 1);
            destination_dependents.requeue_blocked_by(&pair.old, &mut queue);
        }
    }
    for ((index, pair), declined) in pairs.into_iter().zip(declined) {
        decisions.push(RenameDecision {
            old: pair.old.clone(),
            new: pair.new.clone(),
            side: if index == 0 {
                MergeSide::Ours
            } else {
                MergeSide::Theirs
            },
            declined,
        });
    }
    // A rename's destination can sit INSIDE a subtree the walk carries whole
    // (`shared` as one `TreeItemMode::Tree` entry). Writing both that entry
    // and a `shared/moved.txt` leaf would put two entries under one name in
    // the result tree, so the covering subtrees are expanded into leaves
    // first — only those, and only when a rename actually lands there.
    let touched: Vec<PathBuf> = decisions
        .iter()
        .filter(|decision| decision.declined.is_none())
        .flat_map(|decision| [decision.old.clone(), decision.new.clone()])
        .collect();
    expand_subtrees_covering(source, merged, &touched)?;
    // One pass to collect the paths the accepted renames will take over, then
    // ONE prune of the conflict list. Conflicts the resolution below pushes are
    // added afterwards, so they survive; two accepted renames never share a
    // path (a shared destination is declined as `SameDestination` or
    // `DestinationCollision`), so the order within the pass does not matter.
    let mut renamed_paths: HashSet<PathBuf> = HashSet::new();
    for decision in &decisions {
        if decision.declined.is_some() {
            continue;
        }
        let index = match decision.side {
            MergeSide::Ours => 0,
            MergeSide::Theirs => 1,
        };
        if base_at(index, &decision.old).is_some()
            && other_at(index, &decision.old).is_some()
            && dest_at(index, &decision.new).is_some()
        {
            renamed_paths.insert(decision.old.clone());
            renamed_paths.insert(decision.new.clone());
        }
    }
    conflicts.retain(|(path, _)| !renamed_paths.contains(path));
    for decision in &decisions {
        if decision.declined.is_some() {
            continue;
        }
        let index = match decision.side {
            MergeSide::Ours => 0,
            MergeSide::Theirs => 1,
        };
        let (Some(base_entry), Some(other_entry), Some(side_entry)) = (
            base_at(index, &decision.old),
            other_at(index, &decision.old),
            dest_at(index, &decision.new),
        ) else {
            continue;
        };
        // Drop what the walk decided for the two paths on their own. The
        // conflict list is pruned ONCE, above, rather than per decision:
        // exact renames are deliberately not capped by `merge.renameLimit`, so
        // scanning the whole list per accepted rename would be quadratic in
        // the tree size on a merge that renames a lot.
        merged.remove(&decision.old);
        merged.remove(&decision.new);
        let (ours_entry, theirs_entry) = match decision.side {
            MergeSide::Ours => (side_entry, other_entry),
            MergeSide::Theirs => (other_entry, side_entry),
        };
        let resolved = match resolve_three_way(
            &decision.new,
            Some(&base_entry),
            Some(&ours_entry),
            Some(&theirs_entry),
            context,
        )? {
            MergeResolution::Use(entry) => {
                merged.insert(decision.new.clone(), entry);
                Some(entry)
            }
            MergeResolution::Delete => None,
            MergeResolution::Conflict(kind) => {
                conflicts.push((decision.new.clone(), kind));
                None
            }
        };
        // `files_changed` must equal what the flattening engine counts over its
        // REMAPPED maps (`count_item_map_changes(ours, merged)`), so correct
        // what the walk counted for the two paths on their own:
        //   * after the remap ours holds the file at the NEW path (its own copy
        //     when ours renamed, the moved one when theirs did), and the source
        //     path is gone from both sides — no change there;
        //   * so the only change is "the result at the new path differs from
        //     ours' entry there".
        // The walk, seeing the paths separately, counted nothing for an ours
        // rename (it kept ours' file at the new path and ours never had the
        // source) and two for a theirs rename (source deleted, destination
        // added).
        // A theirs rename adds one ONLY when Git's diffstat would show a line
        // the remapped comparison does not: either the content is unchanged
        // (the comparison sees nothing, Git sees one `old => new`), or the pair
        // no longer reads as a rename at all (the comparison sees one, Git sees
        // a delete AND an add). A rename that changed content and still reads
        // as one is already counted by the first term (Codex R19, then R20).
        let ours_after_remap = Some(ours_entry);
        let changed_at_new_path = resolved != ours_after_remap;
        let still_a_rename = !changed_at_new_path
            || resolved.is_some_and(|entry| {
                pair_reads_as_a_rename(
                    &ours_entry,
                    &entry,
                    config,
                    context.virtual_blobs,
                    &mut reader,
                )
            });
        let truth = usize::from(changed_at_new_path)
            + usize::from(
                decision.side == MergeSide::Theirs && !(changed_at_new_path && still_a_rename),
            );
        let counted_by_walk = match decision.side {
            MergeSide::Ours => 0,
            MergeSide::Theirs => 2,
        };
        *files_changed = (*files_changed + truth).saturating_sub(counted_by_walk);
    }
    // MG-06: the declines the walk left as "no rename" become Git's PATH-LEVEL
    // conflicts here, over the walk's own output. The flattening engine does
    // the same surgery on its maps before it resolves anything
    // ([`apply_renames`]); both engines must land on the same result, which is
    // what the double-walk cases assert.
    let mut notes = Vec::new();
    let mut folded: HashSet<PathBuf> = HashSet::new();
    // A 2to1 collision updates each destination stage separately. Keep the
    // first source's merged content when processing the second source.
    let mut collision_inputs: HashMap<PathBuf, [Option<MergeTreeEntry>; 2]> = HashMap::new();
    // Whatever the walk decided for a path the rename pass takes over is
    // replaced wholesale: its merged entry and any conflict it recorded go,
    // and the rename's own verdict (if any) takes their place.
    let settle = |path: &PathBuf,
                  kind: Option<ConflictKind>,
                  merged: &mut HashMap<PathBuf, MergeTreeEntry>,
                  conflicts: &mut Vec<(PathBuf, ConflictKind)>| {
        merged.remove(path);
        conflicts.retain(|(other, _)| other != path);
        if let Some(kind) = kind {
            conflicts.push((path.clone(), kind));
        }
    };
    for decision in &decisions {
        let index = match decision.side {
            MergeSide::Ours => 0,
            MergeSide::Theirs => 1,
        };
        match &decision.declined {
            None => {}
            Some(RenameDeclined::SameDestination) => {
                // rename/rename(1to1): Git carries the base to the shared
                // destination and merges normally there
                // (`merge-ort.c:2991-3018`), so the add/add the walk saw
                // becomes a three-way merge that can come out CLEAN.
                let (Some(base_entry), Some(ours_entry), Some(theirs_entry)) = (
                    base_at(index, &decision.old),
                    dest_at(0, &decision.new),
                    dest_at(1, &decision.new),
                ) else {
                    continue;
                };
                if !folded.insert(decision.new.clone()) {
                    continue;
                }
                let resolved = resolve_three_way(
                    &decision.new,
                    Some(&base_entry),
                    Some(&ours_entry),
                    Some(&theirs_entry),
                    context,
                )?;
                let before_counted = merged
                    .get(&decision.new)
                    .is_none_or(|entry| *entry != ours_entry)
                    || conflicts.iter().any(|(path, _)| *path == decision.new);
                settle(
                    &decision.new,
                    match resolved {
                        MergeResolution::Conflict(kind) => Some(kind),
                        _ => None,
                    },
                    merged,
                    conflicts,
                );
                let after = match resolved {
                    MergeResolution::Use(entry) => {
                        merged.insert(decision.new.clone(), entry);
                        Some(entry)
                    }
                    MergeResolution::Delete => None,
                    MergeResolution::Conflict(_) => None,
                };
                let after_counted = after != Some(ours_entry);
                *files_changed = (*files_changed + usize::from(after_counted))
                    .saturating_sub(usize::from(before_counted));
                // The old path is gone from both sides already; the walk
                // resolved it as a clean delete, which is what Git does.
            }
            Some(RenameDeclined::DivergentRenames { theirs: other_path }) => {
                if !folded.insert(decision.old.clone()) {
                    continue;
                }
                let (ours_path, theirs_path) = match decision.side {
                    MergeSide::Ours => (decision.new.clone(), other_path.clone()),
                    MergeSide::Theirs => (other_path.clone(), decision.new.clone()),
                };
                let (Some(base_entry), Some(ours_entry), Some(theirs_entry)) = (
                    base_at(index, &decision.old),
                    dest_at(0, &ours_path),
                    dest_at(1, &theirs_path),
                ) else {
                    continue;
                };
                let (merged_entry, merged_clean) = merge_rename_content(
                    &ours_path,
                    Some(&base_entry),
                    &ours_entry,
                    &theirs_entry,
                    &format!(
                        "{}:{}",
                        df_branch_label(MergeSide::Ours, upstream),
                        ours_path.display()
                    ),
                    &format!(
                        "{}:{}",
                        df_branch_label(MergeSide::Theirs, upstream),
                        theirs_path.display()
                    ),
                    &format!("base:{}", decision.old.display()),
                    conflict_style,
                    context,
                )?;
                // Git's `was_binary_blob` fallback (`merge-ort.c:3032-3053`):
                // when the content merge could not actually be performed it
                // just TOOK one whole side, so copying that side's blob to both
                // destinations would overwrite the other side's data. Git
                // detects exactly that shape — an unclean merge whose result IS
                // ours' input — and hands the second destination theirs'
                // original object instead. Git's own regression
                // `t/t6422-merge-rename-corner-cases.sh:1423-1438` requires the
                // two destinations to equal the two sides' originals
                // (Codex R1 P1-2).
                let theirs_content = if !merged_clean && merged_entry == ours_entry {
                    theirs_entry
                } else {
                    merged_entry
                };
                for (path, content, rename_side) in [
                    (&ours_path, merged_entry, 0),
                    (&theirs_path, theirs_content, 1),
                ] {
                    let original_ours = dest_at(0, path);
                    let before_counted = merged.get(path).copied() != original_ours;
                    let kind = match dest_at(1 - rename_side, path) {
                        Some(added) => {
                            let (ours_side, theirs_side) = if rename_side == 0 {
                                (content, added)
                            } else {
                                (added, content)
                            };
                            rename_destination_conflict(
                                path,
                                &ours_side,
                                &theirs_side,
                                upstream,
                                conflict_style,
                                context,
                            )?
                        }
                        None => ConflictKind::RenameMerged {
                            content,
                            kind: RenameConflictKind::RenameRename,
                        },
                    };
                    settle(path, Some(kind), merged, conflicts);
                    let ours_after_remap = if rename_side == 0 {
                        Some(content)
                    } else {
                        original_ours
                    };
                    *files_changed = (*files_changed + usize::from(ours_after_remap.is_some()))
                        .saturating_sub(usize::from(before_counted));
                }
                // The source is resolved by removal, not left unmerged — see
                // the deviation note in `apply_renames`.
                settle(&decision.old, None, merged, conflicts);
                notes.push(RenameConflictNote::RenameRename {
                    old: decision.old.clone(),
                    ours_path,
                    theirs_path,
                });
            }
            Some(reason @ (RenameDeclined::SourceDeleted | RenameDeclined::SourceTypeChanged)) => {
                let type_changed = *reason == RenameDeclined::SourceTypeChanged;
                let Some(side_entry) = dest_at(index, &decision.new) else {
                    continue;
                };
                // rename/add/delete: the destination is ALSO taken, and Git
                // leaves it looking like an add/add (`merge-ort.c:3180-3188`).
                let destination_taken = dest_at(1 - index, &decision.new).is_some();
                if !destination_taken {
                    settle(
                        &decision.new,
                        Some(match decision.side {
                            MergeSide::Ours => ConflictKind::OursModifiedTheirsDeleted {
                                ours: side_entry.hash,
                            },
                            MergeSide::Theirs => ConflictKind::TheirsModifiedOursDeleted {
                                theirs: side_entry.hash,
                            },
                        }),
                        merged,
                        conflicts,
                    );
                    // Codex R1 P1-6. The walk decided the destination on its
                    // own: when OURS renamed, it saw ours' own entry there and
                    // counted nothing; when THEIRS renamed, it saw an add that
                    // differs from ours' absence and counted one. Forcing the
                    // conflict takes the path out of the result, so the truth —
                    // what `count_item_map_changes(ours_after_remap, merged)`
                    // sees — is one when ours holds a file there after the
                    // remap (i.e. ours did the rename) and zero otherwise.
                    let truth = usize::from(decision.side == MergeSide::Ours);
                    let counted_by_walk = usize::from(decision.side == MergeSide::Theirs);
                    *files_changed = (*files_changed + truth).saturating_sub(counted_by_walk);
                } else if !type_changed {
                    let (Some(ours_entry), Some(theirs_entry)) =
                        (dest_at(0, &decision.new), dest_at(1, &decision.new))
                    else {
                        continue;
                    };
                    let before_counted = merged.get(&decision.new).copied() != Some(ours_entry);
                    settle(
                        &decision.new,
                        Some(rename_destination_conflict(
                            &decision.new,
                            &ours_entry,
                            &theirs_entry,
                            upstream,
                            conflict_style,
                            context,
                        )?),
                        merged,
                        conflicts,
                    );
                    *files_changed =
                        (*files_changed + 1).saturating_sub(usize::from(before_counted));
                }
                if type_changed && destination_taken {
                    // The base travels to the destination even though something
                    // occupies it (Codex R1 P1-3), so the walk's base-less
                    // add/add verdict has to be replaced by the three-way the
                    // flattening engine forms there.
                    if let (Some(base_entry), Some(other_entry)) = (
                        base_at(index, &decision.old),
                        dest_at(1 - index, &decision.new),
                    ) {
                        let (ours_side, theirs_side) = match decision.side {
                            MergeSide::Ours => (side_entry, other_entry),
                            MergeSide::Theirs => (other_entry, side_entry),
                        };
                        let resolved = resolve_three_way(
                            &decision.new,
                            Some(&base_entry),
                            Some(&ours_side),
                            Some(&theirs_side),
                            context,
                        )?;
                        settle(
                            &decision.new,
                            match resolved {
                                MergeResolution::Conflict(kind) => Some(kind),
                                _ => None,
                            },
                            merged,
                            conflicts,
                        );
                        if let MergeResolution::Use(entry) = resolved {
                            merged.insert(decision.new.clone(), entry);
                        }
                    }
                }
                if type_changed {
                    // Git clears only the BASE bit at the source
                    // (`oldinfo->filemask &= 0x06`, `merge-ort.c:3211`), which
                    // leaves the type-changed entry as a plain one-sided add —
                    // the walk saw base + one side and called it modify/delete,
                    // so its verdict has to be replaced. Measured on git 2.50.1
                    // (`/Volumes/Data/tmp/mg06-git/ktype`): the result tree
                    // holds `120000 old` beside the conflicted `new`.
                    if let Some(surviving) = other_at(index, &decision.old) {
                        settle(&decision.old, None, merged, conflicts);
                        merged.insert(decision.old.clone(), surviving);
                    }
                } else {
                    settle(&decision.old, None, merged, conflicts);
                    notes.push(RenameConflictNote::RenameDelete {
                        old: decision.old.clone(),
                        new: decision.new.clone(),
                        rename_side: decision.side,
                    });
                }
            }
            Some(RenameDeclined::DestinationCollision) => {
                let (Some(base_entry), Some(side_entry), Some(other_entry)) = (
                    base_at(index, &decision.old),
                    dest_at(index, &decision.new),
                    other_at(index, &decision.old),
                ) else {
                    continue;
                };
                let (ours_entry, theirs_entry) = match decision.side {
                    MergeSide::Ours => (side_entry, other_entry),
                    MergeSide::Theirs => (other_entry, side_entry),
                };
                let (rename_merged, clean) = merge_rename_content(
                    &decision.new,
                    Some(&base_entry),
                    &ours_entry,
                    &theirs_entry,
                    // Codex R1 P2-1: only the renaming side carries the
                    // destination path; the other side keeps the source.
                    &format!(
                        "{}:{}",
                        df_branch_label(MergeSide::Ours, upstream),
                        match decision.side {
                            MergeSide::Ours => decision.new.display(),
                            MergeSide::Theirs => decision.old.display(),
                        }
                    ),
                    &format!(
                        "{}:{}",
                        df_branch_label(MergeSide::Theirs, upstream),
                        match decision.side {
                            MergeSide::Theirs => decision.new.display(),
                            MergeSide::Ours => decision.old.display(),
                        }
                    ),
                    &format!("base:{}", decision.old.display()),
                    conflict_style,
                    context,
                )?;
                // The destination becomes an add/add between the rename's own
                // merge and whatever the other side put there — no base stage,
                // exactly as measured for rename/add and rename/rename(2to1).
                let destination = collision_inputs
                    .entry(decision.new.clone())
                    .or_insert_with(|| [dest_at(0, &decision.new), dest_at(1, &decision.new)]);
                let counted_destination = merged.get(&decision.new).copied() != destination[0];
                let original_source = match decision.side {
                    MergeSide::Ours => None,
                    MergeSide::Theirs => Some(other_entry),
                };
                let counted_source = merged.get(&decision.old).copied() != original_source;
                destination[index] = Some(rename_merged);
                let [ours_side, theirs_side] = *destination;
                let resolved = resolve_three_way(
                    &decision.new,
                    None,
                    ours_side.as_ref(),
                    theirs_side.as_ref(),
                    context,
                )?;
                settle(
                    &decision.new,
                    match resolved {
                        MergeResolution::Conflict(kind) => Some(kind),
                        _ => None,
                    },
                    merged,
                    conflicts,
                );
                if let MergeResolution::Use(entry) = resolved {
                    merged.insert(decision.new.clone(), entry);
                }
                settle(&decision.old, None, merged, conflicts);
                // Compare against the same remapped ours that the flat path
                // uses. Consuming a source removes its earlier walk count;
                // a destination shared by two renames is counted only once.
                let changed_destination = merged.get(&decision.new).copied() != ours_side;
                *files_changed = (*files_changed + usize::from(changed_destination))
                    .saturating_sub(usize::from(counted_destination) + usize::from(counted_source));
                if !clean {
                    notes.push(RenameConflictNote::Collision {
                        old: decision.old.clone(),
                        new: decision.new.clone(),
                    });
                }
            }
            Some(RenameDeclined::DestinationBlocked) => {}
        }
    }
    Ok(RenameOutcome {
        decisions,
        limited,
        notes,
        forced: Vec::new(),
    })
}

/// A path that is a FILE on one side and a DIRECTORY on the other — recorded
/// by both engines while they still see the directory's entries, and settled
/// by [`resolve_df_conflicts`] once the result is complete.
#[derive(Debug, Clone)]
struct DfCandidate {
    path: PathBuf,
    /// The side holding the file (the other side holds the directory).
    file_side: MergeSide,
    /// That side's file entry.
    file: MergeTreeEntry,
    /// The merge base's FILE at this path, if it had one (never a directory).
    base_file: Option<MergeTreeEntry>,
    /// Whether the merge base had ANY entry at or under this path — a file, a
    /// directory, anything. Git's traversal defers a directory that is new
    /// relative to the base and adopts its tree verbatim, which makes even an
    /// empty-only subtree "in the way"; a directory the base already had is
    /// traversed, and one that merges to nothing is not in the way. See
    /// [`resolve_df_conflicts`].
    base_present: bool,
}

/// Whether a directory is still "in the way" of the file at the same path
/// (`merge-ort.c:4100-4198`: `ci->merged.result.mode != 0`), measured against
/// real `git merge` (git@3cb9185f6) on crafted trees:
///
/// * a surviving FILE beneath the path — the ordinary case — is in the way;
/// * a subtree that contributes no file is in the way only when the merge base
///   had NOTHING at that path: Git defers such a new directory and adopts its
///   tree verbatim (`collect_merge_info_callback`'s
///   `possible_trivial_merges`), so the directory survives even holding only
///   empty trees (verified: base ∅ + ours adds file `foo` + theirs adds
///   `foo/bar` = empty tree → `foo~HEAD`; with `foo` a file in the base and
///   edited by ours → plain `CONFLICT (modify/delete): foo`, no relocation);
/// * a directory-side entry that is itself an empty tree at the file's own
///   path is not "beneath" it and never in the way (verified: clean merge).
///
/// `subtree_has_file` reads a carried subtree (the incremental engine keeps
/// whole subtrees as `TreeItemMode::Tree` entries); the flattening engine's
/// only tree entries are empty-directory markers, so it answers `false`.
fn directory_is_in_the_way(
    path: &Path,
    entries: &[(PathBuf, MergeTreeEntry)],
    base_present: bool,
    subtree_has_file: &mut dyn FnMut(&ObjectHash) -> Result<bool, PullMergeError>,
) -> Result<bool, PullMergeError> {
    let start = entries.partition_point(|(candidate, _)| candidate.as_path() < path);
    let mut trees = Vec::new();
    for (entry_path, entry) in &entries[start..] {
        if entry_path == path {
            continue;
        }
        if !entry_path.starts_with(path) {
            break;
        }
        if entry.mode != TreeItemMode::Tree {
            return Ok(true);
        }
        trees.push(entry.hash);
    }
    if trees.is_empty() {
        return Ok(false);
    }
    if !base_present {
        // Git adopts a new directory's tree verbatim, empty subtrees included.
        return Ok(true);
    }
    for id in trees {
        if subtree_has_file(&id)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// MG-04 post-pass shared by both engines, run while the result still lists
/// every directory entry (empty-directory markers, carried subtrees): a
/// candidate whose directory is still in the way ([`directory_is_in_the_way`])
/// becomes a [`ConflictKind::FileDirectory`]. The three shapes Git
/// distinguishes:
///
/// * the path's own resolution is a modify/delete conflict — Git relocates
///   FIRST and runs the modify/delete branch on the moved entry
///   (`:4120-4198`, then `:4374`), so the kind becomes `modify_delete`;
/// * the file survived on its own (a one-sided add, or `-X` favoured it) —
///   Git's "added on one side" under `df_conflict` is not clean;
/// * the file side changed it and `-X` favoured the deletion — Git keeps
///   modify/delete a conflict regardless of `-X` (verified), so the
///   path-level conflict is re-raised at the moved name.
///
/// Returns the change-count delta for the incremental engine (which counts as
/// it walks): a kept file moved out of the result is one more change when
/// ours held it, one less when theirs added it — exactly what the flat
/// engine's `count_item_map_changes(ours, merged)` sees over its final map.
fn resolve_df_conflicts(
    merged: &mut HashMap<PathBuf, MergeTreeEntry>,
    conflicts: &mut Vec<(PathBuf, ConflictKind)>,
    candidates: Vec<DfCandidate>,
    subtree_has_file: &mut dyn FnMut(&ObjectHash) -> Result<bool, PullMergeError>,
) -> Result<isize, PullMergeError> {
    if candidates.is_empty() {
        return Ok(0);
    }
    // One sorted view of the result and one lookup for the modify/delete slots
    // — the candidate list can grow with the tree, so neither may be rescanned
    // per candidate.
    let mut entries: Vec<(PathBuf, MergeTreeEntry)> = merged
        .iter()
        .map(|(path, entry)| (path.clone(), *entry))
        .collect();
    for (path, kind) in conflicts.iter() {
        entries.push((
            path.clone(),
            match kind {
                ConflictKind::FileDirectory { file, .. } => *file,
                // Only the KIND matters for the "is a file beneath" test.
                _ => MergeTreeEntry {
                    hash: ObjectHash::new(&[0u8; 20]),
                    mode: TreeItemMode::Blob,
                },
            },
        ));
    }
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));
    let modify_delete_slots: HashMap<PathBuf, usize> = conflicts
        .iter()
        .enumerate()
        .filter(|(_, (_, kind))| {
            matches!(
                kind,
                ConflictKind::OursModifiedTheirsDeleted { .. }
                    | ConflictKind::TheirsModifiedOursDeleted { .. }
            )
        })
        .map(|(slot, (path, _))| (path.clone(), slot))
        .collect();
    let mut delta: isize = 0;
    for candidate in candidates {
        let DfCandidate {
            path,
            file_side,
            file,
            base_file,
            base_present,
        } = candidate;
        if !directory_is_in_the_way(&path, &entries, base_present, subtree_has_file)? {
            // The file stays. Anything still recorded BENEATH it contributes no
            // file (that is what "not in the way" means), so it is an
            // empty-only subtree: dropping it keeps a blob and a subtree from
            // sharing one name in the result tree. The flattening engine drops
            // every marker for the same reason; neither path ever rebuilds an
            // empty directory (registered in MG-03).
            if merged
                .get(&path)
                .is_some_and(|entry| entry.mode != TreeItemMode::Tree)
            {
                merged.retain(|other, _| other == &path || !other.starts_with(&path));
            }
            continue;
        }
        let modified_vs_base = base_file.is_some_and(|base| base != file);
        let kind = |modify_delete: bool| ConflictKind::FileDirectory {
            file,
            file_side,
            base_file,
            modify_delete,
        };
        if let Some(&slot) = modify_delete_slots.get(&path) {
            conflicts[slot].1 = kind(true);
        } else if merged
            .get(&path)
            .is_some_and(|entry| entry.mode != TreeItemMode::Tree)
        {
            merged.remove(&path);
            delta += match file_side {
                MergeSide::Ours => 1,
                MergeSide::Theirs => -1,
            };
            conflicts.push((path, kind(modified_vs_base)));
        } else if modified_vs_base {
            conflicts.push((path, kind(true)));
        }
    }
    Ok(delta)
}

/// Open just enough of a carried subtree that CONTAINS one of `paths` for a
/// later insertion at that path not to leave a tree entry and a leaf sharing
/// one name in the result.
///
/// "Just enough" is the load-bearing part (Codex R14). The walk deliberately
/// carries whole subtrees it never opened, and expanding one of them into ALL
/// its leaves reads every tree inside it — with 100 unchanged siblings beside
/// the rename destination that turned 10 reads into 110, for a merge whose
/// result is identical either way. So the descent follows only the route to
/// the paths that need a leaf: at each level the siblings OFF that route are
/// re-recorded exactly as the walk had them (a subtree stays one carried tree
/// entry, a leaf stays a leaf) and only the child on the route is opened. The
/// reads are then the depth of the route, not the size of the subtree.
fn expand_subtrees_covering(
    source: &mut dyn TreeSource,
    items: &mut HashMap<PathBuf, MergeTreeEntry>,
    paths: &[PathBuf],
) -> Result<(), PullMergeError> {
    let mut covering: Vec<PathBuf> = Vec::new();
    for path in paths {
        for ancestor in path.ancestors().skip(1) {
            if ancestor.as_os_str().is_empty() {
                break;
            }
            if items
                .get(ancestor)
                .is_some_and(|entry| entry.mode == TreeItemMode::Tree)
            {
                covering.push(ancestor.to_path_buf());
            }
        }
    }
    covering.sort();
    covering.dedup();
    // Every directory on the way to one of `paths`, indexed ONCE (Codex R15):
    // asking "is this entry on the route" by scanning `paths` per entry made
    // the descent quadratic in the number of renames, which `merge.renameLimit`
    // does not bound because exact pairs are never capped.
    let mut on_route: HashSet<&Path> = HashSet::new();
    for path in paths {
        for ancestor in path.ancestors() {
            if ancestor.as_os_str().is_empty() {
                break;
            }
            if !on_route.insert(ancestor) {
                break;
            }
        }
    }
    for directory in covering {
        let Some(entry) = items.remove(&directory) else {
            continue;
        };
        let mut stack = vec![(directory, entry.hash)];
        while let Some((prefix, id)) = stack.pop() {
            for item in &source.tree(&id)?.tree_items {
                let path = prefix.join(&item.name);
                // On the route means "an ancestor of, or equal to, a path we
                // must place". Everything else is recorded as it stands.
                if item.mode == TreeItemMode::Tree && on_route.contains(path.as_path()) {
                    stack.push((path, item.id));
                } else {
                    items.insert(
                        path,
                        MergeTreeEntry {
                            hash: item.id,
                            mode: item.mode,
                        },
                    );
                }
            }
        }
    }
    Ok(())
}

/// Whether the result carries a subtree AT `path` that holds a file. An
/// unreadable tree is an error, never a silent "occupied" (Codex R5 P2).
fn carried_subtree_holds_a_file(
    source: &mut dyn TreeSource,
    merged: &HashMap<PathBuf, MergeTreeEntry>,
    path: &Path,
) -> Result<bool, PullMergeError> {
    let Some(entry) = merged.get(path) else {
        return Ok(false);
    };
    if entry.mode != TreeItemMode::Tree {
        return Ok(false);
    }
    subtree_holds_a_file(source, &entry.hash)
}

/// Whether a tree holds any non-tree entry, at any depth.
fn subtree_holds_a_file(
    source: &mut dyn TreeSource,
    root: &ObjectHash,
) -> Result<bool, PullMergeError> {
    let mut stack = vec![*root];
    while let Some(id) = stack.pop() {
        for item in &source.tree(&id)?.tree_items {
            if item.mode == TreeItemMode::Tree {
                stack.push(item.id);
            } else {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Every name the merge result occupies: each path AND each of its ancestor
/// directories — Git's `opt->priv->paths` holds directory entries too, so an
/// existing `foo~HEAD/bar` makes `foo~HEAD` taken and the moved file becomes
/// `foo~HEAD_0` (verified against `git merge`).
fn occupied_names<'a>(paths: impl Iterator<Item = &'a PathBuf>) -> HashSet<PathBuf> {
    let mut taken = HashSet::new();
    for path in paths {
        let mut current: Option<&Path> = Some(path.as_path());
        while let Some(prefix) = current {
            if prefix.as_os_str().is_empty() || !taken.insert(prefix.to_path_buf()) {
                break;
            }
            current = prefix.parent();
        }
    }
    taken
}

/// Git's `unique_path`: `<path>~<branch>` with `/` in the branch name flattened
/// to `_`, plus `_<n>` while the name is taken by another path (or directory)
/// of the merge.
fn unique_df_path(path: &Path, branch: &str, taken: &HashSet<PathBuf>) -> PathBuf {
    let flattened: String = branch
        .chars()
        .map(|c| if c == '/' { '_' } else { c })
        .collect();
    let base = format!("{}~{flattened}", path.display());
    let mut candidate = PathBuf::from(&base);
    let mut suffix = 0;
    while taken.contains(&candidate) {
        candidate = PathBuf::from(format!("{base}_{suffix}"));
        suffix += 1;
    }
    candidate
}

/// The branch label a D/F file is renamed after: `HEAD` for ours, the merged
/// ref's name for theirs — the labels the conflict markers already use.
fn df_branch_label(file_side: MergeSide, upstream: &str) -> String {
    match file_side {
        MergeSide::Ours => "HEAD".to_string(),
        MergeSide::Theirs => upstream.to_string(),
    }
}

/// Where each conflict will be unmerged: a D/F file at its `unique_path`,
/// every other conflict at its own path. Also the `(path, kind, original)`
/// triples the `--dry-run` summary reports. `occupied` is what Git's
/// `unique_path` treats as taken — see [`df_occupied_names`].
fn conflict_placements(
    conflicts: &[(PathBuf, ConflictKind)],
    occupied: &HashSet<PathBuf>,
    upstream: &str,
) -> Vec<(PathBuf, ConflictKind, Option<PathBuf>)> {
    let mut taken = occupied.clone();
    let mut placed = Vec::with_capacity(conflicts.len());
    for (path, kind) in conflicts {
        match df_file_side(kind) {
            Some(file_side) => {
                let target = unique_df_path(path, &df_branch_label(file_side, upstream), &taken);
                taken.insert(target.clone());
                placed.push((target, *kind, Some(path.clone())));
            }
            None => placed.push((path.clone(), *kind, None)),
        }
    }
    placed
}

/// Every name Git's `unique_path` treats as taken (`opt->priv->paths` holds
/// every INPUT path — a path deleted on both sides included: a `foo~HEAD` only
/// the base had still yields `foo~HEAD_0`, verified — every result path, and
/// all their ancestor directories).
fn df_occupied_names(
    inputs: &[&HashMap<PathBuf, MergeTreeEntry>],
    merged: &HashMap<PathBuf, MergeTreeEntry>,
    conflicts: &[(PathBuf, ConflictKind)],
) -> HashSet<PathBuf> {
    occupied_names(
        inputs
            .iter()
            .flat_map(|items| items.keys())
            .chain(merged.keys())
            .chain(conflicts.iter().map(|(path, _)| path)),
    )
}

/// [`df_occupied_names`], but only when a relocation will actually consult it
/// — every other conflict keeps its own path, so a merge without a D/F
/// collision pays nothing.
fn df_occupied_names_if_needed(
    inputs: &[&HashMap<PathBuf, MergeTreeEntry>],
    merged: &HashMap<PathBuf, MergeTreeEntry>,
    conflicts: &[(PathBuf, ConflictKind)],
) -> HashSet<PathBuf> {
    if !conflicts
        .iter()
        .any(|(_, kind)| matches!(kind, ConflictKind::FileDirectory { .. }))
    {
        return HashSet::new();
    }
    df_occupied_names(inputs, merged, conflicts)
}

/// The incremental engine's occupancy: only a merge with a D/F relocation
/// needs the inputs' paths, and then it enumerates them — leaves and
/// directories, empty ones included — through the walk's source, which
/// already caches every tree the walk and the gate opened.
fn incremental_df_occupancy(
    source: &mut dyn TreeSource,
    roots: [Option<ObjectHash>; 3],
    merged: &HashMap<PathBuf, MergeTreeEntry>,
    conflicts: &[(PathBuf, ConflictKind)],
) -> Result<HashSet<PathBuf>, PullMergeError> {
    let mut names = occupied_names(merged.keys().chain(conflicts.iter().map(|(path, _)| path)));
    if !conflicts
        .iter()
        .any(|(_, kind)| matches!(kind, ConflictKind::FileDirectory { .. }))
    {
        return Ok(names);
    }
    for root in roots.into_iter().flatten() {
        let mut stack = vec![(PathBuf::new(), root)];
        while let Some((prefix, id)) = stack.pop() {
            let tree = source.tree(&id)?;
            for item in &tree.tree_items {
                let path = prefix.join(&item.name);
                if item.mode == TreeItemMode::Tree {
                    stack.push((path.clone(), item.id));
                }
                names.insert(path);
            }
        }
    }
    Ok(names)
}

/// Which side's file a conflict moves for: only a [`ConflictKind::FileDirectory`]
/// moves (every relocation, modify/delete included, is settled into that kind
/// by [`resolve_df_conflicts`]).
fn df_file_side(kind: &ConflictKind) -> Option<MergeSide> {
    match kind {
        ConflictKind::FileDirectory { file_side, .. } => Some(*file_side),
        _ => None,
    }
}

fn conflict_kind_name(kind: &ConflictKind) -> &'static str {
    match kind {
        ConflictKind::BothChanged { .. } => "content",
        ConflictKind::OursModifiedTheirsDeleted { .. }
        | ConflictKind::TheirsModifiedOursDeleted { .. } => "modify-delete",
        ConflictKind::FileDirectory {
            modify_delete: true,
            ..
        } => "modify-delete",
        ConflictKind::FileDirectory { .. } => "file-directory",
        // A 1to2 destination with another add is a content collision as well
        // as a path conflict; pre-rendering must not hide that public kind.
        ConflictKind::RenameMerged { kind, .. } => match kind {
            RenameConflictKind::RenameRename => "rename-rename",
            RenameConflictKind::Content => "content",
            RenameConflictKind::DirectoryRename => "directory-rename",
        },
        ConflictKind::DirectorySplit { .. } => "directory-rename",
    }
}

/// The `--dry-run` summary's view of the conflicts.
fn conflict_reports(
    placements: &[(PathBuf, ConflictKind, Option<PathBuf>)],
) -> Vec<ConflictReport> {
    placements
        .iter()
        .map(|(path, kind, original)| ConflictReport {
            path: path.display().to_string(),
            kind: conflict_kind_name(kind).to_string(),
            original_path: original.as_ref().map(|p| p.display().to_string()),
        })
        .collect()
}

/// Git's messages for a D/F collision, printed once per moved file — the
/// file/directory line and, when the moved entry is a modify/delete conflict,
/// Git's modify/delete line for the MOVED name (`merge-ort.c:4374`; verified
/// against `git merge`). Human output only: `--json`/`--machine` keep stdout
/// machine-clean and report the conflict through the error envelope.
fn announce_df_conflicts(
    placements: &[(PathBuf, ConflictKind, Option<PathBuf>)],
    upstream: &str,
    output: &OutputConfig,
) {
    if output.is_json() {
        return;
    }
    for (target, kind, original) in placements {
        let Some(original) = original else {
            continue;
        };
        let Some(file_side) = df_file_side(kind) else {
            continue;
        };
        info_println!(
            output,
            "CONFLICT (file/directory): directory in the way of {} from {}; moving it to {} instead.",
            original.display(),
            df_branch_label(file_side, upstream),
            target.display()
        );
        let (deleted_in, modified_in) = match (kind, file_side) {
            (
                ConflictKind::FileDirectory {
                    modify_delete: true,
                    ..
                },
                MergeSide::Ours,
            ) => (upstream, "HEAD"),
            (
                ConflictKind::FileDirectory {
                    modify_delete: true,
                    ..
                },
                MergeSide::Theirs,
            ) => ("HEAD", upstream),
            _ => continue,
        };
        info_println!(
            output,
            "CONFLICT (modify/delete): {target} deleted in {deleted_in} and modified in {modified_in}.  Version {modified_in} of {target} left in tree.",
            target = target.display()
        );
    }
}

/// ADR-MG-01 over three trees WITHOUT flattening them: the read-only gate the
/// preflight and the engine both run.
///
/// Rule (identical to `ensure_gitlinks_not_arbitrated` over the flattened
/// maps): a gitlink path is arbitrated unless all THREE sides carry the same
/// pointer there. Reads: a directory the three sides agree on is never opened
/// (nothing inside can differ); any other directory is opened on every side
/// that has it — an added or deleted subtree is therefore enumerated in full,
/// which is exactly the change being merged. Every arbitrated path is
/// collected and the smallest is reported, as the flattening path does.
/// Returns the pass-through pointers the gate SAW (a pointer inside an unopened
/// subtree travels with that subtree and needs no entry).
fn incremental_gitlink_gate(
    source: &mut dyn TreeSource,
    sides: &[Option<WalkEntry>; 3],
) -> Result<GitlinkEntries, PullMergeError> {
    let mut arbitrated: Vec<PathBuf> = Vec::new();
    let mut passthrough = GitlinkEntries::new();
    gitlink_gate_walk(
        source,
        Path::new(""),
        *sides,
        &mut arbitrated,
        &mut passthrough,
    )?;
    arbitrated.sort();
    if let Some(path) = arbitrated.into_iter().next() {
        return Err(PullMergeError::GitlinkUnsupported(GitlinkNotSupported {
            operation: "merge",
            path,
        }));
    }
    Ok(passthrough)
}

fn gitlink_gate_walk(
    source: &mut dyn TreeSource,
    dir: &Path,
    sides: [Option<WalkEntry>; 3],
    arbitrated: &mut Vec<PathBuf>,
    passthrough: &mut GitlinkEntries,
) -> Result<(), PullMergeError> {
    if let [Some(b), Some(o), Some(t)] = sides
        && b == o
        && o == t
    {
        return Ok(());
    }
    let levels = read_walk_level(source, sides)?;
    let mut names: BTreeSet<&String> = BTreeSet::new();
    for level in &levels {
        names.extend(level.keys());
    }
    for name in names {
        let path = dir.join(name);
        let entries = [
            levels[0].get(name).copied(),
            levels[1].get(name).copied(),
            levels[2].get(name).copied(),
        ];
        let gitlink = |entry: Option<WalkEntry>| {
            entry.is_some_and(|entry| entry.mode == TreeItemMode::Commit)
        };
        if entries.iter().any(|entry| gitlink(*entry)) {
            match entries {
                [Some(b), Some(o), Some(t)] if b == o && o == t => {
                    passthrough.insert(path, b.id);
                }
                _ => arbitrated.push(path),
            }
            continue;
        }
        let trees = entries.map(|entry| entry.filter(|entry| entry.is_tree()));
        if trees.iter().any(Option::is_some) {
            gitlink_gate_walk(source, &path, trees, arbitrated, passthrough)?;
        }
    }
    Ok(())
}

/// The incremental counterpart of the flattening half of
/// `perform_three_way_merge`: same gates, same outputs, same writes — only the
/// tree reads differ.
async fn perform_incremental_three_way_merge(
    current_commit: Commit,
    target_commit: Commit,
    base_commit: Option<&Commit>,
    head_name: String,
    upstream: &str,
    options: ThreeWayMergeOptions<'_>,
) -> Result<Option<PullMergeSummary>, PullMergeError> {
    // ROOT trees go through `refs/replace` exactly as the flattening path's
    // `load_object(&commit.tree_id)` does; nested trees are read raw on both
    // paths (`Tree::load` there, `load_object_raw` here). Same view, same ids.
    let base_tree = base_commit.map(|base| super::replace::resolve(base.tree_id));
    // The single real base is recorded in the merge state exactly as the
    // flattening path records it (`recorded_merge_base`); `None` is the
    // unrelated-history virtual empty base.
    let recorded_base = base_commit.map(|base| base.id);
    let ours_tree = super::replace::resolve(current_commit.tree_id);
    let theirs_tree = super::replace::resolve(target_commit.tree_id);
    let mut source = ObjectStoreTrees::new();
    let mut virtual_blobs = VirtualBlobs::new();
    // MG-05: rename candidates are collected DURING the walk (and inside the
    // pruned diffs it already runs), so collection needs no extra reads; only
    // the deferred enumeration can read more, and only the subtrees that
    // differ on a side offering both a source and a destination.
    let rename_config = three_way_rename_config(&options).await?;
    let (walk, passthrough_gitlinks) = incremental_merge_trees(
        &mut source,
        base_tree,
        ours_tree,
        theirs_tree,
        &mut TreeMergeContext::top_level_with_external(
            !options.dry_run,
            options.favor,
            options.merge_default_driver.as_deref(),
            upstream,
            options.external_merge_runtime.clone(),
            &mut virtual_blobs,
        ),
        rename_config.enabled,
    )?;
    if incremental_may_need_flat_directory_renames(
        &walk.rename_sources,
        &walk.rename_dests,
        &rename_config,
        &virtual_blobs,
    ) {
        return Ok(None);
    }
    let mut files_changed = walk.changed_paths;
    let df_candidates = walk.df_candidates;
    let introduced: HashSet<PathBuf> = walk
        .adopted_from_theirs
        .iter()
        .map(|(dir, _, _)| dir.clone())
        .collect();
    let mut merged_items = walk.merged;
    let mut conflicts = walk.conflicts;
    // The walk resolved each path on its own; a detected rename joins two of
    // them, so the pair is re-resolved at the new path (Git's
    // `process_renames` does the same to its already-collected entries).
    //
    // MG-06: the rename pass now renders content merges of its own, so the
    // conflict style is resolved BEFORE it rather than only on the conflict
    // path — an invalid `merge.conflictStyle` stops the merge before anything
    // is written either way, and a merge without renames still reads the
    // config exactly once.
    let conflict_style = conflict_style_from_config().await.map_err(|e| match e {
        ConflictStyleError::Invalid(value) => PullMergeError::InvalidConflictStyle(value),
        ConflictStyleError::Read(detail) => PullMergeError::ConflictStyleRead(detail),
    })?;
    let mut rename_context = TreeMergeContext::top_level_with_external(
        !options.dry_run,
        options.favor,
        options.merge_default_driver.as_deref(),
        upstream,
        options.external_merge_runtime.clone(),
        &mut virtual_blobs,
    );
    let rename_decisions = apply_incremental_renames(
        &mut source,
        &walk.rename_sources,
        &walk.rename_dests,
        &rename_config,
        &mut merged_items,
        &mut conflicts,
        &mut files_changed,
        &mut rename_context,
        conflict_style,
        upstream,
        base_tree,
    )?;
    // Now the result is complete, so the collisions are settled last: an
    // accepted rename may have emptied a directory that was in the way, and it
    // may equally have CONSUMED the file that made a collision in the first
    // place. A candidate at the source of an accepted rename is exactly the
    // second case — the other side's file at `old` is the same file the rename
    // moved to its new path — so it is dropped rather than settled. The
    // flattening engine never sees these because it remaps before it resolves.
    // Both ends of an accepted rename are stale as collision candidates: the
    // SOURCE because the rename consumed the other side's file there, and the
    // DESTINATION because the fix-up has just resolved that path — settling a
    // candidate recorded during the walk would put the pre-rename blob back and
    // drop the resolved content from every stage (Codex R16/R17).
    let renamed_paths: HashSet<&Path> = rename_decisions
        .decisions
        .iter()
        .flat_map(|decision| match &decision.declined {
            None => [Some(decision.old.as_path()), Some(decision.new.as_path())],
            Some(RenameDeclined::DestinationCollision) => [Some(decision.old.as_path()), None],
            _ => [None, None],
        })
        .flatten()
        .collect();
    let df_candidates: Vec<DfCandidate> = df_candidates
        .into_iter()
        .filter(|candidate| !renamed_paths.contains(candidate.path.as_path()))
        .collect();
    settle_incremental_df_conflicts(
        &mut source,
        &mut merged_items,
        &mut conflicts,
        df_candidates,
        &mut files_changed,
    )?;

    if options.dry_run {
        // A preview writes nothing, so there is no write preflight to wait for
        // — and it must still report the rename decisions the real merge would
        // make (Codex R13 P2). `--json`/`--machine` stay silent, as always.
        announce_rename_notices(
            &rename_decisions.notes,
            &rename_decisions.limited,
            upstream,
            options.output,
        );
        // A real merge's checkout reads every tree the result carries and
        // fails on a missing one before it writes anything; a preview has no
        // checkout, so it probes those trees itself (read-only, through the
        // caching source — the trees the walk opened cost nothing more). This
        // is what keeps the preview's verdict equal to the real merge's, at the
        // read cost the flattening preview always paid.
        // Seen the way the REAL merge would see them. A clean merge checks out
        // the result, and the checkout resolves `refs/replace` on nested trees
        // (`reset::rebuild_index_from_tree` uses `load_object`) — so a clean
        // preview probes through a fresh replacement-aware source. A
        // CONFLICTED merge never checks out: it expands the carried subtrees
        // through the walk's own raw source to write the conflict state — so a
        // conflicted preview probes through that same raw source. Either way
        // the preview fails exactly where the real merge would, and passes
        // where it would.
        if conflicts.is_empty() {
            let mut checkout_view = ObjectStoreTrees::as_checkout_sees_them();
            probe_carried_trees_readable(&mut checkout_view, &merged_items)?;
        } else {
            probe_carried_trees_readable(&mut source, &merged_items)?;
        }
        conflicts.sort_by(|(left, _), (right, _)| left.cmp(right));
        let placements = conflict_placements(
            &conflicts,
            &incremental_df_occupancy(
                &mut source,
                [base_tree, Some(ours_tree), Some(theirs_tree)],
                &merged_items,
                &conflicts,
            )?,
            upstream,
        );
        report_incremental_walk_stats();
        let conflicted_paths: Vec<String> = placements
            .iter()
            .map(|(path, _, _)| path.display().to_string())
            .collect();
        let conflict_kinds = conflict_reports(&placements);
        let would_conflict = !conflicted_paths.is_empty();
        return Ok(Some(PullMergeSummary {
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
            conflict_kinds,
            autostash: None,
        }));
    }

    let resolved_message = resolve_merge_message(
        current_commit.id,
        target_commit.id,
        upstream,
        &head_name,
        options.message_override.as_ref(),
        options.merge_log,
    )?;

    if !conflicts.is_empty() {
        // The conflict path writes per-file index entries and worktree files,
        // so EVERY carried subtree is expanded here — the conflicted index
        // lists every file. Reads here are O(tree), as the flattening path's
        // were; the pruning pays off on the decision and on the clean path.
        expand_adopted_subtrees(&mut source, &mut merged_items)?;
        let (mut base_items, _) = match base_tree {
            Some(id) => split_gitlink_entries(tree_leaves(&mut source, id)?),
            None => (HashMap::new(), GitlinkEntries::new()),
        };
        // The SAME resolved roots the walk merged, so the stage-2/stage-3
        // entries name what was actually merged (a replaced root included).
        let (mut our_items, _) = split_gitlink_entries(tree_leaves(&mut source, ours_tree)?);
        let (mut their_items, _) = split_gitlink_entries(tree_leaves(&mut source, theirs_tree)?);
        // MG-05: a conflict the rename moved is unmerged at the NEW path, so
        // its stages must be looked up there — the same remap the flattening
        // engine applies to its maps before it resolves anything.
        // MG-06: replaying the SAME surgery the walk's rename pass performed
        // is what makes the stages agree with it — a rename/rename(1to2) puts
        // the one merged blob on both destinations and keeps the base under
        // the old name, a collision drops the source entirely. The content
        // merges are deterministic, so replaying them re-derives the identical
        // object ids; the forced conflicts are already in `conflicts` and the
        // replay's copy is discarded.
        let mut replayed_forced = Vec::new();
        apply_renames(
            &mut base_items,
            &mut our_items,
            &mut their_items,
            &rename_decisions.decisions,
            &mut replayed_forced,
            conflict_style,
            (
                df_branch_label(MergeSide::Ours, upstream).as_str(),
                upstream,
            ),
            &mut TreeMergeContext::top_level_with_external(
                !options.dry_run,
                options.favor,
                options.merge_default_driver.as_deref(),
                upstream,
                options.external_merge_runtime.clone(),
                &mut virtual_blobs,
            ),
        )?;
        conflicts.sort_by(|(left, _), (right, _)| left.cmp(right));
        let placements = conflict_placements(
            &conflicts,
            &incremental_df_occupancy(
                &mut source,
                [base_tree, Some(ours_tree), Some(theirs_tree)],
                &merged_items,
                &conflicts,
            )?,
            upstream,
        );
        report_incremental_walk_stats();
        write_conflicted_merge_state(MergeConflictInput {
            head_name,
            message: resolved_message,
            squash: options.squash,
            upstream: upstream.to_string(),
            base: recorded_base,
            allow_unrelated_histories: options.allow_unrelated_histories,
            skip_hooks: options.skip_hooks,
            ours: current_commit.id,
            theirs: target_commit.id,
            merged_items,
            placements: placements.clone(),
            base_items,
            our_items,
            their_items,
            conflict_style,
        })?;
        // Announced only now: the writer's preflight (untracked collisions,
        // symlink traversal, directory takeover) may still refuse the merge,
        // and Git prints nothing when it does. Same for the rename notices.
        announce_rename_notices(
            &rename_decisions.notes,
            &rename_decisions.limited,
            upstream,
            options.output,
        );
        announce_df_conflicts(&placements, upstream, options.output);
        if let Err(error) = crate::command::rerere::auto_update(false).await {
            tracing::warn!("rerere auto-update after merge conflict failed: {error}");
        }
        let paths = placements
            .iter()
            .map(|(path, _, _)| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(PullMergeError::Conflicts {
            paths,
            squash: options.squash,
        });
    }

    let current_index =
        Index::load(path::index()).map_err(|error| PullMergeError::IndexLoad(error.to_string()))?;
    let gitlink_paths: Vec<PathBuf> = passthrough_gitlinks.keys().cloned().collect();
    // Untracked-collision check. The flattening path hands
    // `ensure_no_untracked_conflicts` every leaf; here an adopted subtree is
    // expanded only when an untracked path collides with it (Git's
    // `paths_conflict`, both directions), so the common case reads nothing
    // more. Recomputed from the CURRENT untracked set at every check point — a
    // hook below may create files after this first pass.
    ensure_no_untracked_conflicts(
        &current_index,
        &adopted_aware_write_paths(&mut source, &merged_items, &introduced, &current_index)?,
        &gitlink_paths,
    )?;
    // The preflight passed, so the merge will happen: the rename notices can be
    // printed now (Codex R12 P2 — a refused merge prints no rename decision).
    announce_rename_notices(
        &rename_decisions.notes,
        &rename_decisions.limited,
        upstream,
        options.output,
    );

    // No readability pass over adopted subtrees here — see the invariant on
    // `incremental_merge_trees`: every tree the walk left unopened is one HEAD
    // already references, so the result cannot introduce an unreadable tree
    // the flattening path would have caught; the gate has already opened (and
    // therefore validated) every tree the merge newly brings in.
    let tree_id = create_tree_from_items_map(&merged_items).map_err(PullMergeError::TreeCreate)?;

    if options.squash {
        report_incremental_walk_stats();
        reset_index_and_workdir_to_tree(&tree_id)?;
        return Ok(Some(PullMergeSummary {
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
            conflict_kinds: Vec::new(),
            autostash: None,
        }));
    }

    if options.no_commit {
        report_incremental_walk_stats();
        reset_index_and_workdir_to_tree(&tree_id)?;
        MergeState {
            head_name: head_name.clone(),
            orig_head: current_commit.id.to_string(),
            target: target_commit.id.to_string(),
            target_ref: upstream.to_string(),
            base: recorded_base.map(|base| base.to_string()),
            strategy: None,
            allow_unrelated_histories: options.allow_unrelated_histories,
            skip_hooks: options.skip_hooks,
            conflicted_paths: Vec::new(),
            message: Some(resolved_message.clone()),
        }
        .save()?;
        return Ok(Some(PullMergeSummary {
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
            conflict_kinds: Vec::new(),
            autostash: None,
        }));
    }

    let message = if !options.skip_hooks {
        run_pre_merge_commit_hook(options.output).await?;
        switch::ensure_clean_status(options.output)
            .await
            .map_err(|_| PullMergeError::DirtyWorktree)?;
        // The hook may have created an untracked path — under an adopted
        // subtree included — after the first check: recompute, do not reuse.
        ensure_no_untracked_conflicts(
            &current_index,
            &adopted_aware_write_paths(&mut source, &merged_items, &introduced, &current_index)?,
            &gitlink_paths,
        )?;
        let message = run_merge_message_hooks(&resolved_message, options.output).await?;
        switch::ensure_clean_status(options.output)
            .await
            .map_err(|_| PullMergeError::DirtyWorktree)?;
        ensure_no_untracked_conflicts(
            &current_index,
            &adopted_aware_write_paths(&mut source, &merged_items, &introduced, &current_index)?,
            &gitlink_paths,
        )?;
        message
    } else {
        resolved_message
    };
    let merge_commit = build_merge_commit(
        tree_id,
        vec![current_commit.id, target_commit.id],
        &format_commit_msg(&message, None),
    )
    .await?;
    report_incremental_walk_stats();
    // Check out the result BEFORE the commit and HEAD are written. The
    // checkout rebuilds the index from the result tree and so reads every tree
    // and blob it carries — it is the one full read the merge cannot avoid, and
    // doing it first makes it the validation the flattening path got for free
    // from flattening: a tree or blob the result names but the store lacks
    // fails here, with HEAD, the index and the working tree untouched
    // (`rebuild_index_from_tree` loads everything before the index is saved).
    // The flattening path writes the commit and HEAD first; its crash window
    // leaves HEAD moved and the tree not checked out. This order's window
    // leaves the merge result checked out and staged under the OLD HEAD, with
    // no merge state — visible as staged changes, recoverable by committing or
    // resetting. Git writes index/worktree first as well.
    reset_index_and_workdir_to_tree(&tree_id)?;
    save_object(&merge_commit, &merge_commit.id)
        .map_err(|error| PullMergeError::CommitSave(error.to_string()))?;
    update_head_with_reflog(&head_name, merge_commit.id, upstream, "three-way").await?;
    if !options.skip_hooks {
        run_advisory_repo_hook(RepoHook::PostCommit, &[], None, options.output).await;
    }

    Ok(Some(PullMergeSummary {
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
        conflict_kinds: Vec::new(),
        autostash: None,
    }))
}

/// Read-only availability probe for `--dry-run`: load every tree the result
/// carries by id, the way the checkout will (replacement-aware source), failing
/// with [`PullMergeError::TreeLoad`] where the checkout would fail. Trees only
/// — the checkout is what reads blobs. Costs the carried trees once, which is
/// what the flattening preview read anyway.
fn probe_carried_trees_readable(
    source: &mut dyn TreeSource,
    merged: &HashMap<PathBuf, MergeTreeEntry>,
) -> Result<(), PullMergeError> {
    let mut stack: Vec<ObjectHash> = merged
        .values()
        .filter(|entry| entry.mode == TreeItemMode::Tree)
        .map(|entry| entry.hash)
        .collect();
    while let Some(id) = stack.pop() {
        let tree = source.tree(&id)?;
        stack.extend(
            tree.tree_items
                .iter()
                .filter(|item| item.mode == TreeItemMode::Tree)
                .map(|item| item.id),
        );
    }
    Ok(())
}

/// The leaf paths the merge result will write, for the untracked-collision
/// check: every leaf in `merged`, plus the leaves of any result-INTRODUCING
/// subtree (`introduced`: adopted from theirs or added by theirs) that an
/// untracked path currently collides with (`paths_conflict`, both directions).
/// A subtree nothing collides with — or one ours already had — is not
/// expanded: nothing under it can be overwritten, and expanding it would read
/// for no reason. Recomputed at every check point, so a file a hook created in
/// between is seen.
fn adopted_aware_write_paths(
    source: &mut dyn TreeSource,
    merged: &HashMap<PathBuf, MergeTreeEntry>,
    introduced: &HashSet<PathBuf>,
    current_index: &Index,
) -> Result<Vec<PathBuf>, PullMergeError> {
    let untracked =
        worktree::untracked_workdir_paths(current_index).map_err(PullMergeError::IndexLoad)?;
    let mut check_items = merged.clone();
    // Only subtrees the RESULT introduces (adopted from theirs, or added by
    // theirs) can write anything the working tree does not already track; a
    // subtree ours already had (agreed by all three, or ours' own) changes no
    // file, so an untracked path beneath it is not overwritten and there is
    // nothing to expand — which keeps a huge unchanged subtree unopened even
    // when the working tree has untracked files under it.
    let colliding: Vec<PathBuf> = check_items
        .iter()
        .filter(|(dir, entry)| {
            entry.mode == TreeItemMode::Tree
                && introduced.contains(*dir)
                && untracked
                    .iter()
                    .any(|path| worktree::paths_conflict(path, dir))
        })
        .map(|(dir, _)| dir.clone())
        .collect();
    if !colliding.is_empty() {
        let mut only: HashMap<PathBuf, MergeTreeEntry> = colliding
            .iter()
            .filter_map(|dir| check_items.remove(dir).map(|entry| (dir.clone(), entry)))
            .collect();
        expand_adopted_subtrees(source, &mut only)?;
        check_items.extend(only);
    }
    check_items.retain(|_, entry| entry.mode != TreeItemMode::Tree);
    Ok(worktree_paths_to_write(&check_items))
}

/// Every leaf of `root` as `(path, id, mode)`, through the walk's source.
fn tree_leaves(
    source: &mut dyn TreeSource,
    root: ObjectHash,
) -> Result<Vec<(PathBuf, ObjectHash, TreeItemMode)>, PullMergeError> {
    let mut leaves = Vec::new();
    let mut stack = vec![(PathBuf::new(), root)];
    while let Some((prefix, id)) = stack.pop() {
        let tree = source.tree(&id)?;
        for item in &tree.tree_items {
            let path = prefix.join(&item.name);
            if item.mode == TreeItemMode::Tree {
                stack.push((path, item.id));
            } else {
                leaves.push((path, item.id, item.mode));
            }
        }
    }
    Ok(leaves)
}

fn merge_tree_items(
    base_items: &HashMap<PathBuf, MergeTreeEntry>,
    our_items: &HashMap<PathBuf, MergeTreeEntry>,
    their_items: &HashMap<PathBuf, MergeTreeEntry>,
    context: &mut TreeMergeContext<'_>,
) -> Result<ThreeWayMergeResult, PullMergeError> {
    let mut all_paths: HashSet<PathBuf> = base_items.keys().cloned().collect();
    all_paths.extend(our_items.keys().cloned());
    all_paths.extend(their_items.keys().cloned());

    let mut merged_items = HashMap::new();
    let mut conflicts = Vec::new();
    for path in all_paths {
        let [base, ours, theirs] = sides_without_empty_dir_beside_file(
            base_items.get(&path),
            our_items.get(&path),
            their_items.get(&path),
        );
        match resolve_three_way(&path, base, ours, theirs, context)? {
            MergeResolution::Use(hash) => {
                merged_items.insert(path, hash);
            }
            MergeResolution::Delete => {}
            MergeResolution::Conflict(kind) => conflicts.push((path, kind)),
        }
    }

    // MG-04: every path that is a file on exactly one side while entries exist
    // beneath it (in the result or in any input) is a D/F candidate; the
    // post-pass decides whether the directory really survives. Component-wise
    // path order puts every `foo/...` entry directly after `foo`, so one sorted
    // pass finds them without a quadratic scan. A candidate is recorded even
    // when the file's own resolution deleted it (a strategy option may have),
    // and an empty-directory marker never counts as a file.
    let side_file = |items: &HashMap<PathBuf, MergeTreeEntry>, path: &PathBuf| {
        items
            .get(path)
            .copied()
            .filter(|entry| entry.mode != TreeItemMode::Tree)
    };
    let mut ordered: Vec<&PathBuf> = merged_items
        .keys()
        .chain(conflicts.iter().map(|(path, _)| path))
        .chain(our_items.keys())
        .chain(their_items.keys())
        .collect();
    ordered.sort();
    ordered.dedup();
    let mut candidates: Vec<DfCandidate> = ordered
        .windows(2)
        .filter(|pair| pair[1].starts_with(pair[0]))
        .filter_map(|pair| {
            let path = pair[0];
            let (file_side, file) = match (side_file(our_items, path), side_file(their_items, path))
            {
                (Some(file), None) => (MergeSide::Ours, file),
                (None, Some(file)) => (MergeSide::Theirs, file),
                _ => return None,
            };
            Some(DfCandidate {
                path: path.clone(),
                file_side,
                file,
                base_file: side_file(base_items, path),
                // Filled in below, once (and only if) there is a candidate.
                base_present: false,
            })
        })
        .collect();
    if !candidates.is_empty() {
        let mut base_paths: Vec<&PathBuf> = base_items.keys().collect();
        base_paths.sort();
        for candidate in &mut candidates {
            let at = base_paths.partition_point(|path| path.as_path() < candidate.path.as_path());
            candidate.base_present = base_paths
                .get(at)
                .is_some_and(|path| path.starts_with(&candidate.path));
        }
    }
    // The flattening engine's only tree entries are empty-directory markers.
    let mut no_subtrees = |_: &ObjectHash| Ok(false);
    resolve_df_conflicts(
        &mut merged_items,
        &mut conflicts,
        candidates,
        &mut no_subtrees,
    )?;
    // Empty-directory entries served the D/F decision; the flat result is
    // leaves only (it never rebuilds empty trees — registered in MG-03).
    merged_items.retain(|_, entry| entry.mode != TreeItemMode::Tree);

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
        .filter(|path| {
            // An empty-directory marker (MG-04's flat view) is not a file: it
            // reads as ABSENT, so an empty directory turning into a file is
            // one added file, and a marker on both sides is no change.
            let file = |entry: Option<&MergeTreeEntry>| {
                entry
                    .copied()
                    .filter(|entry| entry.mode != TreeItemMode::Tree)
            };
            file(before.get(path)) != file(after.get(path))
        })
        .count()
}

fn add_blob_index_entry(
    index: &mut Index,
    path: &Path,
    item: MergeTreeEntry,
    stage: u8,
) -> Result<(), PullMergeError> {
    // A gitlink records a SUBMODULE's commit id, which is not an object of this
    // repository — asking for it as a blob would fail. Only a pass-through
    // gitlink (identical on all three sides, ADR-MG-01) ever reaches here, so
    // the pointer is registered verbatim with a zero size.
    let size = if item.mode == TreeItemMode::Commit {
        0
    } else {
        let blob: Blob = load_object(&item.hash).map_err(|error| {
            PullMergeError::IndexSave(format!(
                "failed to load blob {} for index entry '{}': {error}",
                item.hash,
                path.display()
            ))
        })?;
        blob.data.len() as u32
    };
    let mut entry =
        IndexEntry::new_from_blob(path_to_index_key(path)?.to_string(), item.hash, size);
    entry.mode = tree_item_mode_to_index_mode(item.mode)?;
    entry.flags.stage = stage;
    index.add(entry);
    Ok(())
}

/// The merged paths that will actually be materialized in the working tree.
///
/// Pass-through gitlinks are excluded: Libra never writes a submodule working
/// tree, so an already-present submodule directory must not be mistaken for an
/// untracked path the merge is about to overwrite.
fn worktree_paths_to_write(merged_items: &HashMap<PathBuf, MergeTreeEntry>) -> Vec<PathBuf> {
    merged_items
        .iter()
        .filter(|(_, entry)| entry.mode != TreeItemMode::Commit)
        .map(|(path, _)| path.clone())
        .collect()
}

fn ensure_no_untracked_conflicts(
    current_index: &Index,
    paths: &[PathBuf],
    gitlink_paths: &[PathBuf],
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
        // A gitlink is matched on the EXACT path only. Libra writes no content
        // inside a submodule, so untracked files UNDER it are the submodule's
        // own checkout and are not overwritten — but a plain file or symlink
        // sitting exactly there WOULD be replaced by the directory placeholder
        // `restore` creates for a `160000` entry (ADR-MG-01).
        for path in gitlink_paths {
            if untracked == path {
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
        clear_ancestor_files(workdir, relative)?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    // Never write THROUGH a symbolic link: an ignored `foo -> /elsewhere` is
    // invisible to the untracked scan and would redirect the write outside the
    // working tree. A symlink sitting exactly at the path is replaced by the
    // file (as a checkout replaces it).
    refuse_symlink_components(workdir, relative)?;
    clear_write_target(&file_path)?;
    fs::write(&file_path, content)
        .map_err(|error| format!("failed to write {}: {error}", file_path.display()))
}

/// Remove a plain FILE standing where a directory of `relative` must go — the
/// only shape that reaches here is an IGNORED file (an untracked, non-ignored
/// one refuses the merge in `ensure_no_untracked_conflicts`, a tracked one is
/// in this merge's removals, and a symlinked component was refused by
/// `refuse_symlink_components`). Git replaces such a file with the directory;
/// without this `create_dir_all` fails mid-write, and on the flattening path
/// that happens after HEAD has already moved.
fn clear_ancestor_files(workdir: &Path, relative: &Path) -> Result<(), String> {
    let mut ancestors: Vec<&Path> = relative.ancestors().skip(1).collect();
    ancestors.reverse();
    for ancestor in ancestors {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        let full = workdir.join(ancestor);
        match fs::symlink_metadata(&full) {
            Ok(meta) if !meta.is_dir() && !meta.file_type().is_symlink() => {
                fs::remove_file(&full).map_err(|error| {
                    format!(
                        "failed to replace the file {} with a directory: {error}",
                        ancestor.display()
                    )
                })?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Make `full` writable as a file or a link: a symbolic link there is
/// unlinked (never followed), and a directory is taken over when it holds
/// nothing but (nested) empty directories — MG-04's `foo/` giving way back to
/// the file `foo`, whose tracked files were removed just before, and whose
/// non-empty case the write preflight already refused.
fn clear_write_target(full: &Path) -> Result<(), String> {
    let Ok(meta) = fs::symlink_metadata(full) else {
        return Ok(());
    };
    if meta.file_type().is_symlink() {
        return fs::remove_file(full).map_err(|error| {
            format!(
                "failed to replace symbolic link {}: {error}",
                full.display()
            )
        });
    }
    if meta.is_dir() {
        return remove_empty_dir_tree(full).map_err(|error| {
            format!(
                "failed to replace directory {} with a file: {error}",
                full.display()
            )
        });
    }
    // A regular file is UNLINKED, never truncated in place: Git replaces the
    // directory entry (verified — after `git merge` rewrites a tracked file
    // its inode changes and a hard-linked alias elsewhere keeps the old
    // content and mode), so writing through the old inode would corrupt such
    // an alias and keep stale permissions.
    fs::remove_file(full).map_err(|error| format!("failed to replace {}: {error}", full.display()))
}

/// Refuse to write `relative` if any directory on its way is a symbolic link
/// (Git: "beyond a symbolic link"). Checked with `symlink_metadata`, never
/// following the link.
fn refuse_symlink_components(workdir: &Path, relative: &Path) -> Result<(), String> {
    let mut current = relative.parent();
    while let Some(dir) = current {
        if dir.as_os_str().is_empty() {
            break;
        }
        if fs::symlink_metadata(workdir.join(dir)).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(format!(
                "refusing to write '{}' through the symbolic link '{}'",
                relative.display(),
                dir.display()
            ));
        }
        current = dir.parent();
    }
    Ok(())
}

/// The same check over every path a merge is about to write OR remove, run
/// BEFORE the first mutation so a refused merge leaves nothing behind. A
/// symlink on the way is tolerated only when it is itself one of the tracked
/// paths this merge removes (a symlink `foo` giving way to the directory
/// `foo/`): removals run first, so by the time `foo/bar` is written the link
/// is gone. Any other symlink — an ignored `gone -> /elsewhere` above a
/// historical `gone/file`, say — would make a removal unlink an external file
/// or a write land outside the working tree, and refuses the merge.
fn refuse_symlink_traversal(
    workdir: &Path,
    writes: &[PathBuf],
    removals: &[PathBuf],
) -> Result<(), PullMergeError> {
    let removed: HashSet<&PathBuf> = removals.iter().collect();
    let mut cleared: HashSet<PathBuf> = HashSet::new();
    for path in writes.iter().chain(removals) {
        let mut current = path.parent();
        while let Some(dir) = current {
            if dir.as_os_str().is_empty() || cleared.contains(dir) {
                break;
            }
            if !removed.contains(&dir.to_path_buf())
                && fs::symlink_metadata(workdir.join(dir))
                    .is_ok_and(|meta| meta.file_type().is_symlink())
            {
                return Err(PullMergeError::WorkdirReset(format!(
                    "refusing to touch '{}' through the symbolic link '{}'",
                    path.display(),
                    dir.display()
                )));
            }
            cleared.insert(dir.to_path_buf());
            current = dir.parent();
        }
    }
    // A path the merge writes as a FILE while the working tree has a directory
    // there (MG-04: `foo/` giving way back to the file `foo`) is taken over
    // only when nothing but this merge's own removals lives inside it —
    // checked here, before the first mutation, instead of failing mid-write.
    for path in writes {
        let full = workdir.join(path);
        if !fs::symlink_metadata(&full).is_ok_and(|meta| meta.is_dir()) {
            continue;
        }
        if let Some(blocker) = directory_content_outside(&full, workdir, &removed)? {
            return Err(PullMergeError::WorkdirReset(format!(
                "refusing to replace directory '{}' with a file: '{}' is in the way",
                path.display(),
                blocker.display()
            )));
        }
    }
    Ok(())
}

/// The first entry under `dir` that this merge is not going to remove, as a
/// path relative to `workdir` (`None` when the directory holds nothing else).
/// Never follows a symbolic link.
fn directory_content_outside(
    dir: &Path,
    workdir: &Path,
    removed: &HashSet<&PathBuf>,
) -> Result<Option<PathBuf>, PullMergeError> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = fs::read_dir(&current).map_err(|error| {
            PullMergeError::WorkdirReset(format!(
                "failed to inspect {}: {error}",
                current.display()
            ))
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                PullMergeError::WorkdirReset(format!(
                    "failed to inspect {}: {error}",
                    current.display()
                ))
            })?;
            let file_type = entry.file_type().map_err(|error| {
                PullMergeError::WorkdirReset(format!(
                    "failed to inspect {}: {error}",
                    entry.path().display()
                ))
            })?;
            if file_type.is_dir() && !file_type.is_symlink() {
                stack.push(entry.path());
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(workdir)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| entry.path());
            if !removed.contains(&relative) {
                return Ok(Some(relative));
            }
        }
    }
    Ok(None)
}

/// Remove a directory that holds nothing but (nested) empty directories —
/// what `foo/a/` looks like once its tracked files are gone. Any file inside
/// is an error: nothing untracked is ever deleted here.
fn remove_empty_dir_tree(dir: &Path) -> std::io::Result<()> {
    if fs::symlink_metadata(dir)?.file_type().is_symlink() {
        return Err(std::io::Error::other(format!(
            "{} is a symbolic link",
            dir.display()
        )));
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() && !file_type.is_symlink() {
            remove_empty_dir_tree(&entry.path())?;
        } else {
            return Err(std::io::Error::other(format!(
                "{} is not empty ({} is in the way)",
                dir.display(),
                entry.path().display()
            )));
        }
    }
    fs::remove_dir(dir)
}

/// After a tracked file is removed, drop the directories it leaves empty (up
/// to, not including, the working tree root) — as Git's checkout does. A
/// directory that still holds anything stays.
fn prune_empty_parents(workdir: &Path, relative: &Path) {
    let mut current = relative.parent();
    while let Some(dir) = current {
        if dir.as_os_str().is_empty() || fs::remove_dir(workdir.join(dir)).is_err() {
            break;
        }
        current = dir.parent();
    }
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
        ConflictKind::BothChanged {
            base,
            ours,
            theirs,
            driver,
            rendered,
        } => {
            if let Some(rendered) = rendered {
                return load_object::<Blob>(&rendered.hash)
                    .map(|blob| blob.data)
                    .map_err(|error| error.to_string())
                    .and_then(|content| write_workdir_file(workdir, path, &content));
            }
            let ours_blob: Blob = load_object(&ours).map_err(|error| error.to_string())?;
            let theirs_blob: Blob = load_object(&theirs).map_err(|error| error.to_string())?;
            match driver {
                BuiltinMergeDriver::Binary => ours_blob.data,
                BuiltinMergeDriver::Union => {
                    let base_data = match base {
                        Some(base) => {
                            load_object::<Blob>(&base)
                                .map_err(|error| error.to_string())?
                                .data
                        }
                        None => Vec::new(),
                    };
                    match merge_bytes_with_driver(
                        driver,
                        &base_data,
                        &ours_blob.data,
                        &theirs_blob.data,
                        None,
                        conflict_style,
                        0,
                    )? {
                        BuiltinMergeOutcome::Clean(bytes)
                        | BuiltinMergeOutcome::Conflict(bytes) => bytes,
                    }
                }
                BuiltinMergeDriver::Text => both_changed_conflict_content(
                    base,
                    &ours_blob.data,
                    &theirs_blob.data,
                    marker_eol,
                    commit_abbrev,
                    conflict_style,
                )?,
            }
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
        // The directory kept the original path; `path` here is already the
        // file's `unique_path`, and its content is written verbatim — Git
        // moves the file, it does not mark it up.
        ConflictKind::FileDirectory { file, .. } => {
            let blob: Blob = load_object(&file.hash).map_err(|error| error.to_string())?;
            blob.data
        }
        // MG-06: already the merged result Git recorded for the rename
        // (`merge-ort.c:3021-3053`), including its conflict markers when the
        // content merge was not clean — written verbatim, never marked up
        // twice.
        ConflictKind::RenameMerged { content, .. } => {
            let blob: Blob = load_object(&content.hash).map_err(|error| error.to_string())?;
            return write_workdir_entry(workdir, path, content.mode, &blob.data);
        }
        ConflictKind::DirectorySplit { content } => {
            let blob: Blob = load_object(&content.hash).map_err(|error| error.to_string())?;
            return write_workdir_entry(workdir, path, content.mode, &blob.data);
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
            "HEAD",
            commit_label,
            "base",
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
fn relabel_conflict_markers(
    conflicted: Vec<u8>,
    marker_len: usize,
    ours_label: &str,
    theirs_label: &str,
    // The `|||||||` label under `diff3`. Git names the merge base
    // `<ancestor>` when all three paths are the same and `<ancestor>:<path>`
    // when they are not (`merge-ort.c:2147-2155`), so a rename-driven merge
    // labels it with the SOURCE path (Codex R1 P2-2).
    base_label: &str,
) -> Vec<u8> {
    let open = "<".repeat(marker_len);
    let close = ">".repeat(marker_len);
    let bars = "|".repeat(marker_len);
    let ours_marker = format!("{open} ours");
    let theirs_marker = format!("{close} theirs");
    // `diffy`'s diff3 base marker; only emitted under ConflictStyle::Diff3.
    let original_marker = format!("{bars} original");
    let head_marker = format!("{open} {ours_label}");
    let label_marker = format!("{close} {theirs_label}");
    // Match the `||||||| base` label convention `restore --conflict=diff3` uses.
    let base_marker = format!("{bars} {base_label}");

    // Byte-wise, never through `String::from_utf8_lossy`: the recursive
    // virtual-ancestor fold relabels content that Git's binary rule considers
    // TEXT (no NUL byte) but that need not be valid UTF-8, and a lossy
    // conversion would rewrite those bytes as U+FFFD.
    //
    // `split(b'\n')` + rejoining round-trips exactly, including a trailing
    // newline (which yields a final empty segment that re-joins cleanly).
    let mut relabelled = Vec::with_capacity(conflicted.len());
    for (index, line) in conflicted.split(|byte| *byte == b'\n').enumerate() {
        if index > 0 {
            relabelled.push(b'\n');
        }
        let replacement = if line == ours_marker.as_bytes() {
            head_marker.as_bytes()
        } else if line == theirs_marker.as_bytes() {
            label_marker.as_bytes()
        } else if line == original_marker.as_bytes() {
            base_marker.as_bytes()
        } else {
            line
        };
        relabelled.extend_from_slice(replacement);
    }
    relabelled
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
    let writes: Vec<PathBuf> = new_tracked_paths
        .iter()
        .filter(|path| !is_gitlink_index_path(new_index, path).unwrap_or(false))
        .cloned()
        .collect();
    let removals: Vec<PathBuf> = current_index
        .tracked_files()
        .into_iter()
        .filter(|path| !new_tracked_paths.contains(path))
        .filter(|path| !is_gitlink_index_path(current_index, path).unwrap_or(false))
        .collect();
    refuse_symlink_traversal(&workdir, &writes, &removals)?;
    for path_buf in current_index.tracked_files() {
        if !new_tracked_paths.contains(&path_buf) {
            // A submodule directory is not Libra's to delete, and a gitlink can
            // only leave the index through a decision the ADR-MG-01 guard
            // already refused — so never unlink one here.
            if is_gitlink_index_path(current_index, &path_buf)? {
                continue;
            }
            let full_path = workdir.join(&path_buf);
            // `exists()` FOLLOWS symlinks, so a dangling tracked link would
            // survive and then block the write of a path beneath it (MG-04: a
            // tracked symlink `foo` giving way to the directory `foo/`).
            if fs::symlink_metadata(&full_path).is_ok() {
                fs::remove_file(&full_path).map_err(|error| {
                    PullMergeError::WorkdirReset(format!("failed to remove file: {error}"))
                })?;
                prune_empty_parents(&workdir, &path_buf);
            }
        }
    }

    for path_buf in new_index.tracked_files() {
        if let Some(entry) = new_index.get(path_to_index_key(&path_buf)?, 0) {
            // Pass-through gitlink: nothing to materialize in the working tree.
            if entry.mode & 0o170000 == 0o160000 {
                continue;
            }
            let blob: Blob = load_object(&entry.hash).map_err(|error| {
                PullMergeError::WorkdirReset(format!(
                    "failed to load blob {} for '{}': {error}",
                    entry.hash,
                    path_buf.display()
                ))
            })?;
            write_workdir_entry(
                &workdir,
                &path_buf,
                index_mode_to_tree_item_mode(entry.mode)?,
                &blob.data,
            )
            .map_err(PullMergeError::WorkdirReset)?;
        }
    }
    Ok(())
}

/// Whether `path` is recorded in `index` as a gitlink (`160000`) at stage 0.
fn is_gitlink_index_path(index: &Index, path: &Path) -> Result<bool, PullMergeError> {
    Ok(index
        .get(path_to_index_key(path)?, 0)
        .is_some_and(|entry| entry.mode & 0o170000 == 0o160000))
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
        // Reachable only for a pass-through gitlink (ADR-MG-01): an arbitrated
        // one is refused by `ensure_gitlinks_not_arbitrated` long before the
        // index is built, so recording the unchanged pointer keeps the index
        // consistent with the merged tree instead of dropping the submodule.
        TreeItemMode::Commit => Ok(0o160000),
    }
}

fn index_mode_to_tree_item_mode(mode: u32) -> Result<TreeItemMode, PullMergeError> {
    match mode {
        0o100644 => Ok(TreeItemMode::Blob),
        0o100755 => Ok(TreeItemMode::BlobExecutable),
        0o120000 => Ok(TreeItemMode::Link),
        0o160000 => Ok(TreeItemMode::Commit),
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
#[path = "merge_rename_content_test.rs"]
mod merge_rename_content_test;

#[cfg(test)]
mod driver {
    use super::*;

    #[test]
    fn builtins_follow_attribute_precedence_and_text_fallbacks() {
        for (attribute, default, expected) in [
            (Some(AttributeState::Set), None, BuiltinMergeDriver::Text),
            (
                Some(AttributeState::Unset),
                Some("union"),
                BuiltinMergeDriver::Binary,
            ),
            (
                Some(AttributeState::Value("union".to_string())),
                None,
                BuiltinMergeDriver::Union,
            ),
            (
                Some(AttributeState::Value("unknown".to_string())),
                Some("union"),
                BuiltinMergeDriver::Text,
            ),
            (None, Some("binary"), BuiltinMergeDriver::Binary),
            (None, Some("unknown"), BuiltinMergeDriver::Text),
            (None, None, BuiltinMergeDriver::Text),
        ] {
            assert_eq!(select_builtin_merge_driver(attribute, default), expected);
        }
    }

    #[test]
    fn union_retains_only_both_conflicting_sides_in_order() {
        let result = merge_bytes_with_driver(
            BuiltinMergeDriver::Union,
            b"top\nbase\nbottom\n",
            b"top\nours\nbottom\n",
            b"top\ntheirs\nbottom\n",
            None,
            diffy::ConflictStyle::Merge,
            0,
        )
        .expect("union driver");
        assert_eq!(
            result,
            BuiltinMergeOutcome::Clean(b"top\nours\ntheirs\nbottom\n".to_vec())
        );
    }

    #[test]
    fn binary_driver_keeps_trivial_three_way_resolutions_clean() {
        for (base, ours, theirs, expected) in [
            (
                &b"base\n"[..],
                &b"base\n"[..],
                &b"theirs\n"[..],
                &b"theirs\n"[..],
            ),
            (
                &b"base\n"[..],
                &b"ours\n"[..],
                &b"base\n"[..],
                &b"ours\n"[..],
            ),
            (
                &b"base\n"[..],
                &b"same\n"[..],
                &b"same\n"[..],
                &b"same\n"[..],
            ),
        ] {
            let outcome = merge_bytes_with_driver(
                BuiltinMergeDriver::Binary,
                base,
                ours,
                theirs,
                None,
                diffy::ConflictStyle::Merge,
                0,
            )
            .expect("binary driver");
            assert_eq!(outcome, BuiltinMergeOutcome::Clean(expected.to_vec()));
        }
    }

    #[test]
    fn union_driver_falls_back_to_binary_for_nul_content() {
        let ours = b"ours\0bytes";
        let outcome = merge_bytes_with_driver(
            BuiltinMergeDriver::Union,
            b"base\0bytes",
            ours,
            b"theirs\0bytes",
            None,
            diffy::ConflictStyle::Merge,
            0,
        )
        .expect("union binary fallback");
        assert_eq!(outcome, BuiltinMergeOutcome::Conflict(ours.to_vec()));
    }

    #[test]
    fn recursive_binary_driver_uses_the_original_for_the_virtual_ancestor() {
        let mut blobs = VirtualBlobs::new();
        let mut entry = |data: &[u8]| {
            let blob = Blob::from_content_bytes(data.to_vec());
            blobs.insert(blob.id, blob.data);
            MergeTreeEntry {
                hash: blob.id,
                mode: TreeItemMode::Blob,
            }
        };
        let base = entry(b"base\n");
        let ours = entry(b"ours\n");
        let theirs = entry(b"theirs\n");
        let path = PathBuf::from("driver.txt");
        let base_items = HashMap::from([(path.clone(), base)]);
        let our_items = HashMap::from([(path.clone(), ours)]);
        let their_items = HashMap::from([(path.clone(), theirs)]);
        let rename_config = MergeRenameConfig::default();

        let merged = merge_virtual_items(
            &base_items,
            &our_items,
            &their_items,
            1,
            &mut blobs,
            VirtualFold {
                persist: false,
                conflict_style: diffy::ConflictStyle::Merge,
                rename_config: &rename_config,
                default_driver: Some("binary"),
                external_merge_runtime: Arc::new(ExternalMergeRuntime::default()),
            },
        )
        .expect("recursive binary-driver merge");

        assert_eq!(merged.get(&path), Some(&base));
    }
}

#[cfg(test)]
mod ext_driver {
    #[cfg(unix)]
    use std::os::unix::{ffi::OsStrExt as _, fs::PermissionsExt as _};

    use super::*;

    fn input<'a>(path: &'a Path, labels: ExternalMergeLabels<'a>) -> ExternalMergeInput<'a> {
        ExternalMergeInput {
            path,
            base_id: ObjectHash::new(&[1; 20]),
            ours_id: ObjectHash::new(&[2; 20]),
            theirs_id: ObjectHash::new(&[3; 20]),
            base: b"base\n",
            ours: b"ours\n",
            theirs: b"theirs\n",
            marker_length: 9,
            labels,
        }
    }

    #[cfg(unix)]
    #[test]
    fn placeholders_quote_only_path_and_revision_labels() {
        let worktree = tempfile::tempdir().expect("create worktree");
        let path = Path::new("dir/odd ' $(touch PWNED).txt");
        let merge_input = input(
            path,
            ExternalMergeLabels {
                ancestor: "merged common ancestors",
                ours: "Temporary merge branch 1's side",
                theirs: "Temporary merge branch 2 $(false)",
            },
        );
        let files = ExternalMergeTempFiles::create_in(worktree.path(), &merge_input)
            .expect("create protected inputs");
        let expanded = expand_external_merge_command(
            "driver %O %A %B %L %P %S %X %Y %% %Q",
            &files,
            &merge_input,
        );
        let expanded = expanded.to_string_lossy();

        assert!(expanded.contains(files.base.path().to_string_lossy().as_ref()));
        assert!(expanded.contains(files.ours.path().to_string_lossy().as_ref()));
        assert!(expanded.contains(files.theirs.path().to_string_lossy().as_ref()));
        assert!(expanded.contains(" 9 "));
        assert!(expanded.contains("'dir/odd '\\'' $(touch PWNED).txt'"));
        assert!(expanded.contains("'merged common ancestors'"));
        assert!(expanded.contains("'Temporary merge branch 1'\\''s side'"));
        assert!(expanded.contains("'Temporary merge branch 2 $(false)'"));
        assert!(expanded.ends_with(" % %Q"));
    }

    #[cfg(unix)]
    #[test]
    fn placeholder_expansion_preserves_non_utf8_path_bytes() {
        let worktree = tempfile::tempdir().expect("create worktree");
        let raw_path = b"dir/non-utf8-\xff.txt";
        let path = Path::new(OsStr::from_bytes(raw_path));
        let merge_input = input(
            path,
            ExternalMergeLabels {
                ancestor: "base",
                ours: "HEAD",
                theirs: "feature",
            },
        );
        let files = ExternalMergeTempFiles::create_in(worktree.path(), &merge_input)
            .expect("create protected inputs");
        let expanded = expand_external_merge_command("driver %P", &files, &merge_input);
        let expanded = expanded.as_os_str().as_bytes();

        assert!(
            expanded
                .windows(raw_path.len())
                .any(|window| window == raw_path),
            "%P must preserve platform-native path bytes"
        );

        let raw_temp_path = b"/tmp/worktree-\xff/ours-file";
        let quoted_temp = expand_external_merge_path(Path::new(OsStr::from_bytes(raw_temp_path)));
        assert!(
            quoted_temp
                .as_os_str()
                .as_bytes()
                .windows(raw_temp_path.len())
                .any(|window| window == raw_temp_path),
            "%O/%A/%B path expansion must preserve platform-native bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn temporary_inputs_are_private_unpredictable_and_removed_on_drop() {
        let worktree = tempfile::tempdir().expect("create worktree");
        let merge_input = input(
            Path::new("driver.txt"),
            ExternalMergeLabels {
                ancestor: "base",
                ours: "HEAD",
                theirs: "feature",
            },
        );
        let files = ExternalMergeTempFiles::create_in(worktree.path(), &merge_input)
            .expect("create protected inputs");
        let directory = files._directory.path().to_path_buf();
        assert_eq!(
            fs::metadata(&directory)
                .expect("temp metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let names: HashSet<_> = [&files.base, &files.ours, &files.theirs]
            .into_iter()
            .map(|file| {
                file.path()
                    .file_name()
                    .expect("random file name")
                    .to_owned()
            })
            .collect();
        assert_eq!(names.len(), 3);
        for file in [&files.base, &files.ours, &files.theirs] {
            assert!(file.path().is_absolute());
            assert_eq!(
                fs::metadata(file.path())
                    .expect("input metadata")
                    .permissions()
                    .mode()
                    & 0o077,
                0,
                "merge inputs are not readable by group/other"
            );
        }
        drop(files);
        assert!(!directory.exists(), "RAII removes the temporary directory");
    }

    #[test]
    fn configured_driver_overrides_a_builtin_name_but_boolean_attributes_do_not() {
        let runtime = ExternalMergeRuntime {
            drivers: HashMap::from([(
                "text".to_string(),
                ExternalMergeDriver {
                    name: "text".to_string(),
                    command: "custom".to_string(),
                },
            )]),
            cache: Mutex::new(HashMap::new()),
        };
        assert!(matches!(
            select_merge_driver(
                Some(AttributeState::Value("text".to_string())),
                None,
                &runtime
            ),
            SelectedMergeDriver::External(_)
        ));
        assert_eq!(
            select_merge_driver(Some(AttributeState::Set), Some("text"), &runtime),
            SelectedMergeDriver::Builtin(BuiltinMergeDriver::Text)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_cli_hints_preserve_both_resolution_and_abort_choices() {
        let cli = merge_error_to_cli(PullMergeError::Conflicts {
            paths: "renamed.txt".to_string(),
            squash: false,
        });
        let rendered = cli.render();
        assert_eq!(rendered.matches("libra merge --continue").count(), 1);
        assert_eq!(rendered.matches("libra merge --abort").count(), 1);
    }

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
    fn strategy_option_favors_only_conflicting_hunks() {
        let base = b"top\nconflict\nmiddle\nbottom\n";
        let ours = b"top\nOURS\nmiddle\nbottom\n";
        let theirs = b"top\nTHEIRS\nmiddle\ntheirs-clean\n";
        let marker_len = unambiguous_conflict_marker_length(&[base, ours, theirs]);
        let mut options = diffy::MergeOptions::new();
        options
            .set_conflict_style(diffy::ConflictStyle::Diff3)
            .set_conflict_marker_length(marker_len);
        let conflicted = options
            .merge_bytes(base, ours, theirs)
            .expect_err("fixture has one conflicting and one clean target hunk");

        assert_eq!(
            resolve_favored_content(conflicted.clone(), marker_len, MergeFavor::Ours)
                .expect("favor ours"),
            b"top\nOURS\nmiddle\ntheirs-clean\n"
        );
        assert_eq!(
            resolve_favored_content(conflicted, marker_len, MergeFavor::Theirs)
                .expect("favor theirs"),
            b"top\nTHEIRS\nmiddle\ntheirs-clean\n"
        );
    }

    #[test]
    fn strategy_option_parser_handles_marker_like_content_and_no_final_newline() {
        let base = b"prefix <<<<<<< ours\nbase";
        let ours = b"prefix <<<<<<< ours\nOURS";
        let theirs = b"prefix <<<<<<< ours\nTHEIRS";
        let marker_len = unambiguous_conflict_marker_length(&[base, ours, theirs]);
        assert_eq!(
            marker_len, 8,
            "mid-line marker runs must bump the parser marker"
        );
        let mut options = diffy::MergeOptions::new();
        options
            .set_conflict_style(diffy::ConflictStyle::Diff3)
            .set_conflict_marker_length(marker_len);
        let conflicted = options
            .merge_bytes(base, ours, theirs)
            .expect_err("fixture conflicts at an unterminated final line");
        assert_eq!(
            resolve_favored_content(conflicted, marker_len, MergeFavor::Ours)
                .expect("favor ours without a final newline"),
            b"prefix <<<<<<< ours\nOURS"
        );
    }

    #[test]
    fn strategy_option_resolves_add_add_but_never_modify_delete() {
        let base = merge_entry(1, TreeItemMode::Blob);
        let ours = merge_entry(2, TreeItemMode::Blob);
        let theirs = merge_entry(3, TreeItemMode::Blob);
        let mut no_virtual_blobs = VirtualBlobs::new();
        let mut favored = |base, ours, theirs, favor| {
            let mut context =
                TreeMergeContext::top_level(false, Some(favor), None, &mut no_virtual_blobs);
            resolve_three_way(Path::new("f"), base, ours, theirs, &mut context)
                .expect("favored resolution")
        };

        assert!(matches!(
            favored(None, Some(&ours), Some(&theirs), MergeFavor::Ours),
            MergeResolution::Use(entry) if entry == ours
        ));
        assert!(matches!(
            favored(None, Some(&ours), Some(&theirs), MergeFavor::Theirs),
            MergeResolution::Use(entry) if entry == theirs
        ));
        // A modify/delete is not a content conflict: neither option settles it,
        // in either direction. Git prints `CONFLICT (modify/delete)` and keeps
        // the modified side; resolving it in favour of the deletion destroyed
        // that content silently (FIX-MG05-01).
        for favor in [MergeFavor::Ours, MergeFavor::Theirs] {
            assert!(
                matches!(
                    favored(Some(&base), Some(&ours), None, favor),
                    MergeResolution::Conflict(ConflictKind::OursModifiedTheirsDeleted { .. })
                ),
                "ours modified / theirs deleted stays a conflict under {favor:?}"
            );
            assert!(
                matches!(
                    favored(Some(&base), None, Some(&theirs), favor),
                    MergeResolution::Conflict(ConflictKind::TheirsModifiedOursDeleted { .. })
                ),
                "theirs modified / ours deleted stays a conflict under {favor:?}"
            );
        }
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

    #[test]
    fn merge_args_parse_noninteractive_strategy_controls() {
        let args = MergeArgs::try_parse_from([
            "merge",
            "-Xours",
            "-X",
            "theirs",
            "--allow-unrelated-histories",
            "--log=7",
            "feature",
        ])
        .expect("parse strategy options");
        assert_eq!(
            args.strategy_option,
            vec![MergeFavor::Ours, MergeFavor::Theirs]
        );
        assert!(args.allow_unrelated_histories);
        assert_eq!(args.log, Some(7));

        let bare_log =
            MergeArgs::try_parse_from(["merge", "--log", "feature"]).expect("parse bare --log");
        assert_eq!(bare_log.log, Some(20));

        let ours = MergeArgs::try_parse_from(["merge", "-s", "ours", "feature"])
            .expect("parse ours strategy");
        assert_eq!(ours.strategy, Some(MergeStrategy::Ours));
        assert!(
            MergeArgs::try_parse_from(["merge", "-s", "recursive", "feature"]).is_err(),
            "unsupported strategies fail during argument parsing"
        );
    }

    #[test]
    fn merge_state_deserializes_pre_strategy_schema() {
        let state: MergeState = serde_json::from_str(
            r#"{
                "head_name":"main",
                "orig_head":"orig",
                "target":"target",
                "target_ref":"feature",
                "base":"base",
                "conflicted_paths":["shared.txt"],
                "message":"Merge feature into main"
            }"#,
        )
        .expect("deserialize merge state written before P1-07b");

        assert_eq!(state.base.as_deref(), Some("base"));
        assert_eq!(state.strategy, None);
        assert!(!state.allow_unrelated_histories);
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
        assert_eq!(
            PullMergeError::VirtualAncestorTooDeep.to_string(),
            "merging these branches needs a virtual common ancestor nested more than 20 levels \
             deep, which Libra does not build",
        );
        assert_eq!(
            PullMergeError::VirtualAncestorTooWide { bases: 33 }.to_string(),
            "merging these branches needs a virtual common ancestor folded from 33 merge bases, \
             more than the 32 Libra folds",
        );
        assert_eq!(
            PullMergeError::GitlinkUnsupported(GitlinkNotSupported {
                operation: "merge",
                path: PathBuf::from("vendor/sub"),
            })
            .to_string(),
            "merge would have to merge the submodule (gitlink) entry 'vendor/sub': Libra does not support submodules",
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

        let mut no_virtual_blobs = VirtualBlobs::new();
        let result = merge_tree_items(
            &base_items,
            &our_items,
            &their_items,
            &mut TreeMergeContext::top_level(true, None, None, &mut no_virtual_blobs),
        )
        .expect("merge tree items");

        assert!(result.conflicts.is_empty());
        assert_eq!(result.merged_items.get(&path), Some(&theirs));
    }

    fn gitlink_side(entries: &[(&str, u8)]) -> GitlinkEntries {
        entries
            .iter()
            .map(|(path, byte)| (PathBuf::from(path), ObjectHash::new(&[*byte; 20])))
            .collect()
    }

    #[test]
    fn split_gitlink_entries_separates_pointers_from_mergeable_entries() {
        let blob = ObjectHash::new(&[1; 20]);
        let gitlink = ObjectHash::new(&[2; 20]);
        let (mergeable, gitlinks) = split_gitlink_entries(vec![
            (PathBuf::from("a.txt"), blob, TreeItemMode::Blob),
            (PathBuf::from("vendor"), gitlink, TreeItemMode::Commit),
        ]);

        assert_eq!(
            mergeable.get(Path::new("a.txt")),
            Some(&MergeTreeEntry {
                hash: blob,
                mode: TreeItemMode::Blob,
            })
        );
        assert!(
            !mergeable.contains_key(Path::new("vendor")),
            "a gitlink must never reach the three-way decision"
        );
        assert_eq!(gitlinks.get(Path::new("vendor")), Some(&gitlink));
    }

    #[test]
    fn ensure_gitlinks_not_arbitrated_passes_pointers_all_sides_agree_on() {
        let side = gitlink_side(&[("vendor", 7)]);

        let passthrough = ensure_gitlinks_not_arbitrated("merge", &side, &side, &side)
            .expect("an unchanged submodule pointer needs no merge decision");

        assert_eq!(passthrough, side, "the pointer is carried through verbatim");
    }

    #[test]
    fn ensure_gitlinks_not_arbitrated_refuses_a_diverged_pointer() {
        let base = gitlink_side(&[("vendor", 7)]);
        let theirs = gitlink_side(&[("vendor", 8)]);

        let refusal = ensure_gitlinks_not_arbitrated("merge", &base, &base, &theirs)
            .expect_err("a moved submodule pointer needs a decision Libra cannot make");

        assert_eq!(refusal.path, PathBuf::from("vendor"));
        assert_eq!(refusal.operation, "merge");
        assert_eq!(
            refusal.to_string(),
            "merge would have to merge the submodule (gitlink) entry 'vendor': Libra does not support submodules"
        );
    }

    #[test]
    fn ensure_gitlinks_not_arbitrated_refuses_a_one_sided_pointer() {
        // Added on one side only, or deleted on one side only: both are
        // "any side differs from the base" (ADR-MG-01) and both are refused,
        // because resolving either would mean deciding about submodule content.
        let none = GitlinkEntries::new();
        let side = gitlink_side(&[("vendor", 7)]);

        let added = ensure_gitlinks_not_arbitrated("rebase", &none, &side, &none)
            .expect_err("an added submodule is still a decision");
        assert_eq!(added.operation, "rebase");
        assert_eq!(added.path, PathBuf::from("vendor"));

        let deleted = ensure_gitlinks_not_arbitrated("cherry-pick", &side, &side, &none)
            .expect_err("a removed submodule is still a decision");
        assert_eq!(deleted.operation, "cherry-pick");
        assert_eq!(deleted.path, PathBuf::from("vendor"));
    }

    #[test]
    fn ensure_gitlinks_not_arbitrated_reports_the_first_path_in_sorted_order() {
        // Deterministic reporting: with several diverged submodules the user
        // must see the same path on every run, not a hash-order pick.
        let base = gitlink_side(&[("b/sub", 1), ("a/sub", 1)]);
        let theirs = gitlink_side(&[("b/sub", 2), ("a/sub", 2)]);

        let refusal = ensure_gitlinks_not_arbitrated("merge", &base, &base, &theirs)
            .expect_err("both submodules diverged");

        assert_eq!(refusal.path, PathBuf::from("a/sub"));
    }
}

/// MG-02: the recursive virtual ancestor that a criss-cross history's several
/// merge bases are folded into.
///
/// Everything here is exercised without an object store: the fold's only
/// contact with one is loading commits, and each unit below drives the pieces
/// below that — the fold order, the depth ceiling, the depth-widened conflict
/// markers, and the pairwise ancestor merge itself (whose blobs are supplied
/// through [`VirtualBlobs`] exactly as a `--dry-run` supplies them).
#[cfg(test)]
mod recursive {
    use std::{
        collections::{HashMap, HashSet},
        path::PathBuf,
        sync::Arc,
    };

    use git_internal::{
        hash::ObjectHash,
        internal::object::{
            blob::Blob,
            commit::Commit,
            signature::{Signature, SignatureType},
            tree::TreeItemMode,
        },
    };

    use super::{
        GitlinkEntries, MAX_VIRTUAL_ANCESTOR_BASES, MAX_VIRTUAL_ANCESTOR_DEPTH, MAX_XDIFF_SIZE,
        MergeTreeEntry, PullMergeError, VIRTUAL_OURS_LABEL, VIRTUAL_THEIRS_LABEL, VirtualBlobs,
        conflict_marker_length_at_depth, ensure_virtual_ancestor_depth, fold_merge_bases,
        merge_bases_of_folded, merge_bases_of_folded_with, merge_input_exceeds_xdiff_size,
        merge_input_is_binary, merge_virtual_items, recorded_merge_base, virtual_base_fold_order,
        virtual_merged_mode,
    };

    fn oid(byte: u8) -> ObjectHash {
        ObjectHash::new(&[byte; 20])
    }

    /// Register `content` as a blob the fold can read back, and return the
    /// entry that names it.
    fn blob_entry(blobs: &mut VirtualBlobs, content: &str, mode: TreeItemMode) -> MergeTreeEntry {
        let blob = Blob::from_content_bytes(content.as_bytes().to_vec());
        blobs.insert(blob.id, blob.data.clone());
        MergeTreeEntry {
            hash: blob.id,
            mode,
        }
    }

    fn items(entries: &[(&str, MergeTreeEntry)]) -> HashMap<PathBuf, MergeTreeEntry> {
        entries
            .iter()
            .map(|(path, entry)| (PathBuf::from(path), *entry))
            .collect()
    }

    fn raw(blobs: &VirtualBlobs, items: &HashMap<PathBuf, MergeTreeEntry>, path: &str) -> Vec<u8> {
        let entry = items
            .get(&PathBuf::from(path))
            .unwrap_or_else(|| panic!("'{path}' present in the folded ancestor"));
        blobs
            .get(&entry.hash)
            .unwrap_or_else(|| panic!("'{path}' content available"))
            .clone()
    }

    fn content(
        blobs: &VirtualBlobs,
        items: &HashMap<PathBuf, MergeTreeEntry>,
        path: &str,
    ) -> String {
        let entry = items
            .get(&PathBuf::from(path))
            .unwrap_or_else(|| panic!("'{path}' present in the folded ancestor"));
        String::from_utf8(
            blobs
                .get(&entry.hash)
                .unwrap_or_else(|| panic!("'{path}' content available"))
                .clone(),
        )
        .expect("utf-8 content")
    }

    fn fold(
        base: &HashMap<PathBuf, MergeTreeEntry>,
        ours: &HashMap<PathBuf, MergeTreeEntry>,
        theirs: &HashMap<PathBuf, MergeTreeEntry>,
        depth: usize,
        blobs: &mut VirtualBlobs,
    ) -> HashMap<PathBuf, MergeTreeEntry> {
        merge_virtual_items(
            base,
            ours,
            theirs,
            depth,
            blobs,
            super::VirtualFold {
                persist: false,
                conflict_style: diffy::ConflictStyle::Merge,
                rename_config: &super::MergeRenameConfig::default(),
                default_driver: None,
                external_merge_runtime: Arc::new(super::ExternalMergeRuntime::default()),
            },
        )
        .expect("folding two ancestors never fails")
    }

    /// G2: the fold order is the bases' ascending hex id, so the same
    /// criss-cross always produces the same virtual ancestor — which is what
    /// lets `--restart` recompute one `maintenance gc` has reclaimed.
    #[test]
    fn folds_bases_in_ascending_hex_order() {
        let ordered = virtual_base_fold_order(&[oid(0xcc), oid(0x11), oid(0x77)]);
        assert_eq!(ordered, vec![oid(0x11), oid(0x77), oid(0xcc)]);
        assert_eq!(
            virtual_base_fold_order(&[oid(0x33), oid(0x33)]),
            vec![oid(0x33)],
            "a base listed twice is folded once"
        );
    }

    /// G3 + G4 at the PRODUCTION entry point: `fold_merge_bases` itself
    /// refuses one level past the ceiling, and at the ceiling it goes on to
    /// read — so the guard, not an accident of the fixture, is what stopped it.
    #[test]
    fn the_production_fold_refuses_one_level_past_the_ceiling() {
        let mut blobs = VirtualBlobs::new();
        let bases = [oid(1), oid(2)];
        let refused = fold_merge_bases(
            &bases,
            &GitlinkEntries::new(),
            MAX_VIRTUAL_ANCESTOR_DEPTH + 1,
            &mut blobs,
            super::VirtualFold {
                persist: false,
                conflict_style: diffy::ConflictStyle::Merge,
                rename_config: &super::MergeRenameConfig::default(),
                default_driver: None,
                external_merge_runtime: Arc::new(super::ExternalMergeRuntime::default()),
            },
        )
        .expect_err("one level past the ceiling is refused");
        assert!(matches!(refused, PullMergeError::VirtualAncestorTooDeep));

        let attempted = fold_merge_bases(
            &bases,
            &GitlinkEntries::new(),
            MAX_VIRTUAL_ANCESTOR_DEPTH,
            &mut blobs,
            super::VirtualFold {
                persist: false,
                conflict_style: diffy::ConflictStyle::Merge,
                rename_config: &super::MergeRenameConfig::default(),
                default_driver: None,
                external_merge_runtime: Arc::new(super::ExternalMergeRuntime::default()),
            },
        )
        .expect_err("these ids name no object");
        assert!(
            matches!(attempted, PullMergeError::ObjectLoad { .. }),
            "at the ceiling the fold proceeds to load the bases: {attempted}"
        );
    }

    /// The fold's WIDTH has a ceiling too: its work is quadratic in the number
    /// of bases (one merge-base walk per already-folded base, per step), and
    /// the depth ceiling says nothing about width. Enforced by the production
    /// fold before it loads anything.
    #[test]
    fn the_production_fold_refuses_more_bases_than_the_width_ceiling() {
        let mut blobs = VirtualBlobs::new();
        let too_many: Vec<ObjectHash> = (1..=MAX_VIRTUAL_ANCESTOR_BASES as u8 + 1)
            .map(oid)
            .collect();
        let refused = fold_merge_bases(
            &too_many,
            &GitlinkEntries::new(),
            1,
            &mut blobs,
            super::VirtualFold {
                persist: false,
                conflict_style: diffy::ConflictStyle::Merge,
                rename_config: &super::MergeRenameConfig::default(),
                default_driver: None,
                external_merge_runtime: Arc::new(super::ExternalMergeRuntime::default()),
            },
        )
        .expect_err("one base past the width ceiling is refused");
        assert!(
            matches!(refused, PullMergeError::VirtualAncestorTooWide { bases } if bases == MAX_VIRTUAL_ANCESTOR_BASES + 1)
        );

        let at_ceiling: Vec<ObjectHash> = (1..=MAX_VIRTUAL_ANCESTOR_BASES as u8).map(oid).collect();
        let attempted = fold_merge_bases(
            &at_ceiling,
            &GitlinkEntries::new(),
            1,
            &mut blobs,
            super::VirtualFold {
                persist: false,
                conflict_style: diffy::ConflictStyle::Merge,
                rename_config: &super::MergeRenameConfig::default(),
                default_driver: None,
                external_merge_runtime: Arc::new(super::ExternalMergeRuntime::default()),
            },
        )
        .expect_err("these ids name no object");
        assert!(
            matches!(attempted, PullMergeError::ObjectLoad { .. }),
            "at the ceiling the fold proceeds to load the bases: {attempted}"
        );
    }

    /// The nested collection point has the same ceiling: a fold that somehow
    /// carried more already-folded bases than the ceiling is refused before
    /// the first merge-base walk (the ids below name no object, so any walk
    /// would have failed differently).
    #[test]
    fn nested_candidate_collection_is_refused_past_the_width_ceiling() {
        let too_many: Vec<ObjectHash> = (1..=MAX_VIRTUAL_ANCESTOR_BASES as u8 + 1)
            .map(oid)
            .collect();
        let refused = merge_bases_of_folded(&too_many, &oid(0xee))
            .expect_err("refused before any graph walk");
        assert!(
            matches!(refused, PullMergeError::VirtualAncestorTooWide { bases } if bases == MAX_VIRTUAL_ANCESTOR_BASES + 1)
        );
    }

    /// The maximal filter must compare candidates contributed by different
    /// folded bases. Here `x` is a strict ancestor of `y`, so the virtual
    /// commit whose parents are `left` and `right` has exactly `y` as its merge
    /// base with `next`; retaining `x` would add redundant work to the next
    /// recursive fold.
    #[test]
    fn cross_part_domination_drops_the_strict_ancestor_candidate() {
        fn ancestors(
            parents: &HashMap<ObjectHash, Vec<ObjectHash>>,
            tip: ObjectHash,
        ) -> HashSet<ObjectHash> {
            let mut seen = HashSet::new();
            let mut stack = vec![tip];
            while let Some(commit) = stack.pop() {
                if seen.insert(commit) {
                    stack.extend(parents.get(&commit).into_iter().flatten().copied());
                }
            }
            seen
        }

        fn graph_merge_bases(
            parents: &HashMap<ObjectHash, Vec<ObjectHash>>,
            left: ObjectHash,
            right: ObjectHash,
        ) -> Vec<ObjectHash> {
            let left_ancestors = ancestors(parents, left);
            let right_ancestors = ancestors(parents, right);
            let common: Vec<ObjectHash> = left_ancestors
                .intersection(&right_ancestors)
                .copied()
                .collect();
            let mut maximal: Vec<ObjectHash> = common
                .iter()
                .copied()
                .filter(|candidate| {
                    !common.iter().any(|other| {
                        candidate != other && ancestors(parents, *other).contains(candidate)
                    })
                })
                .collect();
            maximal.sort_by_key(|id| id.to_string());
            maximal
        }

        // root <- x <- y; left descends only from x, while right and next
        // descend from y. Therefore mb(left,next)={x}, mb(right,next)={y}, and
        // the synthetic commit with parents left+right has mb(virtual,next)={y}.
        let root = oid(0x10);
        let x = oid(0x20);
        let y = oid(0x30);
        let left = oid(0x40);
        let right = oid(0x50);
        let next = oid(0x60);
        let virtual_commit = oid(0x70);
        let parents = HashMap::from([
            (root, vec![]),
            (x, vec![root]),
            (y, vec![x]),
            (left, vec![x]),
            (right, vec![y]),
            (next, vec![y]),
            (virtual_commit, vec![left, right]),
        ]);
        let mut ancestry_checks = Vec::new();

        let maximal = merge_bases_of_folded_with(
            &[left, right],
            &next,
            |base, tip| Ok(graph_merge_bases(&parents, *base, *tip)),
            |ancestor, descendant| {
                ancestry_checks.push((*ancestor, *descendant));
                Ok(ancestors(&parents, *descendant).contains(ancestor))
            },
        )
        .expect("the in-memory ancestry graph is valid");

        assert_eq!(
            maximal,
            graph_merge_bases(&parents, virtual_commit, next),
            "filtering the union of per-parent bases matches the equivalent virtual commit"
        );
        assert_eq!(maximal, vec![y]);
        assert_eq!(
            ancestry_checks,
            vec![(x, y), (y, x)],
            "both cross-part directions are considered; only the strict ancestor is dropped"
        );
    }

    /// G3 + G4: the recursion has a ceiling and reports it instead of running
    /// the stack out. Git recurses unbounded (`merge-ort.c:5313`); the ceiling
    /// is Libra's, because the fold recurses for real.
    #[test]
    fn refuses_to_nest_past_the_recursion_ceiling() {
        ensure_virtual_ancestor_depth(0).expect("the outer merge is always allowed");
        ensure_virtual_ancestor_depth(MAX_VIRTUAL_ANCESTOR_DEPTH)
            .expect("the last permitted level still folds");
        let refused = ensure_virtual_ancestor_depth(MAX_VIRTUAL_ANCESTOR_DEPTH + 1)
            .expect_err("one level past the ceiling is refused");
        assert!(matches!(refused, PullMergeError::VirtualAncestorTooDeep));
        assert_eq!(
            refused.to_string(),
            format!(
                "merging these branches needs a virtual common ancestor nested more than \
                 {MAX_VIRTUAL_ANCESTOR_DEPTH} levels deep, which Libra does not build"
            ),
            "the ceiling is named in the message the user sees"
        );
    }

    /// G5: Git widens the markers by two per recursion level (`merge-ort.c`
    /// passes `call_depth * 2` as `extra_marker_size`), so a conflict recorded
    /// inside an ancestor cannot be read as one the outer merge produced.
    #[test]
    fn conflict_markers_widen_two_per_recursion_level() {
        let plain: &[&[u8]] = &[b"a\n", b"b\n", b"c\n"];
        assert_eq!(conflict_marker_length_at_depth(plain, 0), 7);
        assert_eq!(conflict_marker_length_at_depth(plain, 1), 9);
        assert_eq!(conflict_marker_length_at_depth(plain, 2), 11);

        // Composed with Libra's content-driven bump, which keeps a marker run
        // distinguishable from the inputs themselves.
        let marker_like: &[&[u8]] = &[b"<<<<<<<<<<\n", b"b\n", b"c\n"];
        assert_eq!(conflict_marker_length_at_depth(marker_like, 0), 11);
        assert_eq!(
            conflict_marker_length_at_depth(marker_like, 1),
            13,
            "the depth widening applies on top of the content bump, never instead of it"
        );
    }

    /// G1: several ancestors fold pairwise, left to right, into ONE tree —
    /// Git's `merged_merge_bases = merge(merged_merge_bases, next)` loop
    /// (`merge-ort.c` `merge_ort_internal`, lines 5353-5385 at git@`3cb9185f6`).
    #[test]
    fn folds_three_ancestors_pairwise_into_one_tree() {
        let mut blobs = VirtualBlobs::new();
        let root = blob_entry(&mut blobs, "0\n", TreeItemMode::Blob);
        let first = items(&[("f", root), ("g", root), ("h", root)]);
        let second = items(&[
            ("f", blob_entry(&mut blobs, "second\n", TreeItemMode::Blob)),
            ("g", root),
            ("h", root),
        ]);
        let third = items(&[
            ("f", root),
            ("g", blob_entry(&mut blobs, "third\n", TreeItemMode::Blob)),
            ("h", root),
        ]);

        // (first ⊕ second) with `first` as their common ancestor, then that
        // result ⊕ third with `first` again.
        let folded = fold(&first, &first, &second, 1, &mut blobs);
        let folded = fold(&first, &folded, &third, 1, &mut blobs);

        assert_eq!(folded.len(), 3, "one tree, not one per base: {folded:?}");
        assert_eq!(content(&blobs, &folded, "f"), "second\n");
        assert_eq!(content(&blobs, &folded, "g"), "third\n");
        assert_eq!(content(&blobs, &folded, "h"), "0\n");
    }

    /// A conflict inside a virtual ancestor is recorded as content, not
    /// surfaced: the ancestor is a synthetic merge input, and Git records the
    /// conflicted text the same way. The markers carry the depth's width and
    /// Git's temporary-branch labels.
    #[test]
    fn records_conflicting_ancestor_content_with_labelled_widened_markers() {
        let mut blobs = VirtualBlobs::new();
        let base = items(&[("f", blob_entry(&mut blobs, "0\n", TreeItemMode::Blob))]);
        let ours = items(&[("f", blob_entry(&mut blobs, "a\n", TreeItemMode::Blob))]);
        let theirs = items(&[("f", blob_entry(&mut blobs, "b\n", TreeItemMode::Blob))]);

        let folded = fold(&base, &ours, &theirs, 1, &mut blobs);
        let text = content(&blobs, &folded, "f");

        assert_eq!(
            text,
            format!(
                "<<<<<<<<< {VIRTUAL_OURS_LABEL}\na\n=========\nb\n>>>>>>>>> {VIRTUAL_THEIRS_LABEL}\n"
            ),
            "nine-character markers (7 + 2 × depth 1) labelled the way Git labels a \
             virtual-ancestor merge"
        );
    }

    /// A blob the fold merges CLEANLY is just as absent from the object store
    /// as a conflicted one when nothing may be written, and the outer merge
    /// loads the virtual ancestor's content BY OBJECT ID — so a `--dry-run`
    /// has to keep it addressable in memory too, not only the conflicted ones.
    #[test]
    fn cleanly_merged_ancestor_content_stays_addressable_without_being_written() {
        let mut blobs = VirtualBlobs::new();
        let base = items(&[(
            "m",
            blob_entry(&mut blobs, "1\n2\n3\n4\n5\n", TreeItemMode::Blob),
        )]);
        let ours = items(&[(
            "m",
            blob_entry(&mut blobs, "one\n2\n3\n4\n5\n", TreeItemMode::Blob),
        )]);
        let theirs = items(&[(
            "m",
            blob_entry(&mut blobs, "1\n2\n3\n4\nfive\n", TreeItemMode::Blob),
        )]);

        let folded = fold(&base, &ours, &theirs, 1, &mut blobs);
        let entry = folded
            .get(&PathBuf::from("m"))
            .expect("the two sides merge cleanly into one entry");
        assert!(
            blobs.contains_key(&entry.hash),
            "the auto-merged ancestor content was neither written nor cached, so the outer \
             merge could not read it back"
        );
        assert_eq!(content(&blobs, &folded, "m"), "one\n2\n3\n4\nfive\n");
    }

    /// Git's rule for a change/delete inside a virtual ancestor: there is no
    /// midpoint between "changed" and "gone", so the ancestor keeps the base
    /// version (`merge-ort.c` `process_entry`, lines 4374-4381 at
    /// git@`3cb9185f6`).
    #[test]
    fn change_delete_inside_an_ancestor_keeps_the_base_version() {
        let mut blobs = VirtualBlobs::new();
        let base_entry = blob_entry(&mut blobs, "0\n", TreeItemMode::Blob);
        let base = items(&[("f", base_entry)]);
        let ours = items(&[("f", blob_entry(&mut blobs, "a\n", TreeItemMode::Blob))]);
        let theirs = items(&[]);

        let folded = fold(&base, &ours, &theirs, 1, &mut blobs);
        assert_eq!(folded.get(&PathBuf::from("f")), Some(&base_entry));

        // Symmetric: the deleting side may be either one.
        let folded = fold(&base, &theirs, &ours, 1, &mut blobs);
        assert_eq!(folded.get(&PathBuf::from("f")), Some(&base_entry));
    }

    /// Binary content has no line-level midpoint. Git's `ll_binary_merge`
    /// steals the ORIGINAL buffer for a virtual ancestor, so a conflicting
    /// binary keeps the base's content — never a side's.
    #[test]
    fn binary_conflict_inside_an_ancestor_keeps_the_original_content() {
        let mut blobs = VirtualBlobs::new();
        let binary = |blobs: &mut VirtualBlobs, byte: u8| {
            let blob = Blob::from_content_bytes(vec![0xff, byte, 0x00]);
            blobs.insert(blob.id, blob.data.clone());
            MergeTreeEntry {
                hash: blob.id,
                mode: TreeItemMode::Blob,
            }
        };
        let base_entry = binary(&mut blobs, 1);
        let base = items(&[("f", base_entry)]);
        let ours = items(&[("f", binary(&mut blobs, 2))]);
        let theirs = items(&[("f", binary(&mut blobs, 3))]);

        let folded = fold(&base, &ours, &theirs, 1, &mut blobs);
        assert_eq!(folded.get(&PathBuf::from("f")), Some(&base_entry));
    }

    /// With no original at all, Git's binary rule steals an EMPTY buffer
    /// (`read_mmblob` of a null oid), so the ancestor records the empty blob.
    /// Recording nothing instead would turn the outer merge's add/add into a
    /// one-sided add and silently drop a side.
    #[test]
    fn binary_add_add_inside_an_ancestor_records_the_empty_blob() {
        let mut blobs = VirtualBlobs::new();
        let binary = |blobs: &mut VirtualBlobs, byte: u8| {
            let blob = Blob::from_content_bytes(vec![0x00, byte]);
            blobs.insert(blob.id, blob.data.clone());
            MergeTreeEntry {
                hash: blob.id,
                mode: TreeItemMode::Blob,
            }
        };
        let base = items(&[]);
        let ours = items(&[("f", binary(&mut blobs, 2))]);
        let theirs = items(&[("f", binary(&mut blobs, 3))]);

        let folded = fold(&base, &ours, &theirs, 1, &mut blobs);
        assert_eq!(
            raw(&blobs, &folded, "f"),
            Vec::<u8>::new(),
            "an add/add binary ancestor is the empty blob, not one of the sides"
        );
    }

    /// The other half of Git's rule: `ll_xdl_merge` refuses inputs past
    /// `MAX_XDIFF_SIZE` (1023 MiB) regardless of content. Pinned on the length
    /// alone — allocating a gibibyte in a unit test is not an option, and the
    /// predicate is a pure comparison.
    #[test]
    fn inputs_past_gits_xdiff_size_limit_are_binary() {
        assert_eq!(
            MAX_XDIFF_SIZE, 1_072_693_248,
            "1023 MiB, as in xdiff/xdiff.h"
        );
        assert!(!merge_input_exceeds_xdiff_size(MAX_XDIFF_SIZE));
        assert!(merge_input_exceeds_xdiff_size(MAX_XDIFF_SIZE + 1));
        assert!(
            !merge_input_is_binary(b"small text\n"),
            "ordinary text stays on the line-level path"
        );
    }

    /// Binary-ness follows Git's `buffer_is_binary` — a NUL byte in the first
    /// 8000 — not UTF-8 validity. Valid UTF-8 carrying a NUL is binary…
    #[test]
    fn utf8_content_containing_a_nul_is_binary_like_git() {
        let mut blobs = VirtualBlobs::new();
        let entry = |blobs: &mut VirtualBlobs, text: &str| {
            let blob = Blob::from_content_bytes(text.as_bytes().to_vec());
            blobs.insert(blob.id, blob.data.clone());
            MergeTreeEntry {
                hash: blob.id,
                mode: TreeItemMode::Blob,
            }
        };
        let base_entry = entry(&mut blobs, "a\u{0}b\n");
        let base = items(&[("f", base_entry)]);
        let ours = items(&[("f", entry(&mut blobs, "a\u{0}ours\n"))]);
        let theirs = items(&[("f", entry(&mut blobs, "a\u{0}theirs\n"))]);

        let folded = fold(&base, &ours, &theirs, 1, &mut blobs);
        assert_eq!(
            folded.get(&PathBuf::from("f")),
            Some(&base_entry),
            "valid UTF-8 with a NUL byte is binary to Git, so the original is kept"
        );
    }

    /// …and content that is not valid UTF-8 but carries no NUL is TEXT, which
    /// must survive the merge byte for byte (a lossy string round-trip would
    /// rewrite those bytes as U+FFFD).
    #[test]
    fn non_utf8_content_without_a_nul_merges_as_text_like_git() {
        let mut blobs = VirtualBlobs::new();
        let entry = |blobs: &mut VirtualBlobs, bytes: &[u8]| {
            let blob = Blob::from_content_bytes(bytes.to_vec());
            blobs.insert(blob.id, blob.data.clone());
            MergeTreeEntry {
                hash: blob.id,
                mode: TreeItemMode::Blob,
            }
        };
        let base = items(&[("f", entry(&mut blobs, b"\xffkeep\n0\n"))]);
        let ours = items(&[("f", entry(&mut blobs, b"\xffkeep\nours\n"))]);
        let theirs = items(&[("f", entry(&mut blobs, b"\xffkeep\ntheirs\n"))]);

        let folded = fold(&base, &ours, &theirs, 1, &mut blobs);
        let merged = raw(&blobs, &folded, "f");
        assert!(
            merged.starts_with(b"\xffkeep\n"),
            "the shared non-UTF-8 line survives verbatim: {merged:?}"
        );
        assert!(
            merged.windows(9).any(|window| window == b"<<<<<<<<<"),
            "the diverging line still conflicts with depth-widened markers: {merged:?}"
        );
    }

    /// Symlinks are not content-merged: `merge-ort.c` keeps the ORIGINAL under
    /// `call_depth`, which is NOTHING when there is no original.
    #[test]
    fn symlink_conflict_inside_an_ancestor_keeps_the_original() {
        let mut blobs = VirtualBlobs::new();
        let link = |blobs: &mut VirtualBlobs, target: &str| {
            let blob = Blob::from_content_bytes(target.as_bytes().to_vec());
            blobs.insert(blob.id, blob.data.clone());
            MergeTreeEntry {
                hash: blob.id,
                mode: TreeItemMode::Link,
            }
        };
        let base_entry = link(&mut blobs, "base");
        let ours = items(&[("l", link(&mut blobs, "ours"))]);
        let theirs = items(&[("l", link(&mut blobs, "theirs"))]);

        let folded = fold(&items(&[("l", base_entry)]), &ours, &theirs, 1, &mut blobs);
        assert_eq!(folded.get(&PathBuf::from("l")), Some(&base_entry));

        let folded = fold(&items(&[]), &ours, &theirs, 1, &mut blobs);
        assert_eq!(
            folded.get(&PathBuf::from("l")),
            None,
            "no original means the ancestor simply does not have the path"
        );
    }

    /// Two sides of DIFFERENT kinds are not a content merge at all in Git
    /// (`handle_content_merge` asserts equal `S_IFMT`); the ancestor keeps the
    /// original.
    #[test]
    fn mixed_kinds_inside_an_ancestor_keep_the_original() {
        let mut blobs = VirtualBlobs::new();
        let entry = |blobs: &mut VirtualBlobs, text: &str, mode| {
            let blob = Blob::from_content_bytes(text.as_bytes().to_vec());
            blobs.insert(blob.id, blob.data.clone());
            MergeTreeEntry {
                hash: blob.id,
                mode,
            }
        };
        let base_entry = entry(&mut blobs, "0\n", TreeItemMode::Blob);
        let ours = items(&[("p", entry(&mut blobs, "ours\n", TreeItemMode::Blob))]);
        let theirs = items(&[("p", entry(&mut blobs, "theirs", TreeItemMode::Link))]);

        let folded = fold(&items(&[("p", base_entry)]), &ours, &theirs, 1, &mut blobs);
        assert_eq!(folded.get(&PathBuf::from("p")), Some(&base_entry));
    }

    /// Git's mode rule for a conflicted content merge (`merge-ort.c`
    /// `handle_content_merge`, lines 2211-2217 at git@`3cb9185f6`).
    #[test]
    fn conflicted_ancestor_mode_follows_gits_rule() {
        let base = MergeTreeEntry {
            hash: oid(1),
            mode: TreeItemMode::Blob,
        };
        let ours_plain = MergeTreeEntry {
            hash: oid(2),
            mode: TreeItemMode::Blob,
        };
        let theirs_exec = MergeTreeEntry {
            hash: oid(3),
            mode: TreeItemMode::BlobExecutable,
        };
        // Ours kept the base's mode → take theirs.
        assert_eq!(
            virtual_merged_mode(Some(&base), &ours_plain, &theirs_exec),
            TreeItemMode::BlobExecutable
        );
        // Both sides changed the mode the same way → take theirs (== ours).
        assert_eq!(
            virtual_merged_mode(Some(&base), &theirs_exec, &theirs_exec),
            TreeItemMode::BlobExecutable
        );
        // Both changed it, differently → keep ours.
        let ours_exec = MergeTreeEntry {
            hash: oid(4),
            mode: TreeItemMode::BlobExecutable,
        };
        let theirs_link = MergeTreeEntry {
            hash: oid(5),
            mode: TreeItemMode::Tree,
        };
        assert_eq!(
            virtual_merged_mode(Some(&base), &ours_exec, &theirs_link),
            TreeItemMode::BlobExecutable
        );
    }

    /// G7: a virtual ancestor is never written into `merge-state.json`, so it
    /// never becomes a GC root (ADR-MG-04). A single real base still is.
    #[test]
    fn only_a_single_real_base_is_recorded_in_the_merge_state() {
        let signature = |signature_type| Signature {
            signature_type,
            name: "Libra".to_string(),
            email: "test@libra.invalid".to_string(),
            timestamp: 0,
            timezone: "+0000".to_string(),
        };
        let commit = |byte: u8| {
            Commit::new(
                signature(SignatureType::Author),
                signature(SignatureType::Committer),
                oid(byte),
                Vec::new(),
                "base",
            )
        };
        let one = commit(1);
        let two = commit(2);

        assert_eq!(recorded_merge_base(&[]), None, "unrelated histories");
        assert_eq!(
            recorded_merge_base(std::slice::from_ref(&one)),
            Some(one.id)
        );
        assert_eq!(
            recorded_merge_base(&[one, two]),
            None,
            "a criss-cross merge's base is virtual and must not be rooted"
        );
    }
}

/// MG-03: the incremental (directory-pruning) tree merge.
///
/// Every test runs against an in-memory [`TreeSource`] that COUNTS reads, so the
/// pruning guarantees are measured rather than assumed, and against the
/// flattening path on the same trees, so the two paths are proven to decide the
/// same thing.
#[cfg(test)]
mod tree {
    use std::{
        collections::{HashMap, HashSet},
        path::{Path, PathBuf},
    };

    use git_internal::{
        hash::ObjectHash,
        internal::object::{
            ObjectTrait,
            tree::{Tree, TreeItem, TreeItemMode},
        },
    };

    use super::{
        GitlinkEntries, IncrementalMergeResult, MergeTreeEntry, PullMergeError, TreeMergeContext,
        TreeSource, VirtualBlobs, incremental_merge_trees, incremental_tree_walk_enabled_for,
        merge_tree_items, split_gitlink_entries,
    };

    /// An in-memory object graph of trees that counts OBJECT-STORE reads the
    /// way the production `ObjectStoreTrees` incurs them: the first read of an
    /// id is a read, later ones come from the per-merge cache. (The gate walk
    /// and the merge walk open the same directories; counting cache hits would
    /// measure the fixture, not the store.)
    #[derive(Default)]
    struct CountingTrees {
        trees: HashMap<ObjectHash, Tree>,
        reads: usize,
        read_ids: Vec<ObjectHash>,
        seen: HashSet<ObjectHash>,
    }

    impl TreeSource for CountingTrees {
        fn tree(&mut self, id: &ObjectHash) -> Result<Tree, PullMergeError> {
            if self.seen.insert(*id) {
                self.reads += 1;
                self.read_ids.push(*id);
            }
            self.trees
                .get(id)
                .cloned()
                .ok_or_else(|| PullMergeError::TreeLoad {
                    tree_id: id.to_string(),
                    detail: "not in the synthetic graph".to_string(),
                })
        }
    }

    /// A directory described as nested leaves; `Dir` builds the tree objects
    /// bottom-up into a [`CountingTrees`] and returns the root id.
    #[derive(Clone)]
    enum Node {
        Blob(u8),
        /// A blob with an arbitrary id (content never loaded).
        Id(ObjectHash),
        Exec(u8),
        Link(u8),
        Gitlink(u8),
        Dir(Vec<(String, Node)>),
    }

    fn blob_id(byte: u8) -> ObjectHash {
        ObjectHash::new(&[byte; 20])
    }

    fn build(graph: &mut CountingTrees, node: &Node) -> (ObjectHash, TreeItemMode) {
        match node {
            Node::Blob(byte) => (blob_id(*byte), TreeItemMode::Blob),
            Node::Id(id) => (*id, TreeItemMode::Blob),
            Node::Exec(byte) => (blob_id(*byte), TreeItemMode::BlobExecutable),
            Node::Link(byte) => (blob_id(*byte), TreeItemMode::Link),
            Node::Gitlink(byte) => (blob_id(*byte), TreeItemMode::Commit),
            Node::Dir(children) => {
                let items: Vec<TreeItem> = children
                    .iter()
                    .map(|(name, child)| {
                        let (id, mode) = build(graph, child);
                        TreeItem::new(mode, id, name.clone())
                    })
                    .collect();
                let tree = if items.is_empty() {
                    let id = ObjectHash::from_type_and_data(
                        git_internal::internal::object::types::ObjectType::Tree,
                        &[],
                    );
                    Tree::from_bytes(&[], id).expect("empty tree")
                } else {
                    Tree::from_tree_items(items).expect("tree")
                };
                let id = tree.id;
                graph.trees.entry(id).or_insert(tree);
                (id, TreeItemMode::Tree)
            }
        }
    }

    fn dir(children: &[(&str, Node)]) -> Node {
        Node::Dir(
            children
                .iter()
                .map(|(name, node)| (name.to_string(), node.clone()))
                .collect(),
        )
    }

    /// The flattening path on the same synthetic graph: every leaf of every
    /// side, through `merge_tree_items`.
    fn leaves(
        graph: &mut CountingTrees,
        root: ObjectHash,
    ) -> Vec<(PathBuf, ObjectHash, TreeItemMode)> {
        let mut out = Vec::new();
        let mut stack = vec![(PathBuf::new(), root)];
        while let Some((prefix, id)) = stack.pop() {
            let tree = graph.tree(&id).expect("tree");
            for item in &tree.tree_items {
                let path = prefix.join(&item.name);
                if item.mode == TreeItemMode::Tree {
                    // Like `flat_items_with_empty_dirs`: an empty subtree is
                    // kept as a directory marker for the D/F decision.
                    if graph.tree(&item.id).expect("tree").tree_items.is_empty() {
                        out.push((path, item.id, TreeItemMode::Tree));
                    } else {
                        stack.push((path, item.id));
                    }
                } else {
                    out.push((path, item.id, item.mode));
                }
            }
        }
        out
    }

    /// In-memory content for every fake blob id the fixtures use, so both paths
    /// run their line-level content merges without an object store.
    fn fixture_blobs() -> VirtualBlobs {
        (1..=255u8)
            .map(|byte| (blob_id(byte), format!("content {byte}\n").into_bytes()))
            .collect()
    }

    /// What the flattening path produced: merged leaves, sorted conflict paths,
    /// pass-through gitlinks.
    type FlatOutcome = (
        HashMap<PathBuf, MergeTreeEntry>,
        Vec<PathBuf>,
        GitlinkEntries,
    );

    fn flat_merge(
        graph: &mut CountingTrees,
        base: Option<ObjectHash>,
        ours: ObjectHash,
        theirs: ObjectHash,
    ) -> Result<FlatOutcome, PullMergeError> {
        let (base_items, base_gl) = match base {
            Some(id) => split_gitlink_entries(leaves(graph, id)),
            None => (HashMap::new(), GitlinkEntries::new()),
        };
        let (our_items, our_gl) = split_gitlink_entries(leaves(graph, ours));
        let (their_items, their_gl) = split_gitlink_entries(leaves(graph, theirs));
        let passthrough =
            super::ensure_gitlinks_not_arbitrated("merge", &base_gl, &our_gl, &their_gl)
                .map_err(PullMergeError::GitlinkUnsupported)?;
        let mut blobs = fixture_blobs();
        let result = merge_tree_items(
            &base_items,
            &our_items,
            &their_items,
            &mut TreeMergeContext::top_level(false, None, None, &mut blobs),
        )?;
        let mut conflicts: Vec<PathBuf> = result.conflicts.into_iter().map(|(p, _)| p).collect();
        conflicts.sort();
        Ok((result.merged_items, conflicts, passthrough))
    }

    fn incremental(
        graph: &mut CountingTrees,
        base: Option<ObjectHash>,
        ours: ObjectHash,
        theirs: ObjectHash,
    ) -> Result<(IncrementalMergeResult, GitlinkEntries), PullMergeError> {
        let mut blobs = fixture_blobs();
        let (mut out, passthrough) = incremental_merge_trees(
            graph,
            base,
            ours,
            theirs,
            &mut TreeMergeContext::top_level(false, None, None, &mut blobs),
            false,
        )?;
        // Production settles the collisions after the rename fix-up; this
        // helper detects no renames, so it settles them right away.
        let candidates = std::mem::take(&mut out.df_candidates);
        let mut files_changed = out.changed_paths;
        super::settle_incremental_df_conflicts(
            graph,
            &mut out.merged,
            &mut out.conflicts,
            candidates,
            &mut files_changed,
        )?;
        out.changed_paths = files_changed;
        Ok((out, passthrough))
    }

    /// The same walk with MG-05 candidate collection ON, so a test can measure
    /// what rename detection costs on top of MG-03's pruned reads.
    fn incremental_collecting_renames(
        graph: &mut CountingTrees,
        base: Option<ObjectHash>,
        ours: ObjectHash,
        theirs: ObjectHash,
    ) -> Result<IncrementalMergeResult, PullMergeError> {
        let mut blobs = fixture_blobs();
        let (out, _) = incremental_merge_trees(
            graph,
            base,
            ours,
            theirs,
            &mut TreeMergeContext::top_level(false, None, None, &mut blobs),
            true,
        )?;
        Ok(out)
    }

    /// Expand adopted subtrees so the two paths' results can be compared leaf
    /// for leaf.
    fn expanded(
        graph: &mut CountingTrees,
        mut merged: HashMap<PathBuf, MergeTreeEntry>,
    ) -> HashMap<PathBuf, MergeTreeEntry> {
        super::expand_adopted_subtrees(graph, &mut merged).expect("expand");
        merged
    }

    /// MG-04, verified against real `git merge` (git@3cb9185f6) on crafted
    /// trees: an empty-only subtree is "in the way" of the file at the same
    /// path only when the merge base had NOTHING there — Git defers such a new
    /// directory and adopts its tree verbatim. With the base holding a file at
    /// that path, Git traverses the directory, finds no file and leaves the
    /// file where it is (plain modify/delete). Both walks must agree.
    #[test]
    fn an_empty_subtree_is_in_the_way_only_when_the_base_had_nothing_there() {
        let mut graph = CountingTrees::default();
        let dir_side = dir(&[
            ("keep.txt", Node::Blob(1)),
            ("foo", dir(&[("bar", dir(&[]))])),
        ]);
        let theirs = build(&mut graph, &dir_side).0;

        // (1) base has NOTHING at `foo`; ours adds the file → relocation.
        let base = build(&mut graph, &dir(&[("keep.txt", Node::Blob(1))])).0;
        let ours = build(
            &mut graph,
            &dir(&[("keep.txt", Node::Blob(1)), ("foo", Node::Blob(2))]),
        )
        .0;
        let (flat_merged, flat_conflicts, _) =
            flat_merge(&mut graph, Some(base), ours, theirs).expect("flat");
        let (inc, _) = incremental(&mut graph, Some(base), ours, theirs).expect("incremental");
        assert_eq!(flat_conflicts, vec![PathBuf::from("foo")]);
        let mut inc_conflicts: Vec<PathBuf> =
            inc.conflicts.iter().map(|(p, _)| p.clone()).collect();
        inc_conflicts.sort();
        assert_eq!(inc_conflicts, vec![PathBuf::from("foo")]);
        assert!(matches!(
            inc.conflicts[0].1,
            super::ConflictKind::FileDirectory {
                file_side: super::MergeSide::Ours,
                base_file: None,
                modify_delete: false,
                ..
            }
        ));
        assert!(!flat_merged.contains_key(Path::new("foo")));
        assert!(!expanded(&mut graph, inc.merged).contains_key(Path::new("foo")));

        // (2) base HAS a file at `foo` and ours edits it → no relocation, the
        // ordinary modify/delete conflict stays at `foo`.
        let base = build(
            &mut graph,
            &dir(&[("keep.txt", Node::Blob(1)), ("foo", Node::Blob(3))]),
        )
        .0;
        let (flat_merged, flat_conflicts, _) =
            flat_merge(&mut graph, Some(base), ours, theirs).expect("flat");
        let (inc, _) = incremental(&mut graph, Some(base), ours, theirs).expect("incremental");
        assert_eq!(flat_conflicts, vec![PathBuf::from("foo")]);
        assert!(matches!(
            inc.conflicts[0].1,
            super::ConflictKind::OursModifiedTheirsDeleted { .. }
        ));
        assert!(!flat_merged.contains_key(Path::new("foo")));
        assert!(!expanded(&mut graph, inc.merged).contains_key(Path::new("foo")));

        // (3) an empty directory at the file's OWN path is not beneath it:
        // clean, the file survives on both walks.
        let base = build(&mut graph, &dir(&[("keep.txt", Node::Blob(1))])).0;
        let theirs_empty = build(
            &mut graph,
            &dir(&[("keep.txt", Node::Blob(1)), ("foo", dir(&[]))]),
        )
        .0;
        let (flat_merged, flat_conflicts, _) =
            flat_merge(&mut graph, Some(base), ours, theirs_empty).expect("flat");
        let (inc, _) =
            incremental(&mut graph, Some(base), ours, theirs_empty).expect("incremental");
        assert!(flat_conflicts.is_empty() && inc.conflicts.is_empty());
        assert!(flat_merged.contains_key(Path::new("foo")));
        assert!(expanded(&mut graph, inc.merged).contains_key(Path::new("foo")));
    }

    /// A deep subtree shared by base, ours and theirs, plus one file each side
    /// touches somewhere else.
    fn deep(byte: u8) -> Node {
        dir(&[(
            "level1",
            dir(&[(
                "level2",
                dir(&[("level3", dir(&[("leaf.txt", Node::Blob(byte))]))]),
            )]),
        )])
    }

    /// G1 + G2: a subtree that equals the base on one side is adopted from the
    /// other side WITHOUT opening it — no tree object inside it is read, hence
    /// no blob inside it can be. Here `shared/` is identical on all three sides
    /// and `moved/` equals the base on ours while theirs rewrote a leaf deep
    /// inside: the walk must open neither `shared/` nor ours' `moved/`.
    #[test]
    fn pruned_subtrees_are_not_read() {
        let mut graph = CountingTrees::default();
        let shared = deep(1);
        let (base, _) = build(
            &mut graph,
            &dir(&[
                ("shared", shared.clone()),
                ("moved", deep(2)),
                ("top.txt", Node::Blob(3)),
            ]),
        );
        let (ours, _) = build(
            &mut graph,
            &dir(&[
                ("shared", shared.clone()),
                ("moved", deep(2)),
                ("top.txt", Node::Blob(4)),
            ]),
        );
        let (theirs, _) = build(
            &mut graph,
            &dir(&[
                ("shared", shared),
                ("moved", deep(5)),
                ("top.txt", Node::Blob(3)),
            ]),
        );
        let shared_trees: HashSet<ObjectHash> = {
            let mut g = CountingTrees::default();
            build(&mut g, &deep(1));
            g.trees.keys().copied().collect()
        };
        let ours_moved_trees: HashSet<ObjectHash> = {
            let mut g = CountingTrees::default();
            build(&mut g, &deep(2));
            g.trees.keys().copied().collect()
        };

        graph.reads = 0;
        graph.read_ids.clear();
        graph.seen.clear();
        let (result, _) = incremental(&mut graph, Some(base), ours, theirs).expect("merge");

        assert!(
            !graph.read_ids.iter().any(|id| shared_trees.contains(id)),
            "a subtree all three sides agree on is never opened: {:?}",
            graph.read_ids
        );
        // The adopted subtree costs the gate (and the pruned files_changed diff,
        // which hits the cache) one read per side per differing level — `moved/
        // level1/level2/level3` differs at every level here, so four levels on
        // two distinct sides — and never the shared `shared/` subtree.
        assert_eq!(
            result.merged.get(Path::new("top.txt")),
            Some(&MergeTreeEntry {
                hash: blob_id(4),
                mode: TreeItemMode::Blob
            }),
            "ours changed top.txt, theirs did not"
        );
        assert_eq!(
            result.changed_paths, 1,
            "exactly moved/…/leaf.txt changed relative to ours"
        );
        // Three roots + moved on both differing sides down four levels = 3 + 8.
        assert!(
            graph.reads <= 3 + 8,
            "reads are confined to the roots and the differing path: {} reads of {:?}",
            graph.reads,
            graph.read_ids
        );
        assert!(
            graph
                .read_ids
                .iter()
                .filter(|id| ours_moved_trees.contains(id))
                .count()
                <= 4,
            "ours' copy of moved/ is opened at most once per differing level (the pruned diff), \
             never re-read: {:?}",
            graph.read_ids
        );
    }

    /// When theirs equals the base and ours changed a deep leaf, ours' subtree is
    /// the result — adopted verbatim, so the MERGE walk opens nothing under
    /// `moved/` (there is nothing to count: the result IS ours). What does open
    /// it is the ADR-MG-01 gate, which must look along the changed chain on both
    /// sides for a pointer ours could have added or moved: one read per
    /// differing level per distinct tree, never more (the walk hits the cache).
    /// No blob is read anywhere.
    #[test]
    fn a_subtree_only_we_changed_is_adopted_with_only_the_gates_reads() {
        let mut graph = CountingTrees::default();
        let (base, _) = build(
            &mut graph,
            &dir(&[("moved", deep(2)), ("top.txt", Node::Blob(3))]),
        );
        let (ours, _) = build(
            &mut graph,
            &dir(&[("moved", deep(5)), ("top.txt", Node::Blob(3))]),
        );
        let (theirs, _) = build(
            &mut graph,
            &dir(&[("moved", deep(2)), ("top.txt", Node::Blob(9))]),
        );
        graph.reads = 0;
        graph.read_ids.clear();
        graph.seen.clear();
        let (result, _) = incremental(&mut graph, Some(base), ours, theirs).expect("merge");
        // Roots: 3 distinct. `moved/` chain: base's copy (== theirs') and ours'
        // copy, four levels each, opened once by the gate = 8.
        assert_eq!(
            graph.reads,
            3 + 8,
            "roots plus one read per side per differing level, nothing else: {:?}",
            graph.read_ids
        );
        assert!(
            matches!(result.merged.get(Path::new("moved")), Some(entry) if entry.mode == TreeItemMode::Tree),
            "moved/ is adopted as a whole subtree"
        );
        assert_eq!(
            result.changed_paths, 1,
            "top.txt is the only path that differs from ours"
        );
    }

    /// G3: the two paths decide every leaf identically — merged entries and the
    /// conflict set — across the shapes that matter: unchanged, one-sided,
    /// same-change, add/add, modify/delete, mode-only, symlink, and a
    /// directory replaced by a file.
    #[test]
    fn incremental_and_flattening_paths_agree() {
        let mut graph = CountingTrees::default();
        let (base, _) = build(
            &mut graph,
            &dir(&[
                ("same", deep(1)),
                (
                    "ours_only",
                    dir(&[("a.txt", Node::Blob(2)), ("b.txt", Node::Blob(3))]),
                ),
                ("theirs_only", dir(&[("c.txt", Node::Blob(4))])),
                ("both_same", dir(&[("d.txt", Node::Blob(5))])),
                ("conflict.txt", Node::Blob(6)),
                ("mode.txt", Node::Blob(7)),
                ("link", Node::Link(8)),
                ("gone_dir", dir(&[("x.txt", Node::Blob(9))])),
                ("del_mod.txt", Node::Blob(10)),
            ]),
        );
        let (ours, _) = build(
            &mut graph,
            &dir(&[
                ("same", deep(1)),
                (
                    "ours_only",
                    dir(&[("a.txt", Node::Blob(20)), ("b.txt", Node::Blob(3))]),
                ),
                ("theirs_only", dir(&[("c.txt", Node::Blob(4))])),
                ("both_same", dir(&[("d.txt", Node::Blob(50))])),
                ("conflict.txt", Node::Blob(60)),
                ("mode.txt", Node::Exec(7)),
                ("link", Node::Link(80)),
                ("gone_dir", Node::Blob(90)),
                ("added.txt", Node::Blob(11)),
            ]),
        );
        let (theirs, _) = build(
            &mut graph,
            &dir(&[
                ("same", deep(1)),
                (
                    "ours_only",
                    dir(&[("a.txt", Node::Blob(2)), ("b.txt", Node::Blob(3))]),
                ),
                (
                    "theirs_only",
                    dir(&[("c.txt", Node::Blob(40)), ("new.txt", Node::Blob(41))]),
                ),
                ("both_same", dir(&[("d.txt", Node::Blob(50))])),
                ("conflict.txt", Node::Blob(61)),
                ("mode.txt", Node::Blob(7)),
                ("link", Node::Link(8)),
                ("gone_dir", dir(&[("x.txt", Node::Blob(9))])),
                ("del_mod.txt", Node::Blob(100)),
                ("added.txt", Node::Blob(12)),
                // An EMPTY directory inside a subtree theirs added: the flattening
                // path has no leaf to emit for it; the incremental path adopts the
                // subtree verbatim (empty tree and all). Leaves agree; the written
                // tree ids may not — Git's own "adopt as-is" semantics, documented
                // as the one known difference (G3 is leaf-level).
                (
                    "theirs_added",
                    dir(&[("empty", dir(&[])), ("z.txt", Node::Blob(13))]),
                ),
            ]),
        );

        let (flat_merged, flat_conflicts, _) =
            flat_merge(&mut graph, Some(base), ours, theirs).expect("flat");
        let (walk, _) = incremental(&mut graph, Some(base), ours, theirs).expect("incremental");
        let mut walk_conflicts: Vec<PathBuf> =
            walk.conflicts.iter().map(|(p, _)| p.clone()).collect();
        walk_conflicts.sort();
        assert_eq!(walk_conflicts, flat_conflicts, "same conflict set");
        let walk_merged = expanded(&mut graph, walk.merged);
        assert_eq!(walk_merged, flat_merged, "same resolution for every leaf");
        assert_eq!(
            walk.changed_paths,
            super::count_item_map_changes(
                &split_gitlink_entries(leaves(&mut graph, ours)).0,
                &flat_merged
            ),
            "same files_changed"
        );
        assert!(flat_conflicts.contains(&PathBuf::from("conflict.txt")));
        assert!(flat_conflicts.contains(&PathBuf::from("del_mod.txt")));
    }

    /// No base (unrelated histories): the empty virtual base gives the same
    /// answers on both paths, including the add/add conflict.
    #[test]
    fn incremental_and_flattening_paths_agree_without_a_base() {
        let mut graph = CountingTrees::default();
        let (ours, _) = build(
            &mut graph,
            &dir(&[
                ("a.txt", Node::Blob(1)),
                ("both.txt", Node::Blob(2)),
                ("d", deep(3)),
            ]),
        );
        let (theirs, _) = build(
            &mut graph,
            &dir(&[
                ("b.txt", Node::Blob(4)),
                ("both.txt", Node::Blob(5)),
                ("d", deep(3)),
            ]),
        );
        let (flat_merged, flat_conflicts, _) =
            flat_merge(&mut graph, None, ours, theirs).expect("flat");
        let (walk, _) = incremental(&mut graph, None, ours, theirs).expect("incremental");
        let mut walk_conflicts: Vec<PathBuf> =
            walk.conflicts.iter().map(|(p, _)| p.clone()).collect();
        walk_conflicts.sort();
        assert_eq!(walk_conflicts, flat_conflicts);
        assert_eq!(expanded(&mut graph, walk.merged), flat_merged);
        assert_eq!(flat_conflicts, vec![PathBuf::from("both.txt")]);
    }

    /// The unopened-tree invariant `incremental_merge_trees` relies on instead
    /// of a validation pass: every `Tree` entry the result carries by id that
    /// the walk never opened is a tree `ours` already references. Checked on
    /// the shape with the most unopened trees — a large shared subtree, an
    /// adopted-from-theirs subtree with unchanged nested parts, and a
    /// theirs-added subtree (which the gate enumerates in full).
    #[test]
    fn unopened_trees_are_heads_own() {
        let mut graph = CountingTrees::default();
        let shared = deep(1);
        let nested_unchanged = deep(7);
        let (base, _) = build(
            &mut graph,
            &dir(&[
                ("shared", shared.clone()),
                (
                    "moved",
                    dir(&[("keep", nested_unchanged.clone()), ("x.txt", Node::Blob(2))]),
                ),
            ]),
        );
        let ours = base;
        let (theirs, _) = build(
            &mut graph,
            &dir(&[
                ("shared", shared),
                (
                    "moved",
                    dir(&[("keep", nested_unchanged), ("x.txt", Node::Blob(3))]),
                ),
                ("added", deep(9)),
            ]),
        );
        let ours_trees: HashSet<ObjectHash> = {
            let mut g = CountingTrees::default();
            let (root, _) = build(
                &mut g,
                &dir(&[
                    ("shared", deep(1)),
                    ("moved", dir(&[("keep", deep(7)), ("x.txt", Node::Blob(2))])),
                ]),
            );
            assert_eq!(root, ours);
            g.trees.keys().copied().collect()
        };
        graph.reads = 0;
        graph.read_ids.clear();
        graph.seen.clear();
        let (walk, _) = incremental(&mut graph, Some(base), ours, theirs).expect("merge");
        let opened: HashSet<ObjectHash> = graph.read_ids.iter().copied().collect();
        // Every subtree carried by id: expand ITS tree closure from the fixture
        // graph and check each tree the walk did not open is one of ours'.
        let mut stack: Vec<ObjectHash> = walk
            .merged
            .values()
            .filter(|e| e.mode == TreeItemMode::Tree)
            .map(|e| e.hash)
            .collect();
        assert!(!stack.is_empty(), "the fixture carries subtrees by id");
        let mut unopened = 0;
        while let Some(id) = stack.pop() {
            if !opened.contains(&id) {
                unopened += 1;
                assert!(
                    ours_trees.contains(&id),
                    "an unopened carried tree must already be referenced by ours: {id}"
                );
            }
            let tree = graph.trees.get(&id).expect("fixture tree");
            stack.extend(
                tree.tree_items
                    .iter()
                    .filter(|i| i.mode == TreeItemMode::Tree)
                    .map(|i| i.id),
            );
        }
        assert!(
            unopened > 0,
            "the shape really leaves trees unopened (shared/, moved/keep/)"
        );
        assert_eq!(
            walk.changed_paths,
            1 + 1,
            "moved/x.txt and added/…/leaf.txt"
        );
    }

    /// G4: the flattening path is selectable only by the exact test-sentinel pair.
    #[test]
    fn flat_walk_switch_requires_the_test_sentinel() {
        use std::ffi::OsStr;
        assert!(incremental_tree_walk_enabled_for(None, None));
        assert!(
            incremental_tree_walk_enabled_for(None, Some(OsStr::new("flat"))),
            "no sentinel: production walk"
        );
        assert!(incremental_tree_walk_enabled_for(
            Some(OsStr::new("1")),
            Some(OsStr::new("tree"))
        ));
        assert!(!incremental_tree_walk_enabled_for(
            Some(OsStr::new("1")),
            Some(OsStr::new("flat"))
        ));
    }

    /// G9 + G10 on the pruning path: a gitlink hidden inside an adopted subtree
    /// that theirs CHANGED is arbitration and fails closed even though the walk
    /// never visited it; one inside a subtree all three sides share passes
    /// through untouched, unvisited.
    #[test]
    fn gitlinks_inside_pruned_subtrees_follow_adr_mg_01() {
        // Pass-through: identical everywhere, buried, never opened.
        let mut graph = CountingTrees::default();
        let sub = dir(&[("vendor", dir(&[("lib", Node::Gitlink(1))]))]);
        let (base, _) = build(
            &mut graph,
            &dir(&[("deps", sub.clone()), ("f.txt", Node::Blob(2))]),
        );
        let (ours, _) = build(
            &mut graph,
            &dir(&[("deps", sub.clone()), ("f.txt", Node::Blob(3))]),
        );
        let (theirs, _) = build(&mut graph, &dir(&[("deps", sub), ("f.txt", Node::Blob(2))]));
        graph.reads = 0;
        let (walk, passthrough) = incremental(&mut graph, Some(base), ours, theirs).expect("merge");
        assert_eq!(
            graph.reads, 2,
            "only the two distinct root trees are opened (base and theirs are the same \
             tree), never deps/: {:?}",
            graph.read_ids
        );
        assert!(
            passthrough.is_empty(),
            "nothing visited, nothing to pass through explicitly"
        );
        assert!(
            matches!(walk.merged.get(Path::new("deps")), Some(e) if e.mode == TreeItemMode::Tree)
        );

        // Arbitrated but hidden: ours == base under deps/, theirs moved the pointer.
        let mut graph = CountingTrees::default();
        let (base, _) = build(
            &mut graph,
            &dir(&[
                (
                    "deps",
                    dir(&[("vendor", dir(&[("lib", Node::Gitlink(1))]))]),
                ),
                ("f.txt", Node::Blob(2)),
            ]),
        );
        let (ours, _) = build(
            &mut graph,
            &dir(&[
                (
                    "deps",
                    dir(&[("vendor", dir(&[("lib", Node::Gitlink(1))]))]),
                ),
                ("f.txt", Node::Blob(2)),
            ]),
        );
        let (theirs, _) = build(
            &mut graph,
            &dir(&[
                (
                    "deps",
                    dir(&[("vendor", dir(&[("lib", Node::Gitlink(9))]))]),
                ),
                ("f.txt", Node::Blob(2)),
            ]),
        );
        let refused =
            incremental(&mut graph, Some(base), ours, theirs).expect_err("hidden arbitration");
        assert!(
            matches!(&refused, PullMergeError::GitlinkUnsupported(g) if g.path.as_path() == Path::new("deps/vendor/lib")),
            "{refused}"
        );

        // Visited arbitration (theirs added a gitlink next to a changed file) is
        // still refused, exactly as on the flattening path.
        let mut graph = CountingTrees::default();
        let (base, _) = build(&mut graph, &dir(&[("f.txt", Node::Blob(2))]));
        let (ours, _) = build(&mut graph, &dir(&[("f.txt", Node::Blob(3))]));
        let (theirs, _) = build(
            &mut graph,
            &dir(&[("f.txt", Node::Blob(2)), ("sub", Node::Gitlink(1))]),
        );
        let refused =
            incremental(&mut graph, Some(base), ours, theirs).expect_err("visited arbitration");
        assert!(
            matches!(&refused, PullMergeError::GitlinkUnsupported(g) if g.path.as_path() == Path::new("sub"))
        );
    }

    /// G5 performance budget: a synthetic tree of ~10^5 files with 1% of them
    /// changed on one side. Reads must scale with the changed paths (their
    /// depth and their directories' siblings), not with the tree.
    #[test]
    fn synthetic_large_tree_reads_scale_with_changes_not_size() {
        const DIRS: usize = 100;
        const SUBDIRS: usize = 40;
        const FILES: usize = 25; // 100 × 40 × 25 = 100_000 files
        fn tree(changed: &dyn Fn(usize, usize, usize) -> bool) -> Node {
            let dirs: Vec<(String, Node)> = (0..DIRS)
                .map(|d| {
                    let subs: Vec<(String, Node)> = (0..SUBDIRS)
                        .map(|s| {
                            let files: Vec<(String, Node)> = (0..FILES)
                                .map(|f| {
                                    let mut bytes = [0u8; 20];
                                    bytes[0] = d as u8;
                                    bytes[1] = s as u8;
                                    bytes[2] = f as u8;
                                    bytes[3] = if changed(d, s, f) { 1 } else { 0 };
                                    let node = Node::Id(ObjectHash::new(&bytes));
                                    (format!("f{f}.txt"), node)
                                })
                                .collect();
                            (format!("s{s}"), Node::Dir(files))
                        })
                        .collect();
                    (format!("d{d}"), Node::Dir(subs))
                })
                .collect();
            Node::Dir(dirs)
        }
        let mut graph = CountingTrees::default();
        let (base, _) = build(&mut graph, &tree(&|_, _, _| false));
        // Ours: untouched. Theirs: 1% of files (every 100th) rewritten — they
        // fall in 1000 distinct subdirectories across all 100 directories.
        let (theirs, _) = build(
            &mut graph,
            &tree(&|d, s, f| (d * SUBDIRS * FILES + s * FILES + f).is_multiple_of(100)),
        );
        let total_trees = graph.trees.len();
        assert!(
            total_trees > 4_000,
            "the fixture really is large: {total_trees} trees"
        );

        graph.reads = 0;
        graph.seen.clear();
        let (walk, _) = incremental(&mut graph, Some(base), base, theirs).expect("merge");
        let changed_files = DIRS * SUBDIRS * FILES / 100;
        assert_eq!(walk.changed_paths, changed_files);
        // Every changed subdirectory is opened on both sides (the pruned diff
        // that counts changed files) plus its parent directory and the roots;
        // nothing else. Bound: roots + 100 changed dirs × 2 sides + 1000 changed
        // subdirs × 2 sides.
        let changed_subdirs = 1_000;
        let bound = 3 + DIRS * 2 + changed_subdirs * 2;
        assert!(
            walk.merged
                .values()
                .filter(|e| e.mode == TreeItemMode::Tree)
                .count()
                >= 1,
            "adopted subtrees exist"
        );
        assert!(
            graph.reads <= bound,
            "reads {} exceed the changed-path bound {bound} (tree has {total_trees} trees)",
            graph.reads
        );
        assert!(
            graph.reads * 2 < total_trees,
            "reads {} are not a fraction of the {total_trees} trees the flattening path opens",
            graph.reads
        );
    }

    /// MG-05's read contract, measured on the same ~10^5-file fixture (Codex
    /// R10). Rename candidates are collected inside the diffs the walk already
    /// runs, and the subtrees the walk SKIPPED are opened only when the side
    /// could hold both a rename source and a rename destination — and then only
    /// the ones that DIFFER on that side, which is the side's own diff against
    /// the base and exactly what per-side detection has to read. A subtree
    /// identical on both sides is never opened, so the pruning MG-03 bought is
    /// kept for everything a rename cannot reach.
    #[test]
    fn rename_collection_reads_only_the_subtrees_that_differ() {
        const DIRS: usize = 60;
        const SUBDIRS: usize = 20;
        const FILES: usize = 20; // 60 × 20 × 20 = 24_000 files
        /// Extra top-level entries a fixture appends after the directories.
        type TopLevel = Vec<(String, Node)>;
        fn tree(extra: &dyn Fn(&mut TopLevel), touch_dir_zero: bool) -> Node {
            let mut dirs: Vec<(String, Node)> = (0..DIRS)
                .map(|d| {
                    let subs: Vec<(String, Node)> = (0..SUBDIRS)
                        .map(|s| {
                            let files: Vec<(String, Node)> = (0..FILES)
                                .map(|f| {
                                    let mut bytes = [0u8; 20];
                                    bytes[0] = d as u8;
                                    bytes[1] = s as u8;
                                    bytes[2] = f as u8;
                                    bytes[3] = u8::from(touch_dir_zero && d == 0);
                                    (format!("f{f}.txt"), Node::Id(ObjectHash::new(&bytes)))
                                })
                                .collect();
                            (format!("s{s}"), Node::Dir(files))
                        })
                        .collect();
                    (format!("d{d}"), Node::Dir(subs))
                })
                .collect();
            extra(&mut dirs);
            Node::Dir(dirs)
        }

        // Ours rewrites every file under `d0` — a subtree theirs never touches,
        // so MG-03 skips it wholesale — and additionally deletes one top-level
        // file while adding another, which is what gives ours BOTH a possible
        // rename source and a possible rename destination.
        let mut graph = CountingTrees::default();
        let (base, _) = build(
            &mut graph,
            &tree(&|dirs| dirs.push(("gone.txt".into(), Node::Blob(7))), false),
        );
        let (ours, _) = build(
            &mut graph,
            &tree(&|dirs| dirs.push(("added.txt".into(), Node::Blob(7))), true),
        );
        // Theirs changes one file in a different directory and nothing else.
        let (theirs, _) = build(
            &mut graph,
            &tree(
                &|dirs| {
                    dirs.push(("gone.txt".into(), Node::Blob(7)));
                    dirs.push(("theirs.txt".into(), Node::Blob(8)));
                },
                false,
            ),
        );
        let total_trees = graph.trees.len();
        assert!(total_trees > 1_000, "the fixture is large: {total_trees}");

        graph.reads = 0;
        graph.seen.clear();
        let walk = incremental_collecting_renames(&mut graph, Some(base), ours, theirs)
            .expect("merge with rename collection");
        let with_renames = graph.reads;
        // `d0` differs on ours, so detection opens it: its 20 subdirectories on
        // both sides, plus the roots and the top-level directory. Everything
        // else — 59 identical directories and their 1 180 subdirectories — is
        // never touched.
        let opened_by_detection = 1 + SUBDIRS * 2;
        let bound = 6 + opened_by_detection;
        assert!(
            with_renames <= bound,
            "reads {with_renames} exceed the differing-subtree bound {bound} \
             (fixture has {total_trees} trees)"
        );
        assert!(
            with_renames * 10 < total_trees,
            "reads {with_renames} are still a small fraction of {total_trees} trees"
        );
        assert!(
            !walk.rename_sources[0].is_empty() && !walk.rename_dests[0].is_empty(),
            "ours really did offer both a source and a destination"
        );

        // What detection actually COSTS is the delta against the same walk with
        // collection off — MG-03's pruned bound on the identical fixture.
        graph.reads = 0;
        graph.seen.clear();
        incremental(&mut graph, Some(base), ours, theirs).expect("merge without collection");
        let without_renames = graph.reads;
        let added_by_detection = with_renames.saturating_sub(without_renames);
        assert!(
            added_by_detection <= opened_by_detection,
            "detection added {added_by_detection} reads, more than the differing \
             subtree costs ({opened_by_detection}); MG-03 alone read {without_renames}"
        );

        // The same merge with NO rename destination on ours: nothing can pair,
        // so the deferred subtree is never opened and detection costs nothing.
        let mut graph = CountingTrees::default();
        let (base, _) = build(
            &mut graph,
            &tree(&|dirs| dirs.push(("gone.txt".into(), Node::Blob(7))), false),
        );
        let (ours, _) = build(&mut graph, &tree(&|_| {}, true));
        let (theirs, _) = build(
            &mut graph,
            &tree(
                &|dirs| {
                    dirs.push(("gone.txt".into(), Node::Blob(7)));
                    dirs.push(("theirs.txt".into(), Node::Blob(8)));
                },
                false,
            ),
        );
        graph.reads = 0;
        graph.seen.clear();
        let walk = incremental_collecting_renames(&mut graph, Some(base), ours, theirs)
            .expect("merge with no rename destination");
        let collecting = graph.reads;
        assert!(
            walk.rename_dests[0].is_empty(),
            "ours offers no rename destination"
        );
        graph.reads = 0;
        graph.seen.clear();
        incremental(&mut graph, Some(base), ours, theirs).expect("merge without collection");
        assert_eq!(
            collecting, graph.reads,
            "with no destination to pair, detection opens nothing extra"
        );
    }
}

#[cfg(test)]
mod dir_rename {
    use super::*;

    fn pair(old: &str, new: &str) -> rename_detect::RenameMatch {
        rename_detect::RenameMatch {
            old: PathBuf::from(old),
            new: PathBuf::from(new),
            exact: true,
            internal_score: 60_000,
        }
    }

    #[test]
    fn unique_highest_destination_wins_without_a_strict_majority() {
        let plan = infer_provisional_directory_renames(
            &[
                pair("old/a", "winner/a"),
                pair("old/b", "winner/b"),
                pair("old/c", "second/c"),
                pair("old/d", "third/d"),
            ],
            MergeSide::Ours,
        );
        assert_eq!(
            plan.renames,
            [DirectoryRename {
                old: PathBuf::from("old"),
                new: PathBuf::from("winner"),
                side: MergeSide::Ours,
            }]
        );
        assert!(plan.splits.is_empty());
    }

    #[test]
    fn tied_highest_destinations_are_sorted_and_not_inferred() {
        let plan = infer_provisional_directory_renames(
            &[pair("old/a", "z/a"), pair("old/b", "a/b")],
            MergeSide::Theirs,
        );
        assert!(plan.renames.is_empty());
        assert_eq!(
            plan.splits,
            [DirectoryRenameSplit {
                old: PathBuf::from("old"),
                destinations: vec![PathBuf::from("a"), PathBuf::from("z")],
                side: MergeSide::Theirs,
            }]
        );
    }

    #[test]
    fn nested_pairs_vote_for_their_parent_and_ancestor_directories() {
        let plan =
            infer_provisional_directory_renames(&[pair("old/sub/a", "new/sub/a")], MergeSide::Ours);
        assert!(plan.renames.contains(&DirectoryRename {
            old: PathBuf::from("old/sub"),
            new: PathBuf::from("new/sub"),
            side: MergeSide::Ours,
        }));
        assert!(plan.renames.contains(&DirectoryRename {
            old: PathBuf::from("old"),
            new: PathBuf::from("new"),
            side: MergeSide::Ours,
        }));
    }

    #[test]
    fn a_file_rename_within_one_directory_casts_no_directory_vote() {
        let plan =
            infer_provisional_directory_renames(&[pair("same/old", "same/new")], MergeSide::Ours);
        assert_eq!(plan, DirectoryRenamePlan::default());
    }
}

#[cfg(test)]
mod rename {
    //! MG-05: per-side rename detection feeding the three-way match.
    use std::collections::HashMap;

    use git_internal::internal::object::blob::Blob;

    use super::*;

    pub(super) fn file(content: &str) -> MergeTreeEntry {
        MergeTreeEntry {
            hash: Blob::from_content(content).id,
            mode: TreeItemMode::Blob,
        }
    }

    pub(super) fn items(entries: &[(&str, MergeTreeEntry)]) -> HashMap<PathBuf, MergeTreeEntry> {
        entries
            .iter()
            .map(|(path, entry)| (PathBuf::from(path), *entry))
            .collect()
    }

    /// MG-06: [`apply_renames`] now merges content for the collision and 1to2
    /// shapes, so the unit tests hand it a blob store already holding the
    /// fixtures' contents. `persist` is false, so nothing reaches the object
    /// store and every merged blob stays addressable in memory.
    pub(super) fn apply_for_test(
        base: &mut HashMap<PathBuf, MergeTreeEntry>,
        ours: &mut HashMap<PathBuf, MergeTreeEntry>,
        theirs: &mut HashMap<PathBuf, MergeTreeEntry>,
        decisions: &[RenameDecision],
        contents: &[&str],
    ) -> (Vec<(PathBuf, ConflictKind)>, Vec<RenameConflictNote>) {
        let mut blobs = VirtualBlobs::new();
        for content in contents {
            let blob = Blob::from_content(content);
            blobs.insert(blob.id, blob.data.clone());
        }
        let mut forced = Vec::new();
        let notes = apply_renames(
            base,
            ours,
            theirs,
            decisions,
            &mut forced,
            diffy::ConflictStyle::Merge,
            ("HEAD", "feature"),
            &mut TreeMergeContext::top_level(false, None, None, &mut blobs),
        )
        .expect("the rename pass succeeds");
        (forced, notes)
    }

    pub(super) fn pair(old: &str, new: &str) -> rename_detect::RenameMatch {
        rename_detect::RenameMatch {
            old: PathBuf::from(old),
            new: PathBuf::from(new),
            exact: true,
            internal_score: 60_000,
        }
    }

    /// Only paths one side deleted and paths it added become candidates, and
    /// gitlinks never do (ADR-MG-01 refuses arbitrating a submodule long
    /// before this).
    #[test]
    fn the_snapshot_holds_only_this_sides_deletions_and_additions() {
        let base = items(&[
            ("gone.txt", file("gone\n")),
            ("kept.txt", file("kept\n")),
            (
                "sub",
                MergeTreeEntry {
                    hash: file("x\n").hash,
                    mode: TreeItemMode::Commit,
                },
            ),
        ]);
        let side = items(&[
            ("kept.txt", file("kept\n")),
            ("added.txt", file("added\n")),
            (
                "sub",
                MergeTreeEntry {
                    hash: file("y\n").hash,
                    mode: TreeItemMode::Commit,
                },
            ),
        ]);
        let snapshot = rename_snapshot(&base, &side, false);
        let mut old: Vec<&PathBuf> = snapshot.old_map.keys().collect();
        let mut new: Vec<&PathBuf> = snapshot.new_map.keys().collect();
        old.sort();
        new.sort();
        assert_eq!(old, vec![&PathBuf::from("gone.txt")]);
        assert_eq!(new, vec![&PathBuf::from("added.txt")]);
    }

    /// The three-way match sees ONE triple at the new path: the base entry and
    /// the other side's entry are moved there, and the source disappears
    /// (Git's `process_renames`).
    #[test]
    fn an_accepted_rename_moves_the_base_and_the_other_side_onto_the_new_path() {
        let base_entry = file("base\n");
        let theirs_entry = file("theirs\n");
        let ours_entry = file("base\n");
        let mut base = items(&[("old.txt", base_entry)]);
        let mut ours = items(&[("new.txt", ours_entry)]);
        let mut theirs = items(&[("old.txt", theirs_entry)]);
        let decisions = decide_renames(&base, &ours, &theirs, &[pair("old.txt", "new.txt")], &[]);
        assert!(decisions[0].declined.is_none());
        apply_for_test(
            &mut base,
            &mut ours,
            &mut theirs,
            &decisions,
            &["base\n", "theirs\n"],
        );
        assert_eq!(base.get(Path::new("new.txt")), Some(&base_entry));
        assert_eq!(theirs.get(Path::new("new.txt")), Some(&theirs_entry));
        assert!(!base.contains_key(Path::new("old.txt")));
        assert!(!theirs.contains_key(Path::new("old.txt")));
    }

    /// Every shape MG-05 leaves to MG-06 is declined with a reason, and
    /// declining changes nothing about the maps.
    #[test]
    fn path_level_rename_shapes_are_classified_and_the_collision_is_settled() {
        let base_entry = file("base\n");
        // Both sides renamed the source, to different paths and to the same.
        let base = items(&[("old.txt", base_entry)]);
        let ours = items(&[("ours.txt", base_entry)]);
        let theirs = items(&[("theirs.txt", base_entry)]);
        let decisions = decide_renames(
            &base,
            &ours,
            &theirs,
            &[pair("old.txt", "ours.txt")],
            &[pair("old.txt", "theirs.txt")],
        );
        assert!(matches!(
            decisions[0].declined,
            Some(RenameDeclined::DivergentRenames { ref theirs }) if theirs == Path::new("theirs.txt")
        ));
        let same = items(&[("new.txt", base_entry)]);
        let decisions = decide_renames(
            &base,
            &same,
            &same,
            &[pair("old.txt", "new.txt")],
            &[pair("old.txt", "new.txt")],
        );
        assert_eq!(decisions[0].declined, Some(RenameDeclined::SameDestination));
        // The other side deleted the source: rename/delete.
        let theirs = items(&[("unrelated.txt", file("u\n"))]);
        let decisions = decide_renames(&base, &ours, &theirs, &[pair("old.txt", "ours.txt")], &[]);
        assert_eq!(decisions[0].declined, Some(RenameDeclined::SourceDeleted));
        // The other side replaced the source with a different KIND of entry:
        // Git's `type_changed` branch, NOT a rename/delete (`merge-ort.c:3205`).
        let link = MergeTreeEntry {
            hash: Blob::from_content("target").id,
            mode: TreeItemMode::Link,
        };
        let theirs = items(&[("old.txt", link)]);
        let decisions = decide_renames(&base, &ours, &theirs, &[pair("old.txt", "ours.txt")], &[]);
        assert_eq!(
            decisions[0].declined,
            Some(RenameDeclined::SourceTypeChanged)
        );
        // A file at the exact destination is rename/add, and the collision
        // consumes the source. Structural occupancy is classified separately
        // because it leaves the source in place.
        let collision_theirs =
            items(&[("old.txt", base_entry), ("ours.txt", file("theirs add\n"))]);
        let decisions = decide_renames(
            &base,
            &ours,
            &collision_theirs,
            &[pair("old.txt", "ours.txt")],
            &[],
        );
        assert_eq!(
            decisions[0].declined,
            Some(RenameDeclined::DestinationCollision)
        );
        let theirs = items(&[
            ("old.txt", base_entry),
            ("ours.txt/child", file("theirs child\n")),
        ]);
        let blocked = decide_renames(&base, &ours, &theirs, &[pair("old.txt", "ours.txt")], &[]);
        assert_eq!(
            blocked[0].declined,
            Some(RenameDeclined::DestinationBlocked)
        );

        // MG-06: a collision no longer degrades to "no rename". Git merges the
        // rename itself first — base source, the renaming side's landing spot,
        // the other side's source — parks that result at the renaming side's
        // stage of the destination, and resolves the SOURCE by removal
        // (`merge-ort.c:3137-3179`), leaving the destination an add/add with no
        // base stage.
        let mut base_copy = base.clone();
        let mut ours_copy = ours.clone();
        let mut theirs_copy = collision_theirs.clone();
        let (forced, notes) = apply_for_test(
            &mut base_copy,
            &mut ours_copy,
            &mut theirs_copy,
            &decisions,
            &["base\n", "ours\n", "theirs add\n"],
        );
        assert!(
            !base_copy.contains_key(Path::new("old.txt"))
                && !ours_copy.contains_key(Path::new("old.txt"))
                && !theirs_copy.contains_key(Path::new("old.txt")),
            "the source is resolved by removal on every side"
        );
        assert!(
            !base_copy.contains_key(Path::new("ours.txt")),
            "the destination records no merge base, so it is an add/add"
        );
        assert_eq!(
            ours_copy.get(Path::new("ours.txt")).map(|entry| entry.hash),
            Some(base_entry.hash),
            "the destination holds the rename's OWN merge — base, ours' landing \
             spot and theirs' source are all the base content here, so that \
             merge is the base content"
        );
        assert_eq!(
            theirs_copy.get(Path::new("ours.txt")),
            collision_theirs.get(Path::new("ours.txt")),
            "the other side's entry stays where it is"
        );
        assert!(
            forced.is_empty(),
            "a collision needs no forced conflict: the add/add is the ordinary match's own verdict"
        );
        assert!(
            notes.is_empty(),
            "the rename's own merge is clean here, so Git prints nothing extra"
        );
    }

    /// G6: past `merge.renameLimit` the engine drops the inexact stage —
    /// exact renames still pair, and the caller is told (the merge never
    /// fails over it).
    #[test]
    fn passing_the_rename_limit_degrades_to_exact_pairs_only() {
        let mut base = HashMap::new();
        let mut side = HashMap::new();
        // One exact pair plus TWO inexact candidates per side: after the exact
        // stage each side still holds more than the limit, so the gate closes.
        base.insert(PathBuf::from("exact-old.txt"), file("exactly the same\n"));
        side.insert(PathBuf::from("exact-new.txt"), file("exactly the same\n"));
        for index in 0..2 {
            base.insert(
                PathBuf::from(format!("similar-old-{index}.txt")),
                file(&format!("aaaa\nbbbb\ncccc\ndddd{index}\n")),
            );
            side.insert(
                PathBuf::from(format!("similar-new-{index}.txt")),
                file(&format!("aaaa\nbbbb\ncccc\nchanged{index}\n")),
            );
        }
        let blobs = VirtualBlobs::new();
        let mut reader = MergeRenameReader::new();
        let limited = detect_side_renames(
            &base,
            &side,
            &MergeRenameConfig {
                rename_limit: 1,
                ..MergeRenameConfig::default()
            },
            &blobs,
            &mut reader,
        );
        assert!(
            limited.skipped_by_limit,
            "the caller can report the degradation"
        );
        // `all` alone would pass on an EMPTY list (Codex R14 P2), which is
        // exactly the regression this guards: past the limit the exact pair
        // must still be there, and be the only one.
        assert_eq!(
            limited.matches.len(),
            1,
            "exactly the exact pair survives: {:?}",
            limited.matches
        );
        assert_eq!(limited.matches[0].old, Path::new("exact-old.txt"));
        assert_eq!(limited.matches[0].new, Path::new("exact-new.txt"));
        assert!(
            limited.matches[0].exact,
            "and it survives as an EXACT pair: {:?}",
            limited.matches
        );
    }
}

#[cfg(test)]
mod df {
    //! MG-04: directory/file collisions.
    use std::collections::HashMap;

    use git_internal::internal::object::blob::Blob;

    use super::*;

    fn file(content: &str) -> MergeTreeEntry {
        MergeTreeEntry {
            hash: Blob::from_content(content).id,
            mode: TreeItemMode::Blob,
        }
    }

    fn marker(byte: u8) -> MergeTreeEntry {
        MergeTreeEntry {
            hash: ObjectHash::new(&[byte; 20]),
            mode: TreeItemMode::Tree,
        }
    }

    fn items(entries: &[(&str, MergeTreeEntry)]) -> HashMap<PathBuf, MergeTreeEntry> {
        entries
            .iter()
            .map(|(path, entry)| (PathBuf::from(path), *entry))
            .collect()
    }

    fn candidate(path: &str, base_file: Option<MergeTreeEntry>, base_present: bool) -> DfCandidate {
        DfCandidate {
            path: PathBuf::from(path),
            file_side: MergeSide::Ours,
            file: file("ours\n"),
            base_file,
            base_present,
        }
    }

    /// No subtree ever needs reading in these fixtures.
    fn no_subtrees() -> impl FnMut(&ObjectHash) -> Result<bool, PullMergeError> {
        |_| Ok(false)
    }

    fn resolve(
        merged: &mut HashMap<PathBuf, MergeTreeEntry>,
        conflicts: &mut Vec<(PathBuf, ConflictKind)>,
        candidates: Vec<DfCandidate>,
    ) -> isize {
        let mut reader = no_subtrees();
        resolve_df_conflicts(merged, conflicts, candidates, &mut reader).expect("resolve")
    }

    #[test]
    fn unique_df_path_flattens_the_branch_and_suffixes_taken_names() {
        let mut taken = HashSet::new();
        assert_eq!(
            unique_df_path(Path::new("dir/foo"), "topic/x", &taken),
            PathBuf::from("dir/foo~topic_x")
        );
        taken.insert(PathBuf::from("foo~HEAD"));
        taken.insert(PathBuf::from("foo~HEAD_0"));
        assert_eq!(
            unique_df_path(Path::new("foo"), "HEAD", &taken),
            PathBuf::from("foo~HEAD_1")
        );
    }

    /// The rule measured against real `git merge` (see
    /// [`directory_is_in_the_way`]): a file beneath the path always blocks it;
    /// an empty-only subtree blocks it only when the base had nothing there;
    /// an entry AT the path itself is not beneath it.
    #[test]
    fn a_directory_is_in_the_way_exactly_as_git_decides() {
        let mut reader = no_subtrees();
        let with_file = [
            (PathBuf::from("foo"), file("f\n")),
            (PathBuf::from("foo/bar.txt"), file("b\n")),
        ];
        let empty_only = [
            (PathBuf::from("foo"), file("f\n")),
            (PathBuf::from("foo/bar"), marker(7)),
        ];
        let at_the_path = [(PathBuf::from("foo"), file("f\n"))];
        for (entries, base_present, expected) in [
            (&with_file[..], true, true),
            (&with_file[..], false, true),
            // Base had a file (or anything) at `foo`: Git traverses the
            // directory, finds no file, and leaves the file where it is.
            (&empty_only[..], true, false),
            // Base had nothing: Git adopts the new subtree verbatim.
            (&empty_only[..], false, true),
            (&at_the_path[..], false, false),
        ] {
            assert_eq!(
                directory_is_in_the_way(Path::new("foo"), entries, base_present, &mut reader)
                    .expect("decide"),
                expected,
                "entries {entries:?}, base_present {base_present}"
            );
        }
    }

    /// A carried subtree (the incremental engine's `TreeItemMode::Tree` entry)
    /// beneath a collision is read to see whether it holds a file.
    #[test]
    fn a_carried_subtree_is_read_to_decide_whether_it_holds_a_file() {
        let entries = [
            (PathBuf::from("foo"), file("f\n")),
            (PathBuf::from("foo/sub"), marker(9)),
        ];
        let mut reads = Vec::new();
        let mut holds = |id: &ObjectHash| {
            reads.push(*id);
            Ok(true)
        };
        assert!(
            directory_is_in_the_way(Path::new("foo"), &entries, true, &mut holds).expect("decide")
        );
        assert_eq!(
            reads,
            vec![marker(9).hash],
            "only the subtree beneath is read"
        );
        let mut empty = |_: &ObjectHash| Ok(false);
        assert!(
            !directory_is_in_the_way(Path::new("foo"), &entries, true, &mut empty).expect("decide")
        );
    }

    /// A modify/delete conflict under a surviving directory is settled into a
    /// `modify_delete` file/directory conflict (Git: D/F relocation runs
    /// before the modify/delete branch, so `foo~HEAD` carries stages 1 and 2)
    /// and PLACED at the unique name; a modify/delete with no directory
    /// beneath it stays where it is.
    #[test]
    fn modify_delete_under_a_surviving_directory_moves_and_keeps_its_report_kind() {
        let base = file("base\n");
        let ours = file("edited\n");
        let mut merged = items(&[("foo/bar.txt", file("b\n")), ("foo~HEAD", file("x\n"))]);
        let mut conflicts = vec![
            (
                PathBuf::from("foo"),
                ConflictKind::OursModifiedTheirsDeleted { ours: ours.hash },
            ),
            (
                PathBuf::from("other.txt"),
                ConflictKind::TheirsModifiedOursDeleted {
                    theirs: file("t\n").hash,
                },
            ),
        ];
        resolve(
            &mut merged,
            &mut conflicts,
            vec![DfCandidate {
                path: PathBuf::from("foo"),
                file_side: MergeSide::Ours,
                file: ours,
                base_file: Some(base),
                base_present: true,
            }],
        );
        assert!(matches!(
            conflicts[0].1,
            ConflictKind::FileDirectory {
                file_side: MergeSide::Ours,
                modify_delete: true,
                base_file: Some(b),
                file: f,
            } if b == base && f == ours
        ));
        let occupied = df_occupied_names(&[], &merged, &conflicts);
        let placed = conflict_placements(&conflicts, &occupied, "feature");
        assert_eq!(placed.len(), 2);
        // `foo~HEAD` is taken by the merge result, so the suffix kicks in.
        assert_eq!(placed[0].0, PathBuf::from("foo~HEAD_0"));
        assert_eq!(placed[0].2.as_deref(), Some(Path::new("foo")));
        // No directory beneath `other.txt`: it stays where it is.
        assert_eq!(placed[1].0, PathBuf::from("other.txt"));
        assert!(placed[1].2.is_none());
        let reports = conflict_reports(&placed);
        assert_eq!(reports[0].kind, "modify-delete");
        assert_eq!(reports[1].kind, "modify-delete");
        assert!(reports[1].original_path.is_none());
    }

    /// `-X ours/theirs` does not settle a modify/delete under a directory:
    /// whether the favoured resolution kept the edited file or dropped it, the
    /// post-pass re-raises Git's conflict at the moved name (verified against
    /// `git merge -X theirs` / `-X ours`).
    #[test]
    fn a_favoured_modify_delete_under_a_directory_stays_a_conflict() {
        let base = file("base\n");
        let ours = file("edited\n");
        let candidate = || DfCandidate {
            path: PathBuf::from("foo"),
            file_side: MergeSide::Ours,
            file: ours,
            base_file: Some(base),
            base_present: true,
        };
        // `-X theirs`: the deletion won, `foo` is gone from the result.
        let mut merged = items(&[("foo/bar.txt", file("b\n"))]);
        let mut conflicts = Vec::new();
        resolve(&mut merged, &mut conflicts, vec![candidate()]);
        assert!(matches!(
            conflicts.as_slice(),
            [(
                _,
                ConflictKind::FileDirectory {
                    modify_delete: true,
                    ..
                }
            )]
        ));
        // `-X ours`: the edited file won and sits in the result.
        let mut merged = items(&[("foo/bar.txt", file("b\n")), ("foo", ours)]);
        let mut conflicts = Vec::new();
        resolve(&mut merged, &mut conflicts, vec![candidate()]);
        assert!(!merged.contains_key(Path::new("foo")));
        assert!(matches!(
            conflicts.as_slice(),
            [(
                _,
                ConflictKind::FileDirectory {
                    modify_delete: true,
                    ..
                }
            )]
        ));
        // Untouched file (equal to the base) replaced by a directory: a clean
        // deletion, never re-raised.
        let mut merged = items(&[("foo/bar.txt", file("b\n"))]);
        let mut conflicts = Vec::new();
        resolve(
            &mut merged,
            &mut conflicts,
            vec![DfCandidate {
                path: PathBuf::from("foo"),
                file_side: MergeSide::Ours,
                file: base,
                base_file: Some(base),
                base_present: true,
            }],
        );
        assert!(conflicts.is_empty());
    }

    /// Git's `unique_path` treats a DIRECTORY named `foo~HEAD` as taken, and
    /// so is every INPUT path even when the merge deleted it (verified against
    /// `git merge`: the file becomes `foo~HEAD_0` in both cases).
    #[test]
    fn placements_treat_occupied_directories_and_deleted_inputs_as_taken() {
        let merged = items(&[
            ("foo/bar.txt", file("b\n")),
            ("foo~HEAD/deep/bar.txt", file("t\n")),
        ]);
        let conflicts = vec![(
            PathBuf::from("foo"),
            ConflictKind::FileDirectory {
                file: file("f\n"),
                file_side: MergeSide::Ours,
                base_file: None,
                modify_delete: false,
            },
        )];
        let occupied = df_occupied_names(&[], &merged, &conflicts);
        let placed = conflict_placements(&conflicts, &occupied, "feature");
        assert_eq!(placed[0].0, PathBuf::from("foo~HEAD_0"));
        let mut names = occupied_names(merged.keys());
        assert!(names.remove(Path::new("foo~HEAD")) && names.remove(Path::new("foo~HEAD/deep")));
        assert!(names.remove(Path::new("foo")) && !names.contains(Path::new("")));

        // A `foo~HEAD` only the base had — deleted on both sides, absent from
        // the result — still occupies its name.
        let base = items(&[("foo", file("base\n")), ("foo~HEAD", file("gone\n"))]);
        let merged = items(&[("foo/bar.txt", file("b\n"))]);
        let occupied = df_occupied_names(&[&base], &merged, &conflicts);
        let placed = conflict_placements(&conflicts, &occupied, "feature");
        assert_eq!(placed[0].0, PathBuf::from("foo~HEAD_0"));

        // Nothing to relocate: the occupancy set is never even built.
        let plain = vec![(
            PathBuf::from("foo"),
            ConflictKind::OursModifiedTheirsDeleted {
                ours: file("f\n").hash,
            },
        )];
        assert!(df_occupied_names_if_needed(&[&base], &merged, &plain).is_empty());
        assert!(!df_occupied_names_if_needed(&[&base], &merged, &conflicts).is_empty());
    }

    /// Codex R3: the post-pass is linear in the candidate count — one slot
    /// lookup, one sorted view of the result — not a per-candidate walk of the
    /// conflict list. Thousands of independent file/directory collisions
    /// resolve in one pass and every one of them moves.
    #[test]
    fn resolve_df_conflicts_scales_linearly_with_the_candidate_count() {
        const N: usize = 4000;
        let mut merged = HashMap::new();
        let mut candidates = Vec::with_capacity(N);
        for i in 0..N {
            let content = format!("{i}\n");
            merged.insert(PathBuf::from(format!("d{i}")), file(&content));
            merged.insert(PathBuf::from(format!("d{i}/leaf.txt")), file("leaf\n"));
            candidates.push(DfCandidate {
                path: PathBuf::from(format!("d{i}")),
                file_side: MergeSide::Ours,
                file: file(&content),
                base_file: None,
                base_present: false,
            });
        }
        let mut conflicts = Vec::new();
        let delta = resolve(&mut merged, &mut conflicts, candidates);
        assert_eq!(conflicts.len(), N);
        assert_eq!(delta, N as isize);
        assert_eq!(merged.len(), N, "only the leaves remain");
    }

    /// The post-pass moves a file only when the directory is in the way, and
    /// reports the delta the incremental engine adds to `files_changed`.
    #[test]
    fn resolve_df_conflicts_moves_only_when_the_directory_is_in_the_way() {
        let mut merged = items(&[("foo", file("f\n")), ("foo/bar.txt", file("b\n"))]);
        let mut conflicts = Vec::new();
        let delta = resolve(
            &mut merged,
            &mut conflicts,
            vec![candidate("foo", None, false)],
        );
        assert_eq!(delta, 1, "ours' kept file left the result");
        assert!(!merged.contains_key(Path::new("foo")));
        assert!(matches!(
            conflicts.as_slice(),
            [(
                path,
                ConflictKind::FileDirectory {
                    file_side: MergeSide::Ours,
                    base_file: None,
                    modify_delete: false,
                    ..
                }
            )] if path == Path::new("foo")
        ));

        // Nothing beneath it: the file stays, no conflict, no delta.
        let mut merged = items(&[("foo", file("f\n"))]);
        let mut conflicts = Vec::new();
        let delta = resolve(
            &mut merged,
            &mut conflicts,
            vec![candidate("foo", None, false)],
        );
        assert!(merged.contains_key(Path::new("foo")) && conflicts.is_empty() && delta == 0);

        // An empty-only subtree the BASE already had: not in the way.
        let mut merged = items(&[("foo", file("f\n")), ("foo/bar", marker(3))]);
        let mut conflicts = Vec::new();
        resolve(
            &mut merged,
            &mut conflicts,
            vec![candidate("foo", Some(file("base\n")), true)],
        );
        assert!(merged.contains_key(Path::new("foo")) && conflicts.is_empty());
    }

    /// Inside the fold nobody is asked: the file is moved under the temporary
    /// branch label so the ancestor tree never holds a blob and a subtree
    /// under one name (`merge-ort.c:4120-4198` at `call_depth > 0`), and the
    /// occupancy covers names only an input had.
    #[test]
    fn relocate_virtual_df_files_uses_the_temporary_branch_labels() {
        let base = items(&[("keep.txt", file("k\n"))]);
        let ours = items(&[("keep.txt", file("k\n")), ("foo", file("file\n"))]);
        let theirs = items(&[("keep.txt", file("k\n")), ("foo/bar.txt", file("bar\n"))]);
        let mut merged = items(&[
            ("keep.txt", file("k\n")),
            ("foo", file("file\n")),
            ("foo/bar.txt", file("bar\n")),
        ]);
        relocate_virtual_df_files(&mut merged, &base, &ours, &theirs);
        let mut paths: Vec<String> = merged.keys().map(|p| p.display().to_string()).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "foo/bar.txt".to_string(),
                "foo~Temporary merge branch 1".to_string(),
                "keep.txt".to_string()
            ]
        );

        // The mirror image labels the file after the fold's theirs, and a name
        // only the base had pushes the relocation to `_0`.
        let base = items(&[
            ("keep.txt", file("k\n")),
            ("foo~Temporary merge branch 2", file("gone\n")),
        ]);
        let mut merged = items(&[
            ("keep.txt", file("k\n")),
            ("foo", file("file\n")),
            ("foo/bar.txt", file("bar\n")),
        ]);
        relocate_virtual_df_files(&mut merged, &base, &theirs, &ours);
        assert!(merged.contains_key(Path::new("foo~Temporary merge branch 2_0")));
        assert!(merged.contains_key(Path::new("foo/bar.txt")));
    }
}

/// MG-06 G9: the index stage combination each path-level rename shape leaves,
/// asserted where it is decided — on the three maps a conflict's stages are
/// read from (`merge.rs` builds stage 1/2/3 from base/ours/theirs at the
/// conflict path) plus the conflicts the rename pass forces. Every expectation
/// is the shape measured on git 2.50.1; the CLI cases in
/// `command::merge_test::merge_rename_conflict*` pin the same combinations end
/// to end, on both walks.
#[cfg(test)]
mod rename_conflict {
    use git_internal::internal::object::blob::Blob;

    use super::{
        rename::{apply_for_test, file, items, pair},
        *,
    };

    /// Which stages a path would get, in the order the index writer reads them.
    fn stages_at(
        base: &HashMap<PathBuf, MergeTreeEntry>,
        ours: &HashMap<PathBuf, MergeTreeEntry>,
        theirs: &HashMap<PathBuf, MergeTreeEntry>,
        path: &str,
    ) -> Vec<u8> {
        let path = PathBuf::from(path);
        let mut stages = Vec::new();
        for (stage, map) in [(1u8, base), (2, ours), (3, theirs)] {
            if map
                .get(&path)
                .is_some_and(|entry| entry.mode != TreeItemMode::Tree)
            {
                stages.push(stage);
            }
        }
        stages
    }

    /// rename/rename(1to2): stage 1 stays under the OLD name, each destination
    /// carries only its own side, and both destinations hold the SAME merged
    /// object (`merge-ort.c:3021-3068`). Measured: `1 old`, `2 a`, `3 b`.
    #[test]
    fn one_to_two_records_both_destinations_and_drops_the_source() {
        let base_entry = file("l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n");
        let ours_entry = file("l1\nOURS\nl3\nl4\nl5\nl6\nl7\nl8\n");
        let theirs_entry = file("l1\nl2\nTHEIRS\nl4\nl5\nl6\nl7\nl8\n");
        let mut base = items(&[("old", base_entry)]);
        let mut ours = items(&[("a", ours_entry)]);
        let mut theirs = items(&[("b", theirs_entry)]);
        let decisions = decide_renames(
            &base,
            &ours,
            &theirs,
            &[pair("old", "a")],
            &[pair("old", "b")],
        );
        let (forced, notes) = apply_for_test(
            &mut base,
            &mut ours,
            &mut theirs,
            &decisions,
            &[
                "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n",
                "l1\nOURS\nl3\nl4\nl5\nl6\nl7\nl8\n",
                "l1\nl2\nTHEIRS\nl4\nl5\nl6\nl7\nl8\n",
            ],
        );
        // Deviation from Git, documented in `apply_renames`: the source is
        // resolved by removal so the conflict stays resolvable.
        assert!(stages_at(&base, &ours, &theirs, "old").is_empty());
        assert_eq!(stages_at(&base, &ours, &theirs, "a"), vec![2]);
        assert_eq!(stages_at(&base, &ours, &theirs, "b"), vec![3]);
        assert_eq!(
            ours.get(Path::new("a")),
            theirs.get(Path::new("b")),
            "ONE content merge is recorded at both destinations"
        );
        let forced_paths: Vec<&Path> = forced.iter().map(|(path, _)| path.as_path()).collect();
        assert!(
            forced_paths.contains(&Path::new("a")) && forced_paths.contains(&Path::new("b")),
            "both destinations are conflicts the ordinary match must not decide: {forced_paths:?}"
        );
        assert!(
            !forced_paths.contains(&Path::new("old")),
            "the source is not a conflict: {forced_paths:?}"
        );
        assert!(matches!(
            notes.as_slice(),
            [RenameConflictNote::RenameRename { .. }]
        ));
    }

    /// rename/delete: the base moves to the NEW path's stage 1, the renaming
    /// side keeps its stage, the deleting side has none, and the conflict is
    /// forced even for a PURE rename (`merge-ort.c:3202-3221`).
    #[test]
    fn rename_delete_moves_the_base_to_the_new_path() {
        let base_entry = file("l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n");
        let mut base = items(&[("old", base_entry)]);
        let mut ours = items(&[("new", base_entry)]);
        let mut theirs = items(&[("unrelated", file("u\n"))]);
        let decisions = decide_renames(&base, &ours, &theirs, &[pair("old", "new")], &[]);
        assert_eq!(decisions[0].declined, Some(RenameDeclined::SourceDeleted));
        let (forced, notes) = apply_for_test(
            &mut base,
            &mut ours,
            &mut theirs,
            &decisions,
            &["l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n"],
        );
        assert_eq!(stages_at(&base, &ours, &theirs, "new"), vec![1, 2]);
        assert!(stages_at(&base, &ours, &theirs, "old").is_empty());
        assert!(
            matches!(
                forced.as_slice(),
                [(path, ConflictKind::OursModifiedTheirsDeleted { .. })] if path == Path::new("new")
            ),
            "a pure rename plus a delete is still forced to conflict: {forced:?}"
        );
        assert!(matches!(
            notes.as_slice(),
            [RenameConflictNote::RenameDelete { .. }]
        ));
    }

    /// A source the other side TYPE-changed is not a rename/delete: the base
    /// still follows the rename, but the type-changed entry SURVIVES under the
    /// old name and Git says nothing about a rename (`merge-ort.c:3205-3212`).
    #[test]
    fn a_type_changed_source_survives_and_is_not_announced() {
        let base_entry = file("l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n");
        let link = MergeTreeEntry {
            hash: Blob::from_content("target").id,
            mode: TreeItemMode::Link,
        };
        let mut base = items(&[("old", base_entry)]);
        let mut ours = items(&[("new", base_entry)]);
        let mut theirs = items(&[("old", link)]);
        let decisions = decide_renames(&base, &ours, &theirs, &[pair("old", "new")], &[]);
        assert_eq!(
            decisions[0].declined,
            Some(RenameDeclined::SourceTypeChanged)
        );
        let (_, notes) = apply_for_test(
            &mut base,
            &mut ours,
            &mut theirs,
            &decisions,
            &["l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\n"],
        );
        assert_eq!(stages_at(&base, &ours, &theirs, "new"), vec![1, 2]);
        assert_eq!(
            theirs.get(Path::new("old")),
            Some(&link),
            "the type-changed entry keeps the old name"
        );
        assert!(
            notes.is_empty(),
            "Git prints no rename line for a type change: {notes:?}"
        );
    }
}
