//! Held merge autostash sidecar and recovery lifecycle.

use std::{fs, path::PathBuf, str::FromStr};

use git_internal::hash::ObjectHash;
use serde::{Deserialize, Serialize};

use super::{MergeError, MergeState, PullMergeError, PullMergeOptions};
use crate::{
    internal::{config::ConfigKv, head::Head},
    utils::{output::OutputConfig, util},
};

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
pub(super) struct AutostashSnapshot {
    pub(super) sidecar: MergeAutostash,
    pub(super) recorded_owner: Option<String>,
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

/// The ownership matrix every held-autostash CONSUMER must pass (W2,
/// ADR-0714-08) before applying or promoting the sidecar — both adopt its
/// commit into user-visible state and then delete the evidence.
///
/// * recorded owner == this scope → operable (proven ours);
/// * recorded owner == some OTHER scope → refused (a copied/moved file);
/// * no record, MAIN scope, linked-worktree history → refused (an old
///   binary's common-storage file could be a removed linked worktree's);
/// * no record otherwise → operable (a W1-era file in an unambiguous gitdir).
pub(super) fn verify_autostash_ownership(recorded: Option<&str>) -> Result<(), String> {
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

/// In-progress merge (and optional held autostash) captured before a user
/// `reset` writes HEAD/index. ADR-HF-03 item 5 (#477 HF-26).
pub(crate) struct StoppedMerge {
    merge_state_bytes: Option<Vec<u8>>,
    autostash: Option<AutostashSnapshot>,
}

/// Snapshot merge-state.json and merge-autostash.json before a whole-tree
/// reset, so conclusion can only ever finish the merge this reset observed.
pub(crate) fn snapshot_stopped_merge() -> Result<Option<StoppedMerge>, String> {
    let path = MergeState::path();
    let merge_state_bytes = if path.exists() {
        Some(
            fs::read(&path)
                .map_err(|error| format!("failed to read {}: {error}", path.display()))?,
        )
    } else {
        None
    };
    let autostash = snapshot_held_autostash()?;
    if merge_state_bytes.is_none() && autostash.is_none() {
        return Ok(None);
    }
    Ok(Some(StoppedMerge {
        merge_state_bytes,
        autostash,
    }))
}

fn merge_autostash_promote_warning(detail: &str) -> String {
    format!(
        "reset completed, but the merge autostash could not be moved into the stash list \
         ({detail}); merge-state.json and merge-autostash.json were left in place. \
         Recover the local changes with `libra merge --abort`."
    )
}

fn merge_reset_promote_fail_injected() -> bool {
    std::env::var_os("LIBRA_TEST").is_some()
        && std::env::var("LIBRA_TEST_MERGE_AUTOSTASH_PROMOTE")
            .ok()
            .as_deref()
            == Some("fail")
}

/// After a successful user reset: promote a held merge autostash into the
/// stash list first, then drop merge-state.json. Promotion failure leaves
/// both sidecars and stops later sequence conclusions (ADR-HF-03 item 5).
pub(crate) async fn conclude_stopped_merge(snapshot: StoppedMerge) -> Result<Vec<String>, String> {
    let mut notes = Vec::new();
    if let Some(autostash) = snapshot.autostash {
        if merge_reset_promote_fail_injected() {
            return Err(merge_autostash_promote_warning("test-injected failure"));
        }
        let oid = ObjectHash::from_str(&autostash.sidecar.stash_commit).map_err(|error| {
            merge_autostash_promote_warning(&format!("invalid stash OID ({error})"))
        })?;
        crate::command::stash::store_stash_commit(&oid, "autostash")
            .await
            .map_err(|error| merge_autostash_promote_warning(&error.to_string()))?;
        match cleanup_autostash_if_matches(&autostash) {
            Ok(true) => {}
            Ok(false) => {
                return Err(merge_autostash_promote_warning(
                    "the autostash sidecar changed while it was being promoted",
                ));
            }
            Err(error) => return Err(merge_autostash_promote_warning(&error)),
        }
        notes.push(
            "Your changes are safe in the stash (stash@{0}).\nAfter resolving any conflicts, run \"libra stash pop\" to restore your local changes."
                .to_string(),
        );
    }

    if let Some(expected) = snapshot.merge_state_bytes {
        let path = MergeState::path();
        let current = if path.exists() {
            fs::read(&path).map_err(|error| {
                format!(
                    "reset completed, but merge-state.json could not be re-read ({error}); \
                     it was left in place. Finish or abort the merge with `libra merge --abort`."
                )
            })?
        } else {
            Vec::new()
        };
        if current == expected {
            MergeState::cleanup().map_err(|error| {
                format!(
                    "reset completed, but merge-state.json could not be removed ({error}); \
                     it was left in place. Finish or abort the merge with `libra merge --abort`."
                )
            })?;
        }
    }

    if let Err(error) =
        crate::internal::sequencer::clear(crate::internal::sequencer::SequenceKind::Merge).await
    {
        notes.push(format!(
            "reset cleared merge-state.json, but the merge sequencer row could not be removed ({error})"
        ));
    }
    Ok(notes)
}

/// After a user `commit` that finished the merge: drop merge-state.json, then
/// apply a held autostash onto the new tree (ADR-HF-21).
pub(crate) async fn conclude_merge_after_commit(
    snapshot: StoppedMerge,
    output: &OutputConfig,
) -> Result<Vec<String>, String> {
    let mut notes = Vec::new();
    if let Some(expected) = snapshot.merge_state_bytes {
        let path = MergeState::path();
        let current = if path.exists() {
            fs::read(&path).map_err(|error| {
                format!(
                    "commit completed, but merge-state.json could not be re-read ({error}); \
                     it was left in place. Finish or abort the merge with `libra merge --abort`."
                )
            })?
        } else {
            Vec::new()
        };
        if current == expected {
            MergeState::cleanup().map_err(|error| {
                format!(
                    "commit completed, but merge-state.json could not be removed ({error}); \
                     it was left in place. Finish or abort the merge with `libra merge --abort`."
                )
            })?;
        }
    }
    if let Err(error) =
        crate::internal::sequencer::clear(crate::internal::sequencer::SequenceKind::Merge).await
    {
        notes.push(format!(
            "commit cleared merge-state.json, but the merge sequencer row could not be removed ({error})"
        ));
    }
    if let Some(autostash) = snapshot.autostash
        && let Some(status) = resolve_pending_autostash_with(output, autostash, false).await
    {
        notes.push(format!("merge autostash: {status}"));
    }
    Ok(notes)
}

/// Finalize a held autostash after a merge action. If the sidecar exists
/// and no merge is in progress, re-apply the stash. A clean apply drops the
/// sidecar; an apply conflict promotes the stash into refs/stash with a notice.
/// Other apply errors leave the sidecar in place and emit a warning. While
/// merge state persists, the stash remains held.
pub(super) async fn resolve_pending_autostash(
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
pub(super) async fn resolve_pending_autostash_with(
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

pub(super) async fn store_pending_autostash(
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

/// Start the shared merge autostash lifecycle after every semantic preflight
/// has passed. The durable ordering is objects -> sidecar -> worktree reset;
/// callers must later invoke [`resolve_pending_autostash`] on every outcome.
pub(super) async fn prepare_merge_autostash(
    options: &PullMergeOptions,
    output: &OutputConfig,
) -> Result<(), PullMergeError> {
    let held_snapshot = if options.preserve_held_autostash || options.dry_run {
        None
    } else {
        snapshot_held_autostash().map_err(PullMergeError::Autostash)?
    };
    if let Some(snapshot) = held_snapshot {
        let sidecar = &snapshot.sidecar;
        verify_autostash_ownership(snapshot.recorded_owner.as_deref())
            .map_err(PullMergeError::Autostash)?;
        if let Ok(oid) = ObjectHash::from_str(&sidecar.stash_commit) {
            crate::command::stash::store_stash_commit(&oid, "autostash")
                .await
                .map_err(|error| {
                    PullMergeError::Autostash(format!(
                        "cannot recover the leftover autostash: {error}"
                    ))
                })?;
            match cleanup_autostash_if_matches(&snapshot) {
                Ok(true) => {}
                Ok(false) => crate::utils::error::emit_warning(
                    "the autostash sidecar changed while it was being recovered; the newer file was left in place",
                ),
                Err(error) => {
                    return Err(PullMergeError::Autostash(format!(
                        "the recovered autostash's sidecar could not be removed: {error}"
                    )));
                }
            }
            crate::utils::error::emit_warning(
                "recovered a leftover autostash into the stash list (it may duplicate already-restored changes — inspect with 'libra stash show')",
            );
        } else {
            return Err(PullMergeError::Autostash(
                "merge-autostash.json holds an invalid OID; inspect and remove it".to_string(),
            ));
        }
    }

    if autostash_enabled(options).await? && Head::current_commit().await.is_some() {
        match crate::command::stash::create_held_stash_commit("autostash").await {
            Ok(Some(stash_commit)) => {
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
            Ok(None) => {}
            Err(error) => return Err(PullMergeError::Autostash(error.to_string())),
        }
    }
    Ok(())
}

/// Preflight for the merge control actions (W2 r5 #1, r6 #4): the
/// held-autostash sidecar must be READABLE before HEAD/index/worktree are
/// restored — an unreadable one would otherwise surface after `abort` had
/// already deleted the merge state, leaving the only stash reference
/// unparseable. The SNAPSHOT is returned and later CONSUMED, so the document
/// the control preflighted is the one applied: a sidecar replaced between
/// the preflight and the consumption is preserved by the identity-checked
/// cleanup, never adopted or deleted.
pub(super) fn preflight_held_autostash() -> Result<Option<AutostashSnapshot>, MergeError> {
    snapshot_held_autostash().map_err(MergeError::StateLoad)
}
