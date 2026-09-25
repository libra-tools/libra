//! Worktree registry schema, parsing, read/write paths, and state invariants.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::{WorktreeError, WorktreeResult};
use crate::utils::util;

/// A single worktree entry persisted in `worktrees.json` (registry v2,
/// plan-20260714 §C.7).
///
/// `path` is always stored as a canonical absolute path. `worktree_id` is
/// the STABLE per-worktree identity (None for main, whose scope is NULL) —
/// persisted so `worktree repair <path>` can restore a corrupt/missing
/// `.libra/worktree_id` from the registry instead of guessing.
///
/// `pub(crate)` so the service dirty-mark gate deserializes the registry with
/// this exact schema (a drifting mirror would fail open on missing fields).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct WorktreeEntry {
    pub(super) path: String,
    pub(super) is_main: bool,
    pub(super) locked: bool,
    pub(super) lock_reason: Option<String>,
    /// Stable worktree identity (v2). `None` for the main worktree. Old v1
    /// entries lack it; the v1→v2 upgrade backfills from each worktree's
    /// gitdir (or the canonical-path synthesis fallback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) worktree_id: Option<String>,
    /// Lifecycle state (W3-s1b). Absent in files written before v0.19.58;
    /// serde defaults it to `Active`.
    #[serde(default, skip_serializing_if = "WorktreeEntryState::is_active")]
    pub(super) state: WorktreeEntryState,
    /// Registration generation (W1 §C.4.1.1 service fence).
    ///
    /// Instance ids are PATH-DERIVED, so a worktree removed and re-added at
    /// the same place has the same id and the same path as its predecessor —
    /// which means neither can fence a request that was in flight across the
    /// re-add. The epoch is bumped for every registration, so a client holding
    /// the old one is refused instead of marking the successor's cache.
    /// Absent in files written before this field existed; `0` then, which no
    /// live registration uses.
    #[serde(default, skip_serializing_if = "is_zero_epoch")]
    pub(super) epoch: u64,
}

pub(super) fn is_zero_epoch(epoch: &u64) -> bool {
    *epoch == 0
}

/// What this registry can say about linked-worktree HISTORY (W1 §C.4.3).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LinkedHistory {
    /// No linked worktree has ever been registered — asserted only by a
    /// registry this binary created and has maintained since.
    #[default]
    Never,
    /// A linked worktree once existed.
    Existed,
    /// Unknowable: the registry was promoted from a pre-v3 shape, whose
    /// removals left no trace. Treated as evidence by every ambiguous-sidecar
    /// decision.
    Unknown,
}

impl LinkedHistory {
    fn is_never(&self) -> bool {
        matches!(self, Self::Never)
    }
}

/// Lifecycle state of a registry entry (W3-s1b, §C.7).
///
/// * `Active` — a normal registered worktree (default; not serialized).
/// * `DetachedFromRegistry` — `worktree remove` (keep-dir) unregistered the
///   directory: its scoped DB rows are KEPT (the directory still holds the
///   user's files and would otherwise lose its HEAD), and every command in
///   that directory fails closed via the gitdir marker until re-add or
///   `--delete-dir` completes.
/// * `Tombstone` — `--delete-dir` durably deleted the directory but the
///   scoped-row cleanup failed; `worktree repair` retries it. Only a
///   tombstone proves the directory is gone, letting GC stop treating its
///   private index as a potential root.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorktreeEntryState {
    #[default]
    Active,
    DetachedFromRegistry,
    Tombstone,
}

impl WorktreeEntryState {
    pub(super) fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }

    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::DetachedFromRegistry => "detached_from_registry",
            Self::Tombstone => "tombstone",
        }
    }
}

/// Gitdir marker file that fail-closes every command inside a
/// detached-from-registry worktree (checked by the storage resolver).
pub(crate) const DETACHED_MARKER: &str = "detached_from_registry";

/// Registry schema version this binary reads and writes.
/// The on-disk `worktrees.json` schema.
///
/// v3 adds the durable registration-generation counter (`epoch_counter`) and
/// each entry's `epoch` — the §C.4.1.1 service fence. Bumped rather than
/// carried on `#[serde(default)]` alone: a v2-era binary would parse a v3 file,
/// drop the counter on rewrite, and the next registration would reissue a
/// generation a live client is still fenced on. Migration 2026073005 records
/// the matching capability marker so those binaries refuse the repository at
/// connect time instead.
pub(super) const REGISTRY_SCHEMA_VERSION: u32 = 3;

