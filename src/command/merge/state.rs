//! Merge sidecar persistence and request-scoped pseudo-ref projection.

use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use super::{MergeStrategy, PullMergeError};
use crate::utils::util;

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
    let mut oids = vec![("orig_head", state.orig_head)];
    if state.targets.is_empty() {
        oids.push(("target", state.target));
    } else {
        oids.extend(state.targets.into_iter().map(|target| ("target", target)));
    }
    if let Some(base) = state.base {
        oids.push(("base", base));
    }
    if let Some(auto_merge) = state.auto_merge {
        oids.push(("auto_merge", auto_merge));
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
    /// Every octopus target in parent order. Empty for legacy and single-head
    /// states, where `target` remains authoritative.
    #[serde(default)]
    pub targets: Vec<String>,
    /// Display refs corresponding to `targets`; status and reflog continue to
    /// use the backward-compatible joined `target_ref` field.
    #[serde(default)]
    pub target_refs: Vec<String>,
    /// Common ancestor used by the three-way merge, when it is a real commit.
    /// `None` represents the virtual empty base used by
    /// `--allow-unrelated-histories` AND the recursive virtual ancestor of a
    /// criss-cross merge, which is a one-shot object deliberately left out of
    /// this file so it is not a GC root (ADR-MG-04, see [`super::recorded_merge_base`]).
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
    /// Resolved signing decision from the invocation that wrote this state.
    /// Older sidecars omit it and are resolved through current configuration
    /// when they are continued.
    #[serde(default)]
    pub signing_policy: Option<crate::command::history_config::CommitSigningPolicy>,
    /// Whether the invocation requested `--signoff`. Persisted so conflict and
    /// `--no-commit` continuations retain the original trailer decision.
    #[serde(default)]
    pub signoff: bool,
    /// Explicit rerere staging choice from the invocation that wrote this state.
    /// Older sidecars omit it and therefore inherit `rerere.autoUpdate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerere_autoupdate: Option<bool>,
    pub conflicted_paths: Vec<String>,
    /// Merge message resolved at merge start (`-m` override or the generated
    /// default including the `merge.log` shortlog), replayed verbatim by
    /// `merge --continue`. `None` for states written by older binaries, which
    /// fall back to the plain `Merge <target> into <head>` form.
    #[serde(default)]
    pub message: Option<String>,
    /// The automatic merge-result tree projected as `AUTO_MERGE` while this
    /// non-squash conflict state exists. Older sidecars omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_merge: Option<String>,
}

impl MergeState {
    pub(super) fn path() -> PathBuf {
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

    pub(super) fn load_required() -> Result<Self, PullMergeError> {
        Self::load_optional_sync()
            .map_err(PullMergeError::StateLoad)?
            .ok_or(PullMergeError::NoMergeInProgress)
    }

    pub(super) fn save(&self) -> Result<(), PullMergeError> {
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

    pub(super) fn cleanup() -> Result<(), PullMergeError> {
        let path = Self::path();
        // Durable (§C.10): a resurrected merge-state replays a merge the user
        // already concluded — same stakes as the stash log.
        crate::utils::atomic_write::remove_durably(&path)
            .map_err(|error| PullMergeError::StateCleanup(format!("{}: {error}", path.display())))
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
