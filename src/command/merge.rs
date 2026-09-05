//! Merge command orchestration that resolves base/target commits, performs recursive merge, stages results, and updates refs or surfaces conflicts.

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    str::FromStr,
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
    get_target_commit, load_object, load_object_raw, reset,
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

async fn resolve_pending_autostash(output: &OutputConfig) -> Option<String> {
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
    resolve_pending_autostash_with(output, snapshot).await
}

/// The consumer half, taking a snapshot the CALLER loaded — the merge
/// controls load theirs before mutating anything (W2 r6 #4), so the document
/// they preflighted is the one consumed.
async fn resolve_pending_autostash_with(
    output: &OutputConfig,
    snapshot: AutostashSnapshot,
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
            match crate::command::stash::store_stash_commit(&oid, "autostash").await {
                Ok(()) => {
                    match cleanup_autostash_if_matches(&snapshot) {
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
    let skip_hooks = options.skip_hooks;
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
    // ADR-MG-01: refuse a submodule-arbitrating merge here, ahead of the
    // autostash below — its stash commit, sidecar and worktree reset are
    // writes, and the card requires a refused merge to make none.
    preflight_merge_gitlinks(&target_commit, &options).await?;

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
    let result = run_merge_for_pull_inner(target_commit, upstream, output, options).await;
    // Uniform finalize: applies when no merge state persists (clean success,
    // up-to-date, squash, or a start failure), holds while state exists
    // (conflict / --no-commit). The merge outcome itself is never changed.
    // Skipped for `--dry-run`, which took no autostash and must not apply or
    // promote a held one either.
    let autostash_outcome = if dry_run {
        None
    } else {
        resolve_pending_autostash(output).await
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
    if base_commits.len() <= 1 && incremental_tree_walk_enabled() {
        return perform_incremental_three_way_merge(
            current_commit,
            target_commit,
            base_commits.first(),
            head_name,
            upstream,
            options,
        )
        .await;
    }
    let (our_items, our_gitlinks) = commit_tree_split_for_merge(&current_commit)?;
    let (their_items, their_gitlinks) = commit_tree_split_for_merge(&target_commit)?;
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
    let (base_items, mut virtual_blobs) = match base_commits.as_slice() {
        [] => (HashMap::new(), VirtualBlobs::new()),
        [base] => (commit_tree_split_for_merge(base)?.0, VirtualBlobs::new()),
        bases => {
            // A conflict INSIDE the virtual ancestor is rendered with the
            // configured style, exactly as Git renders one at any call depth.
            // Resolving the style here rather than only on the outer conflict
            // path is what a multi-base merge costs: an invalid
            // `merge.conflictStyle` stops a criss-cross merge even when the
            // merge itself would come out clean.
            let conflict_style = conflict_style_from_config().await.map_err(|e| match e {
                ConflictStyleError::Invalid(value) => PullMergeError::InvalidConflictStyle(value),
                ConflictStyleError::Read(detail) => PullMergeError::ConflictStyleRead(detail),
            })?;
            let base_ids: Vec<ObjectHash> = bases.iter().map(|base| base.id).collect();
            let ancestor = virtual_merge_base(
                &base_ids,
                &passthrough_gitlinks,
                !options.dry_run,
                conflict_style,
            )?;
            (ancestor.items, ancestor.blobs)
        }
    };
    // Under `--dry-run`, auto-merged blobs are computed in memory only
    // (persist=false) so the preview writes nothing to the object store —
    // under tiered storage a `save_object` would even upload to the remote.
    // The virtual ancestor obeys the same rule: its blobs stay in
    // `virtual_blobs` and its one-shot tree/commit are not materialized.
    let merge_result = merge_tree_items(
        &base_items,
        &our_items,
        &their_items,
        &mut TreeMergeContext::top_level(!options.dry_run, options.favor, &mut virtual_blobs),
    )?;
    let files_changed = count_item_map_changes(&our_items, &merge_result.merged_items);

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
        // and Git prints nothing when it does.
        announce_df_conflicts(&placements, upstream, options.output);
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
    state.save()?;

    if let Err(error) = index.save(path::index()) {
        let _ = MergeState::cleanup();
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
    let files_changed = count_item_map_changes(&original_items, &index_items);
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
        Some(snapshot) => resolve_pending_autostash_with(output, snapshot).await,
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
        Some(snapshot) => resolve_pending_autostash_with(output, snapshot).await,
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
            } else if let Some(favor) = favor {
                favored_resolution(favor, Some(ours), Some(theirs))
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
                && let Some(merged) = try_merge_blob_contents(base, ours, theirs, context)?
            {
                MergeResolution::Use(merged)
            } else if let Some(favor) = favor {
                favored_resolution(favor, Some(ours), Some(theirs))
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
            if let Some(favor) = favor {
                favored_resolution(favor, None, Some(theirs))
            } else {
                MergeResolution::Conflict(ConflictKind::TheirsModifiedOursDeleted {
                    theirs: theirs.hash,
                })
            }
        }
        (true, RelativeState::Modified(ours), RelativeState::Deleted) => {
            if let Some(favor) = favor {
                favored_resolution(favor, Some(ours), None)
            } else {
                MergeResolution::Conflict(ConflictKind::OursModifiedTheirsDeleted {
                    ours: ours.hash,
                })
            }
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
    base: &MergeTreeEntry,
    ours: MergeTreeEntry,
    theirs: MergeTreeEntry,
    context: &mut TreeMergeContext<'_>,
) -> Result<Option<MergeTreeEntry>, PullMergeError> {
    if base.mode != ours.mode || base.mode != theirs.mode || !is_regular_file_mode(base.mode) {
        return Ok(None);
    }

    let base_blob = load_merge_blob(base.hash, context.virtual_blobs)?;
    let ours_blob = load_merge_blob(ours.hash, context.virtual_blobs)?;
    let theirs_blob = load_merge_blob(theirs.hash, context.virtual_blobs)?;

    // Git routes ANY binary input to `ll_binary_merge` instead of the
    // line-level merge (`merge-ll.c` `ll_xdl_merge`), and inside a virtual
    // ancestor that is what decides the content — so the line-level path has to
    // decline here and let [`virtual_conflict_resolution`] apply the rule. The
    // depth-0 behaviour is deliberately left exactly as it was: binary content
    // merging for the user's own merge is a different axis, untouched by MG-02.
    if context.depth > 0
        && (merge_input_is_binary(&base_blob.data)
            || merge_input_is_binary(&ours_blob.data)
            || merge_input_is_binary(&theirs_blob.data))
    {
        return Ok(None);
    }

    let marker_len = conflict_marker_length_at_depth(
        &[&base_blob.data, &ours_blob.data, &theirs_blob.data],
        context.depth,
    );
    let mut merge_options = diffy::MergeOptions::new();
    merge_options
        .set_conflict_style(diffy::ConflictStyle::Diff3)
        .set_conflict_marker_length(marker_len);
    let merged_bytes =
        match merge_options.merge_bytes(&base_blob.data, &ours_blob.data, &theirs_blob.data) {
            Ok(merged) => merged,
            Err(conflicted) => match context.favor {
                Some(favor) => resolve_favored_content(conflicted, marker_len, favor)
                    .map_err(PullMergeError::TreeCreate)?,
                None => return Ok(None),
            },
        };

    let merged_blob = Blob::from_content_bytes(merged_bytes);
    // `--dry-run` (persist=false): the merged OID is computed in memory only —
    // persisting here would write the object store (and, under tiered storage,
    // upload to the durable tier) from a preview. It still has to stay
    // ADDRESSABLE, because inside the recursive fold this blob becomes the
    // virtual ancestor's content and the outer merge loads it by id.
    context.record_merged_blob(&merged_blob)?;

    Ok(Some(MergeTreeEntry {
        hash: merged_blob.id,
        mode: ours.mode,
    }))
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
        match favor {
            MergeFavor::Ours => output.extend_from_slice(&conflicted[ours_start..original_start]),
            MergeFavor::Theirs => output.extend_from_slice(&conflicted[theirs_start..close_start]),
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
/// (`merge-ort.c:5429` swaps `opt->branch1`/`branch2` for these while it folds).
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
    fn top_level(
        persist_merged_blobs: bool,
        favor: Option<MergeFavor>,
        virtual_blobs: &mut VirtualBlobs,
    ) -> TreeMergeContext<'_> {
        TreeMergeContext {
            persist_merged_blobs,
            favor,
            depth: 0,
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
/// Git asks this of the synthetic commit it just built (`merge-ort.c:5429`
/// chains `make_virtual_commit` so the next round can call `get_merge_bases()`
/// on it). The same set falls out of the REAL bases folded so far: the virtual
/// commit's ancestry is exactly the union of theirs, so the common ancestors of
/// it and `next` are `⋃ᵢ (anc(folded[i]) ∩ anc(next))`, and the maximal elements
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
    let history = |error: merge_base::MergeBaseError| PullMergeError::History(error.to_string());
    ensure_virtual_ancestor_width(folded.len())?;
    let [single] = folded else {
        // Candidates tagged with the part (folded base) that produced them.
        let mut candidates: Vec<(usize, ObjectHash)> = Vec::new();
        for (part, base) in folded.iter().enumerate() {
            for candidate in merge_base::merge_bases(base, next).map_err(history)? {
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
                if merge_base::is_ancestor(candidate, other).map_err(history)? {
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
    merge_base::merge_bases(single, next).map_err(history)
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
) -> Result<VirtualAncestor, PullMergeError> {
    let mut blobs = VirtualBlobs::new();
    let items = fold_merge_bases(bases, gitlinks, 1, persist, conflict_style, &mut blobs)?;
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
    persist: bool,
    conflict_style: diffy::ConflictStyle,
    blobs: &mut VirtualBlobs,
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
        let sub_items = fold_merge_bases(
            &sub_bases,
            gitlinks,
            depth + 1,
            persist,
            conflict_style,
            blobs,
        )?;
        items = merge_virtual_items(
            &sub_items,
            &items,
            &next_items,
            depth,
            persist,
            conflict_style,
            blobs,
        )?;
        folded_ids.push(*next);
        timestamp = timestamp.max(next_commit.committer.timestamp);
        if persist {
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
/// with its markers, and a modify/delete "simply reuse[s] the base version for
/// [the] virtual merge base" (`merge-recursive.c`, `handle_change_delete`).
fn merge_virtual_items(
    base_items: &HashMap<PathBuf, MergeTreeEntry>,
    our_items: &HashMap<PathBuf, MergeTreeEntry>,
    their_items: &HashMap<PathBuf, MergeTreeEntry>,
    depth: usize,
    persist: bool,
    conflict_style: diffy::ConflictStyle,
    blobs: &mut VirtualBlobs,
) -> Result<HashMap<PathBuf, MergeTreeEntry>, PullMergeError> {
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
                persist_merged_blobs: persist,
                favor: None,
                depth,
                virtual_blobs: blobs,
            };
            resolve_three_way(base, ours, theirs, &mut context)?
        };
        let entry = match resolution {
            MergeResolution::Use(entry) => Some(entry),
            MergeResolution::Delete => None,
            MergeResolution::Conflict(_) => virtual_conflict_resolution(
                base,
                ours,
                theirs,
                depth,
                persist,
                conflict_style,
                blobs,
            )?,
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
///   is no midpoint between "changed" and "gone"
///   (`merge-recursive.c` `handle_change_delete`);
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
    depth: usize,
    persist: bool,
    conflict_style: diffy::ConflictStyle,
    blobs: &mut VirtualBlobs,
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
            persist_merged_blobs: persist,
            favor: None,
            depth,
            virtual_blobs: blobs,
        }
        .record_merged_blob(blob)
    };

    if merge_input_is_binary(base_bytes)
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
        base_bytes,
        &ours_blob.data,
        &theirs_blob.data,
        depth,
        conflict_style,
    );
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
fn merge_input_is_binary(content: &[u8]) -> bool {
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

/// Git's mode rule for a conflicted content merge (`merge-recursive.c`,
/// `merge_mode_and_contents`): take theirs when the two sides agree or when
/// ours is unchanged, otherwise keep ours.
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
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    depth: usize,
    conflict_style: diffy::ConflictStyle,
) -> Vec<u8> {
    let marker_len = conflict_marker_length_at_depth(&[base, ours, theirs], depth);
    let mut options = diffy::MergeOptions::new();
    options
        .set_conflict_style(conflict_style)
        .set_conflict_marker_length(marker_len);
    match options.merge_bytes(base, ours, theirs) {
        Ok(merged) => merged,
        Err(conflicted) => relabel_conflict_markers(
            conflicted,
            marker_len,
            VIRTUAL_OURS_LABEL,
            VIRTUAL_THEIRS_LABEL,
        ),
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
                    out.merged.insert(path, o.leaf());
                }
                // Both sides made the SAME change: take it, verbatim.
                (_, Some(o), Some(t)) if o == t => {
                    out.merged.insert(path, o.leaf());
                }
                // Added on one side only (no base): take it, verbatim.
                (None, Some(o), None) => {
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
                }
                (Some(b), None, Some(t)) if b == t => {}
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
    let resolution = resolve_three_way(base.as_ref(), ours.as_ref(), theirs.as_ref(), context)?;
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
        for side in [left, right] {
            match side {
                Some(entry) if entry.is_tree() => {
                    let mut sub = SubtreeDiff::default();
                    pruned_subtree_diff(source, path, None, Some(entry), &mut sub)?;
                    leaves += sub.changed_leaves;
                }
                Some(_) => leaves += 1,
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
    };
    incremental_merge_walk(source, Path::new(""), sides, context, &mut out)?;
    // `files_changed` for adopted subtrees: one pruned diff each, against what
    // ours had there.
    let adopted = std::mem::take(&mut out.adopted_from_theirs);
    for (dir, replaced, adopted_entry) in &adopted {
        let mut scan = SubtreeDiff::default();
        pruned_subtree_diff(source, dir, *replaced, Some(*adopted_entry), &mut scan)?;
        out.changed_paths += scan.changed_leaves;
    }
    out.adopted_from_theirs = adopted;
    let candidates = std::mem::take(&mut out.df_candidates);
    // A carried subtree beneath a collision has to be read to know whether it
    // holds a file at all (only collisions pay this).
    let mut has_file = |id: &ObjectHash| subtree_holds_a_file(source, id);
    let delta = resolve_df_conflicts(
        &mut out.merged,
        &mut out.conflicts,
        candidates,
        &mut has_file,
    )?;
    out.changed_paths = out.changed_paths.saturating_add_signed(delta);
    Ok((out, passthrough))
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
) -> Result<PullMergeSummary, PullMergeError> {
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
    let (walk, passthrough_gitlinks) = incremental_merge_trees(
        &mut source,
        base_tree,
        ours_tree,
        theirs_tree,
        &mut TreeMergeContext::top_level(!options.dry_run, options.favor, &mut virtual_blobs),
    )?;
    let files_changed = walk.changed_paths;
    let introduced: HashSet<PathBuf> = walk
        .adopted_from_theirs
        .iter()
        .map(|(dir, _, _)| dir.clone())
        .collect();
    let mut merged_items = walk.merged;
    let mut conflicts = walk.conflicts;

    if options.dry_run {
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
        let conflict_style = conflict_style_from_config().await.map_err(|e| match e {
            ConflictStyleError::Invalid(value) => PullMergeError::InvalidConflictStyle(value),
            ConflictStyleError::Read(detail) => PullMergeError::ConflictStyleRead(detail),
        })?;
        let (base_items, _) = match base_tree {
            Some(id) => split_gitlink_entries(tree_leaves(&mut source, id)?),
            None => (HashMap::new(), GitlinkEntries::new()),
        };
        // The SAME resolved roots the walk merged, so the stage-2/stage-3
        // entries name what was actually merged (a replaced root included).
        let (our_items, _) = split_gitlink_entries(tree_leaves(&mut source, ours_tree)?);
        let (their_items, _) = split_gitlink_entries(tree_leaves(&mut source, theirs_tree)?);
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
        // and Git prints nothing when it does.
        announce_df_conflicts(&placements, upstream, options.output);
        if let Err(error) = crate::command::rerere::auto_update(false).await {
            tracing::warn!("rerere auto-update after merge conflict failed: {error}");
        }
        let paths = MergeState::load_required()?.conflicted_paths.join(", ");
        return Err(PullMergeError::Conflicts { paths });
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

    // No readability pass over adopted subtrees here — see the invariant on
    // `incremental_merge_trees`: every tree the walk left unopened is one HEAD
    // already references, so the result cannot introduce an unreadable tree
    // the flattening path would have caught; the gate has already opened (and
    // therefore validated) every tree the merge newly brings in.
    let tree_id = create_tree_from_items_map(&merged_items).map_err(PullMergeError::TreeCreate)?;

    if options.squash {
        report_incremental_walk_stats();
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
        match resolve_three_way(base, ours, theirs, context)? {
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
        // The directory kept the original path; `path` here is already the
        // file's `unique_path`, and its content is written verbatim — Git
        // moves the file, it does not mark it up.
        ConflictKind::FileDirectory { file, .. } => {
            let blob: Blob = load_object(&file.hash).map_err(|error| error.to_string())?;
            blob.data
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
    let base_marker = format!("{bars} base");

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
    fn strategy_option_resolves_add_add_and_modify_delete_paths() {
        let base = merge_entry(1, TreeItemMode::Blob);
        let ours = merge_entry(2, TreeItemMode::Blob);
        let theirs = merge_entry(3, TreeItemMode::Blob);
        let mut no_virtual_blobs = VirtualBlobs::new();
        let mut favored = |base, ours, theirs, favor| {
            let mut context =
                TreeMergeContext::top_level(false, Some(favor), &mut no_virtual_blobs);
            resolve_three_way(base, ours, theirs, &mut context).expect("favored resolution")
        };

        assert!(matches!(
            favored(None, Some(&ours), Some(&theirs), MergeFavor::Ours),
            MergeResolution::Use(entry) if entry == ours
        ));
        assert!(matches!(
            favored(None, Some(&ours), Some(&theirs), MergeFavor::Theirs),
            MergeResolution::Use(entry) if entry == theirs
        ));
        assert!(matches!(
            favored(Some(&base), Some(&ours), None, MergeFavor::Ours),
            MergeResolution::Use(entry) if entry == ours
        ));
        assert!(matches!(
            favored(Some(&base), Some(&ours), None, MergeFavor::Theirs),
            MergeResolution::Delete
        ));
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
            &mut TreeMergeContext::top_level(true, None, &mut no_virtual_blobs),
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
    use std::{collections::HashMap, path::PathBuf};

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
        merge_bases_of_folded, merge_input_exceeds_xdiff_size, merge_input_is_binary,
        merge_virtual_items, recorded_merge_base, virtual_base_fold_order, virtual_merged_mode,
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
            false,
            diffy::ConflictStyle::Merge,
            blobs,
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
            false,
            diffy::ConflictStyle::Merge,
            &mut blobs,
        )
        .expect_err("one level past the ceiling is refused");
        assert!(matches!(refused, PullMergeError::VirtualAncestorTooDeep));

        let attempted = fold_merge_bases(
            &bases,
            &GitlinkEntries::new(),
            MAX_VIRTUAL_ANCESTOR_DEPTH,
            false,
            diffy::ConflictStyle::Merge,
            &mut blobs,
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
            false,
            diffy::ConflictStyle::Merge,
            &mut blobs,
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
            false,
            diffy::ConflictStyle::Merge,
            &mut blobs,
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
    /// (`merge-ort.c:5429`).
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
    /// version (`merge-recursive.c`, `handle_change_delete`).
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

    /// Git's mode rule for a conflicted content merge
    /// (`merge-recursive.c`, `merge_mode_and_contents`).
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
            &mut TreeMergeContext::top_level(false, None, &mut blobs),
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
        incremental_merge_trees(
            graph,
            base,
            ours,
            theirs,
            &mut TreeMergeContext::top_level(false, None, &mut blobs),
        )
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