/// Top-level registry v2 persisted in `worktrees.json` (plan-20260714 §C.7).
///
/// v2 deliberately renames the top-level array to `entries`: a v1 binary's
/// `{ worktrees: Vec<_> }` parser FAILS on a v2 file (missing field) instead
/// of silently reading stale data and rewriting it — the second belt behind
/// the SQLite capability marker that already refuses old binaries at
/// connect time (future-schema fail-closed).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct WorktreeState {
    pub(crate) schema_version: u32,
    pub(crate) entries: Vec<WorktreeEntry>,
    /// Whether a linked worktree has ever existed in this repository (W1
    /// §C.4.3). `Never` is only ever written by a registry this binary
    /// created; a promoted pre-v3 file records `Unknown`, because its history
    /// cannot be reconstructed.
    #[serde(default, skip_serializing_if = "LinkedHistory::is_never")]
    pub(crate) linked_history: LinkedHistory,
    /// The highest registration generation this registry has ever handed out
    /// (W1 §C.4.1.1 service fence).
    ///
    /// Kept at the REGISTRY level, not derived from the entries, because the
    /// case the fence exists for is a worktree that was REMOVED: its entry is
    /// gone, so a maximum over the survivors would hand its generation out
    /// again and a client fenced on it would be served.
    #[serde(default, skip_serializing_if = "is_zero_epoch")]
    pub(crate) epoch_counter: u64,
}

impl Default for WorktreeState {
    fn default() -> Self {
        Self {
            schema_version: REGISTRY_SCHEMA_VERSION,
            entries: Vec::new(),
            linked_history: LinkedHistory::Never,
            epoch_counter: 0,
        }
    }
}

/// The LEGACY v1 on-disk shape, parsed only to upgrade in place.
#[derive(Deserialize)]
pub(super) struct WorktreeStateV1 {
    worktrees: Vec<WorktreeEntryV1>,
}

#[derive(Deserialize)]
struct WorktreeEntryV1 {
    path: String,
    is_main: bool,
    locked: bool,
    lock_reason: Option<String>,
}

/// Which on-disk shape a registry file parsed as (v1 files are upgraded
/// in memory; only the LOCKED loader persists the upgrade).
pub(super) enum RegistryShape {
    V2,
    V1,
}

impl WorktreeState {
    /// Parse registry bytes, discriminating on the top-level shape BEFORE
    /// choosing a parser (fail-closed): a document carrying any v2 key
    /// (`schema_version`/`entries`) must be a fully valid, supported v2
    /// registry — it never falls through to the lenient v1 reader, so a
    /// malformed or future v2 file cannot be misread as (and rewritten from)
    /// a stale embedded `worktrees` array. A pure v1 document upgrades in
    /// memory with ids unfilled; anything else is corrupt.
    pub(super) fn parse_document(data: &[u8]) -> Result<(Self, RegistryShape), String> {
        let document: serde_json::Value = serde_json::from_slice(data)
            .map_err(|error| format!("registry parse failed: {error}"))?;
        let Some(object) = document.as_object() else {
            return Err("registry root is not a JSON object".to_string());
        };
        let has_v2_keys = object.contains_key("schema_version") || object.contains_key("entries");
        let has_v1_keys = object.contains_key("worktrees");
        if has_v2_keys && has_v1_keys {
            return Err(
                "registry mixes v2 (`schema_version`/`entries`) and legacy v1 (`worktrees`) \
                  keys; refusing the ambiguous file"
                    .to_string(),
            );
        }
        if has_v2_keys {
            let state: WorktreeState = serde_json::from_value(document)
                .map_err(|error| format!("registry v2 parse failed: {error}"))?;
            // v2 files are READ as-is and upgraded in place by the mutation
            // loader: their entries simply have no generations yet, which is
            // what `epoch = 0` means. A version we do not know is refused.
            if state.schema_version != REGISTRY_SCHEMA_VERSION && state.schema_version != 2 {
                return Err(format!(
                    "unsupported registry schema_version {}",
                    state.schema_version
                ));
            }
            let mut state = state;
            if state.schema_version != REGISTRY_SCHEMA_VERSION {
                // Promoting a PRE-v3 registry: whether this repository ever had
                // a linked worktree is unknowable from it — a worktree removed
                // before v3 left no entry and no generation. Recording that
                // explicitly is the difference between "never had one" and "we
                // cannot tell", and the ambiguous-sidecar rules (§C.4.3) must
                // treat the second as evidence. Without this, a repository
                // whose only linked worktree was removed pre-v3 would
                // auto-adopt and DELETE that worktree's old common state.
                state.linked_history = LinkedHistory::Unknown;
            }
            state.schema_version = REGISTRY_SCHEMA_VERSION;
            return Ok((state, RegistryShape::V2));
        }
        if has_v1_keys {
            let legacy: WorktreeStateV1 = serde_json::from_value(document)
                .map_err(|error| format!("registry v1 parse failed: {error}"))?;
            return Ok((
                WorktreeState {
                    schema_version: REGISTRY_SCHEMA_VERSION,
                    entries: legacy
                        .worktrees
                        .into_iter()
                        .map(|entry| WorktreeEntry {
                            path: entry.path,
                            is_main: entry.is_main,
                            locked: entry.locked,
                            lock_reason: entry.lock_reason,
                            worktree_id: None,
                            state: WorktreeEntryState::Active,
                            epoch: 0,
                        })
                        .collect(),
                    // A v1 file predates every generation too.
                    linked_history: LinkedHistory::Unknown,
                    epoch_counter: 0,
                },
                RegistryShape::V1,
            ));
        }
        Err("registry has neither an `entries` (v2) nor a `worktrees` (v1) array".to_string())
    }

    /// Parse registry bytes accepting BOTH the v2 shape and the legacy v1
    /// shape (read-only in-memory upgrade; ids stay unfilled — read-side
    /// consumers like the service dirty-mark gate and rerere's
    /// linked-evidence probe only inspect `is_main`). The locked loader
    /// performs the durable v1→v2 upgrade separately.
    pub(crate) fn parse(data: &[u8]) -> Result<Self, String> {
        let (state, shape) = Self::parse_document(data)?;
        match shape {
            RegistryShape::V2 => state.validate_v2()?,
            // v1 has no persisted ids, but the structural main-entry
            // invariant still applies: read-side consumers (the service
            // dirty gate, rerere's evidence probe, the rejected-cleanup
            // snapshot) must fail closed on a mainless/multi-main document
            // instead of consuming it as an empty or ambiguous root set.
            // Only the LOCKED worktree loaders (which go through
            // `parse_document` directly) may repair the main entry.
            RegistryShape::V1 => state.validate_main_count()?,
        }
        Ok(state)
    }

    /// v2 identity invariants (§C.7): the registry is the persisted identity
    /// AUTHORITY — main carries no id, every linked entry carries a non-empty
    /// one. A v2 file violating this is corrupt and must be refused, never
    /// silently patched from the mutable gitdir.
    fn validate_v2(&self) -> Result<(), String> {
        self.validate_main_count()?;
        for entry in &self.entries {
            if entry.is_main {
                if entry.worktree_id.is_some() {
                    return Err(format!(
                        "main worktree entry '{}' must not carry a worktree_id",
                        entry.path
                    ));
                }
                if !entry.state.is_active() {
                    return Err(format!(
                        "main worktree entry '{}' must be active, not {}",
                        entry.path,
                        entry.state.as_str()
                    ));
                }
            } else if entry
                .worktree_id
                .as_deref()
                .is_none_or(|id| id.trim().is_empty())
            {
                return Err(format!(
                    "linked worktree entry '{}' is missing its persisted worktree_id",
                    entry.path
                ));
            }
        }
        Ok(())
    }

    /// Every registered worktree path (main and linked), for read-side
    /// consumers that only need the paths (e.g. the rejected-object-cleanup
    /// index snapshot).
    pub(crate) fn entry_paths(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect()
    }

    /// Structural invariant shared by BOTH shapes: exactly one main entry.
    fn validate_main_count(&self) -> Result<(), String> {
        let main_count = self.entries.iter().filter(|entry| entry.is_main).count();
        if main_count != 1 {
            return Err(format!(
                "registry must contain exactly one main worktree entry (found {main_count})"
            ));
        }
        Ok(())
    }

    /// True when the registry holds exactly the main worktree entry — the
    /// only shape under which a scope-less service dirty-mark may default
    /// to the main scope. Anything else (empty, multi-entry, or a sole
    /// non-main entry) is indistinguishable from corruption or a
    /// multi-worktree layout and must fail closed.
    pub(crate) fn is_single_main(&self) -> bool {
        matches!(self.entries.as_slice(), [entry] if entry.is_main)
    }

    /// Whether a linked worktree has EVER existed here (§C.4.3).
    ///
    /// The single answer behind every ambiguous-sidecar decision. A live linked
    /// entry, a recorded history other than `never`, or a non-zero generation
    /// counter all count — and so does the `Unknown` a pre-v3 registry is
    /// promoted to on load, because a worktree removed before v3 left nothing
    /// to read.
    pub(crate) fn ever_had_linked_worktree(&self) -> bool {
        self.entries.iter().any(|entry| !entry.is_main)
            || !self.linked_history.is_never()
            || self.epoch_counter > 0
    }

    /// Registry identity invariants beyond the per-entry shape: linked ids are
    /// unique, and none of them is a reserved spelling.
    ///
    /// `main` is reserved because callers address the main worktree by name in
    /// places where only a string can travel; a linked worktree literally
    /// called `main` would let a corrupt registry redirect an explicit
    /// main-scope request into it.
    /// The generation to stamp on the next registration: one past every epoch
    /// the registry has ever handed out, INCLUDING tombstoned and detached
    /// entries, so a re-add at a freed path never reuses a live client's fence.
    pub(crate) fn next_epoch(&mut self) -> u64 {
        let issued = self
            .entries
            .iter()
            .map(|entry| entry.epoch)
            .max()
            .unwrap_or(0)
            .max(self.epoch_counter)
            .saturating_add(1);
        self.epoch_counter = issued;
        issued
    }

    /// [`Self::identity_conflict`] extended with an id about to be ADDED.
    pub(crate) fn identity_conflict_with(&self, candidate: &str) -> Option<String> {
        if candidate == "main" {
            return Some("a linked worktree may not use the reserved id 'main'".to_string());
        }
        if self
            .entries
            .iter()
            .any(|entry| !entry.is_main && entry.worktree_id.as_deref() == Some(candidate))
        {
            return Some(format!(
                "another registry entry already uses the id '{candidate}'"
            ));
        }
        self.identity_conflict()
    }

    pub(crate) fn identity_conflict(&self) -> Option<String> {
        let mut seen: Vec<&str> = Vec::new();
        for entry in &self.entries {
            if entry.is_main {
                continue;
            }
            // Only a LIVE registration claims an identity. A detached or
            // tombstoned entry keeps its id so its rows stay attributable and
            // a re-add can identity-check against it, but it is not competing
            // for the scope — which is what makes detaching one side the way
            // out of a collision.
            if !entry.state.is_active() {
                continue;
            }
            let Some(id) = entry.worktree_id.as_deref() else {
                continue;
            };
            if id == "main" {
                return Some("a linked worktree may not use the reserved id 'main'".to_string());
            }
            if seen.contains(&id) {
                return Some(format!("two linked worktrees share the id '{id}'"));
            }
            seen.push(id);
        }
        None
    }
}

impl WorktreeEntry {
    /// Whether this entry is the main worktree.
    pub(crate) fn is_main(&self) -> bool {
        self.is_main
    }

    /// The stable worktree identity (`None` for main, and for v1 entries that
    /// predate the field).
    pub(crate) fn worktree_id(&self) -> Option<&str> {
        self.worktree_id.as_deref()
    }

    /// The path this entry is registered at.
    pub(crate) fn registered_path(&self) -> &str {
        &self.path
    }

    /// This registration's generation — see the field.
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Whether this entry is a LIVE registration (not detached or tombstoned).
    pub(crate) fn is_active(&self) -> bool {
        self.state.is_active()
    }
}

pub(super) fn state_path() -> PathBuf {
    util::storage_path().join("worktrees.json")
}

/// Loads the current `WorktreeState` from disk, ensuring a main worktree entry.
///
/// If the state file does not exist, this function initializes a fresh state
/// with a single main worktree derived from the storage path and persists it
/// before returning; an existing zero-byte file is refused as a torn write.
/// Load the registry for MUTATION. The caller MUST hold the registry lock
/// (`acquire_registry_lock`): this variant performs DURABLE repairs — it
/// creates a MISSING registry (an existing zero-byte file is refused as a
/// torn write), rewrites a legacy v1 file as v2 with
/// each linked entry's stable id backfilled, and persists main-entry fixes.
/// A v2 file violating the identity invariants is REFUSED (only the
/// explicit no-arg `worktree repair` may heal it — see
/// `load_state_for_repair`). Lockless readers use `load_state_readonly`
/// instead (a lockless writer could overwrite a concurrent locked mutation).
pub(super) fn load_state() -> WorktreeResult<WorktreeState> {
    let state = load_state_impl(false)?;
    // Cross-entry identity invariants are checked HERE rather than in the
    // parser: a registry an older binary could produce (`add A` → `move A B`
    // → `add A`, where move keeps A's path-derived id) has duplicate ids, and
    // refusing it at parse time would take `worktree list` and `worktree
    // doctor` down with every mutation — leaving the user no way to even see
    // the problem. Reads load it and report; MUTATIONS refuse, because there
    // is no fact of the matter about which worktree they would act on.
    if let Some(conflict) = state.identity_conflict() {
        return Err(WorktreeError::OperationBlocked(format!(
            "the worktree registry is ambiguous: {conflict}. Run `libra worktree doctor` to see \
              which entries collide, then \
              `libra worktree repair <path> --resolve-identity --yes` to unregister the one you \
              do not want (the directory is left on disk)"
        )));
    }
    Ok(state)
}

/// The no-arg `worktree repair` loader: like `load_state` but HEALS v2
/// identity-invariant violations instead of refusing them — the user
/// explicitly asked for a repair, so a main entry's stray id is cleared and
/// a linked entry's missing id is deterministically backfilled from its
/// gitdir (or the canonical-path synthesis fallback) and persisted.
pub(super) fn load_state_for_repair() -> WorktreeResult<WorktreeState> {
    load_state_impl(true)
}

fn load_state_impl(heal_identity_invariants: bool) -> WorktreeResult<WorktreeState> {
    let path = state_path();
    if !path.exists() {
        let mut state = WorktreeState::default();
        let _ = ensure_main_entry(&mut state)
            .map_err(|source| WorktreeError::StateRepair { source })?;
        write_state(&state)?;
        return Ok(state);
    }
    let data = fs::read(&path).map_err(|source| WorktreeError::StateRead {
        path: path.clone(),
        source,
    })?;
    if data.is_empty() {
        return Err(WorktreeError::StateCorrupt {
            path: path.clone(),
            source: "registry file exists but is EMPTY (torn write?); restore it from a \
                      backup, or delete it to let the next worktree command reinitialize \
                      a fresh registry"
                .to_string(),
        });
    }
    let (mut state, shape) =
        WorktreeState::parse_document(&data).map_err(|source| WorktreeError::StateCorrupt {
            path: path.clone(),
            source,
        })?;
    if !heal_identity_invariants && matches!(shape, RegistryShape::V2) {
        // A validated v2 file is used AS-IS: exactly-one-main and the id
        // invariants already hold, so ordinary mutators never silently
        // re-elect mains or rewrite ids — that authority belongs to the
        // explicit no-arg `worktree repair` alone (heal mode below).
        return match state.validate_v2() {
            Ok(()) => Ok(state),
            Err(source) => Err(WorktreeError::StateCorrupt {
                path: path.clone(),
                source: format!("{source}; run `libra worktree repair --confirm` to heal it"),
            }),
        };
    }
    // v1→v2 upgrade path (§C.7): a legacy `{ worktrees: [...] }` file is
    // parsed once, each linked entry's STABLE id backfilled from its gitdir
    // (or the canonical-path synthesis fallback), and the registry
    // rewritten as v2 — durably, and only here, under the registry lock.
    // Heal mode (no-arg repair) runs the same main-entry/id repairs on a v2
    // file whose invariants were violated.
    let mut dirty = matches!(shape, RegistryShape::V1);
    if ensure_main_entry(&mut state).map_err(|source| WorktreeError::StateRepair { source })? {
        dirty = true;
    }
    if normalize_v2_ids(&mut state) {
        dirty = true;
    }
    if dirty {
        write_state(&state)?;
    }
    Ok(state)
}

/// Read-only registry view for LOCKLESS consumers (`worktree list`): parses
/// both shapes and synthesizes a missing main entry IN MEMORY, but never
/// touches the file — the durable v1→v2 upgrade happens only in the locked
/// loader. Legacy v1 entries keep `worktree_id: None`; per-entry consumers
/// fall back to the gitdir probe.
/// Is `worktree_id` a linked worktree the registry actually knows about?
///
/// `current_worktree_id` SYNTHESIZES an id from the canonical path when the
/// `worktree_id` file is missing, empty, or unreadable. That fallback is
/// deliberately not `None` — aliasing to `Main` would graft this worktree
/// onto main's HEAD — but a synthesized id is a guess, and a mutation
/// performed under a guess writes rows no other process associates with this
/// worktree. Callers use this to refuse the mutation instead.
///
/// Returns `None` when the registry itself cannot be read: "unknown" is not
/// "absent", and a torn registry must not be reported as a corrupt identity.
pub(crate) fn registry_knows_linked_worktree(worktree_id: &str) -> Option<bool> {
    let cwd = std::env::current_dir().ok()?;
    let root = crate::internal::worktree_scope::RequestScope::resolve(cwd)
        .map(|request| request.worktree_root);
    registry_knows_linked_worktree_in_storage(&util::storage_path(), worktree_id, root.as_deref())
}

/// [`registry_knows_linked_worktree`] against an already-resolved common
/// storage root so `--cwd`/`--repo` and automation dispatch cannot consult
/// the process cwd's registry for a different worktree.
///
/// `worktree_root` is required: a copied `worktree_id` in an unregistered
/// directory must not pass just because some other active entry owns that
/// id. v2 entries match stored id + canonical path; lockless v1 readers
/// leave ids unset and match live gitdir id + registered path.
/// Detached/tombstone entries never count as registered.
pub(crate) fn registry_knows_linked_worktree_in_storage(
    storage: &std::path::Path,
    worktree_id: &str,
    worktree_root: Option<&std::path::Path>,
) -> Option<bool> {
    let state = load_state_readonly_at(&storage.join("worktrees.json")).ok()?;
    let Some(requested_root) = worktree_root.and_then(|path| canonicalize(path).ok()) else {
        return Some(false);
    };
    Some(state.entries.iter().any(|entry| {
        if entry.is_main || !entry.state.is_active() {
            return false;
        }
        let Ok(entry_root) = canonicalize(std::path::Path::new(&entry.path)) else {
            return false;
        };
        if entry_root != requested_root {
            return false;
        }
        match entry.worktree_id.as_deref() {
            Some(registered_id) => registered_id == worktree_id,
            None => resolve_worktree_id(&entry_root).as_deref() == Some(worktree_id),
        }
    }))
}

/// The LOCAL gitdir of an explicitly named scope (§C.5 pseudo-ref projection).
///
/// The pseudo-ref service is handed a scope and must read THAT worktree's
/// sidecars — the request pin answers for the worktree the user invoked from,
/// which is a different question whenever the two differ. Main resolves to the
/// common storage; a linked scope is looked up by its stable id in the
/// registry, so a scope naming a worktree this repository does not know is an
/// error rather than a silent fallback to main's files.
pub(crate) fn local_gitdir_for_scope(
    scope: &crate::internal::worktree_scope::WorktreeScope,
) -> Result<std::path::PathBuf, String> {
    use crate::internal::worktree_scope::WorktreeScope;
    let id = match scope {
        WorktreeScope::Main => {
            return util::try_get_storage_path(None)
                .map_err(|error| format!("cannot resolve the repository storage: {error}"));
        }
        WorktreeScope::Linked(id) => id.as_str(),
    };
    let state = load_state_readonly()
        .map_err(|error| format!("cannot read the worktree registry: {error}"))?;
    let entry = state
        .entries
        .iter()
        .find(|entry| entry.worktree_id.as_deref() == Some(id))
        .ok_or_else(|| format!("no worktree with id '{id}' is registered in this repository"))?;
    let gitdir = std::path::Path::new(&entry.path).join(util::ROOT_DIR);
    if !gitdir.exists() {
        return Err(format!(
            "worktree '{id}' is registered at '{}', but its gitdir is missing",
            entry.path
        ));
    }
    Ok(gitdir)
}

pub(super) fn load_state_readonly() -> WorktreeResult<WorktreeState> {
    load_state_readonly_at(&state_path())
}

/// [`load_state_readonly`] against an EXPLICIT registry path — for callers
/// that resolved their storage root once (§C.4.2) and must not let an
/// ambient re-resolution answer for a different repository (the GC root
/// enumeration binds everything to the request pin this way).
pub(super) fn load_state_readonly_at(path: &std::path::Path) -> WorktreeResult<WorktreeState> {
    let path = path.to_path_buf();
    if !path.exists() {
        let mut state = WorktreeState::default();
        let _ = ensure_main_entry(&mut state)
            .map_err(|source| WorktreeError::StateRepair { source })?;
        return Ok(state);
    }
    let data = fs::read(&path).map_err(|source| WorktreeError::StateRead {
        path: path.clone(),
        source,
    })?;
    if data.is_empty() {
        return Err(WorktreeError::StateCorrupt {
            path: path.clone(),
            source: "registry file exists but is EMPTY (torn write?); restore it from a \
                      backup, or delete it to let the next worktree command reinitialize \
                      a fresh registry"
                .to_string(),
        });
    }
    let (mut state, shape) =
        WorktreeState::parse_document(&data).map_err(|source| WorktreeError::StateCorrupt {
            path: path.clone(),
            source,
        })?;
    // A valid v2 registry already guarantees exactly one main entry — use it
    // as-is (a lockless reader must not even repair flags in memory, or the
    // synthesized main would carry a persisted linked id). Only the legacy
    // v1 shape needs the in-memory main-entry synthesis.
    match shape {
        RegistryShape::V2 => {
            if let Err(source) = state.validate_v2() {
                return Err(WorktreeError::StateCorrupt {
                    path: path.clone(),
                    source: format!("{source}; run `libra worktree repair --confirm` to heal it"),
                });
            }
        }
        RegistryShape::V1 => {
            let _ = ensure_main_entry(&mut state)
                .map_err(|source| WorktreeError::StateRepair { source })?;
        }
    }
    Ok(state)
}

/// Restore the v2 identity invariants after an in-memory repair mutated
/// `is_main` flags or upgraded a v1 file: main carries no id, every linked
/// entry gets its stable id backfilled from the gitdir (or the
/// canonical-path synthesis fallback, which always resolves).
pub(super) fn normalize_v2_ids(state: &mut WorktreeState) -> bool {
    let mut changed = false;
    for entry in &mut state.entries {
        if entry.is_main {
            if entry.worktree_id.is_some() {
                entry.worktree_id = None;
                changed = true;
            }
        } else if entry
            .worktree_id
            .as_deref()
            .is_none_or(|id| id.trim().is_empty())
        {
            entry.worktree_id = resolve_worktree_id(Path::new(&entry.path));
            changed = true;
        }
    }
    changed
}

/// Atomically writes the given `WorktreeState` to disk.
///
/// Uses a uniquely-named temporary file plus atomic replacement on every
/// platform (Windows replaces via `MoveFileExW`), so a concurrent reader
/// sees the old registry or the new one — never a missing or partial file.
pub(super) fn save_state(state: &WorktreeState) -> io::Result<()> {
    let path = state_path();
    let data = serde_json::to_vec_pretty(state).map_err(|e| io::Error::other(e.to_string()))?;
    // Unique-temp + atomic replacement on every platform (Windows uses
    // MoveFileExW replacement) — a concurrent reader sees the old registry
    // or the new one, never a missing or partial file. The old
    // remove-then-rename Windows path opened exactly that missing-file
    // window, which a lockless reader would misread as a fresh repository.
    crate::utils::atomic_write::write_atomic(
        &path,
        &data,
        crate::utils::atomic_write::sync_data_enabled(),
    )
}

pub(super) fn write_state(state: &WorktreeState) -> WorktreeResult<()> {
    let path = state_path();
    save_state(state).map_err(|source| WorktreeError::StateWrite { path, source })
}

/// Normalizes the given path into an absolute, canonical path where possible.
///
/// For non-existing paths, this resolves the deepest existing ancestor and
/// appends the remaining lexical components. This keeps persisted worktree
/// paths stable even when intermediate parents do not exist yet.
///
/// Delegates to [`util::canonicalize_deepest_existing`] — the registry and the
/// workspace/lease store (§C.8) MUST agree on what "the same directory" means,
/// so there is exactly one implementation.
pub(super) fn canonicalize<P: AsRef<Path>>(path: P) -> io::Result<PathBuf> {
    util::canonicalize_deepest_existing(path)
}

/// Ensure the registry designates EXACTLY ONE main entry. In the standard
/// layout the repository root (the directory holding `.libra`) is
/// authoritative: it is crowned when present and restored when absent —
/// other entries are never elected main. The valid-path/first-entry/cwd
/// heuristics apply only to non-standard layouts where the root cannot be
/// inferred.
pub(super) fn ensure_main_entry(state: &mut WorktreeState) -> io::Result<bool> {
    fn is_valid_worktree_path(path: &Path) -> bool {
        path.join(util::ROOT_DIR).exists()
    }

    fn apply_unique_main(state: &mut WorktreeState, idx: usize) -> bool {
        let mut changed = false;
        for (i, w) in state.entries.iter_mut().enumerate() {
            let should_be_main = i == idx;
            if w.is_main != should_be_main {
                w.is_main = should_be_main;
                changed = true;
            }
        }
        changed
    }

    // The repository's OWN root (the directory holding the `.libra` common
    // storage) is the AUTHORITATIVE main worktree whenever it can be
    // inferred. A stray `is_main` marker on a linked entry — or a mainless
    // legacy file whose only entries are linked worktrees — must never
    // crown a linked path as main: that would durably swap the
    // main-versus-linked scope mapping (§C.7).
    let storage = util::storage_path();
    let inferred_standard_main =
        if storage.file_name() == Some(std::ffi::OsStr::new(util::ROOT_DIR)) {
            let repo_root = storage
                .parent()
                .ok_or_else(|| io::Error::other("invalid storage path"))?;
            Some(canonicalize(repo_root)?)
        } else {
            None
        };

    if let Some(root) = inferred_standard_main.as_ref() {
        if let Some(idx) = state
            .entries
            .iter()
            .position(|w| Path::new(&w.path) == root)
        {
            return Ok(apply_unique_main(state, idx));
        }
        // The true main is absent from the registry: restore it instead of
        // electing one of the remaining (linked) entries.
        for w in &mut *state.entries {
            w.is_main = false;
        }
        state.entries.push(WorktreeEntry {
            path: root.to_string_lossy().to_string(),
            is_main: true,
            locked: false,
            lock_reason: None,
            worktree_id: None,
            state: WorktreeEntryState::Active,
            epoch: 0,
        });
        return Ok(true);
    }

    // Non-standard layout — the root cannot be inferred. Fall back to the
    // conservative heuristics: keep a valid marked main, else prefer any
    // real worktree path, else the first entry, else infer from cwd.
    if let Some(idx) =
        state.entries.iter().enumerate().find_map(|(i, w)| {
            (w.is_main && is_valid_worktree_path(Path::new(&w.path))).then_some(i)
        })
    {
        return Ok(apply_unique_main(state, idx));
    }
    if let Some(idx) = state
        .entries
        .iter()
        .position(|w| is_valid_worktree_path(Path::new(&w.path)))
        .or_else(|| (!state.entries.is_empty()).then_some(0))
    {
        return Ok(apply_unique_main(state, idx));
    }

    let inferred_main = canonicalize(util::working_dir())?;
    if let Some(idx) = state
        .entries
        .iter()
        .position(|w| Path::new(&w.path) == inferred_main)
    {
        Ok(apply_unique_main(state, idx))
    } else {
        for w in &mut *state.entries {
            w.is_main = false;
        }
        state.entries.push(WorktreeEntry {
            path: inferred_main.to_string_lossy().to_string(),
            is_main: true,
            locked: false,
            lock_reason: None,
            worktree_id: None,
            state: WorktreeEntryState::Active,
            epoch: 0,
        });
        Ok(true)
    }
}

/// Finds a mutable worktree entry by canonical path.
pub(super) fn find_entry_mut<'a>(
    state: &'a mut WorktreeState,
    path: &Path,
) -> Option<&'a mut WorktreeEntry> {
    state
        .entries
        .iter_mut()
        .find(|w| Path::new(&w.path) == path)
}

/// Finds an immutable worktree entry by canonical path.
pub(super) fn find_entry<'a>(state: &'a WorktreeState, path: &Path) -> Option<&'a WorktreeEntry> {
    state.entries.iter().find(|w| Path::new(&w.path) == path)
}

/// Resolve a (possibly already-deleted) worktree's stable instance id: read
/// its `.libra/worktree_id` file if present, else recompute deterministically
/// from the canonical path (lore.md 2.1).
pub(super) fn resolve_worktree_id(target: &Path) -> Option<String> {
    if let Ok(id) = fs::read_to_string(target.join(util::ROOT_DIR).join("worktree_id")) {
        let id = id.trim();
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    fs::canonicalize(target)
        .ok()
        .map(|c| util::worktree_instance_id(&c))
        .or_else(|| Some(util::worktree_instance_id(target)))
}
